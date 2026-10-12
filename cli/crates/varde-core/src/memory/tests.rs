use super::recall::indexed_rows;
use super::*;
use crate::cairn::Edit;
use crate::store::temp as store;

fn task_with_run(s: &Store, task: &str, title: &str, project: &str) {
    s.conn
        .execute(
            "INSERT INTO task (id, title, project, created) VALUES (?1, ?2, ?3, 1)",
            params![task, title, project],
        )
        .unwrap();
    s.conn
        .execute(
            "INSERT INTO run (id, task, agent, native_id, relation, started, transcript)
             VALUES (?1, ?2, 'claude', ?1, 'fresh', 1, '/t')",
            params![format!("R-{task}"), task],
        )
        .unwrap();
}

fn cairn_of(status: &str, finding: &str) -> Cairn {
    Cairn { status: status.into(), findings: vec![Item::new(finding)], ..Default::default() }
}

const ME: Asker = Asker { source: "user", run: None };

fn found(s: &Store, q: &str, project: Option<&str>, exclude: Option<&str>) -> Vec<String> {
    recall(s, q, project, exclude, 5, ME, 2).unwrap().hits.into_iter().map(|h| h.task).collect()
}

fn new(c: &Cairn) -> Outcome<'_> {
    Outcome::New { cairn: c, gone: &[], capped: &[], confirmed: &[], reason: None, took_notes: true }
}

#[test]
fn a_job_folds_notes_moves_the_cursor_and_writes_history() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "Fix login", "/p");
    add_note(&s, "R-T1", "the fixture is shared", 5).unwrap();
    let job = enqueue(&s, "R-T1", 100, 6).unwrap().unwrap();
    // The same fold is not queued twice.
    assert!(enqueue(&s, "R-T1", 100, 6).unwrap().is_none());

    let input = claim_job(&mut s, &job, 0).unwrap().unwrap();
    assert!(claim_job(&mut s, &job, 0).unwrap().is_none());
    assert_eq!(input.notes.len(), 1);
    assert_eq!((input.kind.as_str(), input.state.as_str()), ("fold", "open"));
    let c = cairn_of("Fixed", "Shared fixture caused the race");
    assert!(finish_job(&mut s, &job, &input, 100, new(&c), 7).unwrap());

    let (stored, version) = cairn(&s.conn, "T1").unwrap().unwrap();
    assert_eq!(version, 1);
    assert_eq!(stored.render(), c.render());
    assert!(stored.findings[0].id > 0);
    assert!(pending_notes(&s.conn, "T1").unwrap().is_empty());
    assert_eq!(history(&s.conn, "T1").unwrap().len(), 1);
    // Nothing past the cursor, nothing to do.
    assert!(enqueue(&s, "R-T1", 100, 8).unwrap().is_none());
}

#[test]
fn a_close_out_and_a_fold_never_both_read_the_same_stretch() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "t", "/p");
    add_note(&s, "R-T1", "keep", 1).unwrap();
    // A fold waiting when the task is closed is the close-out's to do.
    let fold = enqueue(&s, "R-T1", 100, 2).unwrap().unwrap();
    let close = enqueue_close(&s, "T1", &|_| Some(100), 3).unwrap().unwrap();
    let state = |id: &str| -> String {
        s.conn.query_row("SELECT state || ' ' || upto FROM job WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    };
    assert_eq!(state(&fold), "skipped 100");
    // The session's end comes after: the close-out reads that far too.
    assert!(enqueue(&s, "R-T1", 150, 4).unwrap().is_none());
    assert_eq!(state(&close), "queued 150");
}

#[test]
fn notes_are_folded_only_by_an_update_that_took_them_up() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    add_note(&s, "R-T1", "the fixture is shared", 1).unwrap();
    let job = enqueue(&s, "R-T1", 100, 2).unwrap().unwrap();
    let input = claim_job(&mut s, &job, 3).unwrap().unwrap();
    let c = Cairn { status: "Going".into(), ..Default::default() };
    let status_only =
        Outcome::New { cairn: &c, gone: &[], capped: &[], confirmed: &[], reason: None, took_notes: false };
    assert!(finish_job(&mut s, &job, &input, 100, status_only, 4).unwrap());
    assert_eq!(pending_notes(&s.conn, "T1").unwrap().len(), 1);
}

#[test]
fn edits_keep_ids_and_move_what_leaves_to_history() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Fix login", "/p");
    let first = Cairn {
        status: "Looking".into(),
        findings: vec![Item::new("The DB driver drops connections"), Item::new("`session_refresh` races")],
        open: vec![Item::new("Find the cause")],
        ..Default::default()
    };
    assert!(put_cairn(&s.conn, "T1", &first, 0, "job", None, 1).unwrap());
    let (before, v) = cairn(&s.conn, "T1").unwrap().unwrap();
    let [driver, race] = [before.findings[0].id, before.findings[1].id];
    let open = before.open[0].id;
    let a = before.apply(
        &[
            Edit::Drop { id: driver, reason: "wrong: it was the fixture".into() },
            Edit::Edit { id: race, text: "`session_refresh` races logout".into() },
            Edit::Drop { id: open, reason: "done".into() },
            Edit::Add { section: Section::Decisions, text: "Per-test fixtures".into() },
        ],
        5,
    );
    let w = Write {
        author: "job",
        run: Some("R-T1"),
        reason: None,
        gone: &a.gone,
        default_gone: "x",
        capped: &a.capped,
        now: 5,
    };
    assert!(save_doc(&s.conn, Owner::Task("T1"), v, &a.cairn, &w).unwrap());
    let (after, v2) = cairn(&s.conn, "T1").unwrap().unwrap();
    assert_eq!(v2, 2);
    assert_eq!(after.findings[0].id, race);
    assert_eq!(after.findings[0].text, "`session_refresh` races logout");
    assert_eq!(after.decisions[0].text, "Per-test fixtures");
    let former = former(&s.conn, "T1").unwrap();
    let reasons: Vec<(&str, Option<&str>)> =
        former.iter().map(|f| (f.text.as_str(), f.reason.as_deref())).collect();
    assert!(reasons.contains(&("The DB driver drops connections", Some("wrong: it was the fixture"))));
    assert!(reasons.contains(&("Find the cause", Some("done"))));
    assert!(reasons.contains(&("`session_refresh` races", Some("edited"))));
    // A stale process holding the old version writes nothing.
    assert!(!save_doc(&s.conn, Owner::Task("T1"), v, &a.cairn, &w).unwrap());
}

#[test]
fn a_resume_folds_on_from_where_its_conversation_got() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    // A resume started while the previous run's fold was still going.
    s.conn
        .execute(
            "INSERT INTO run (id, task, agent, native_id, parent_run, relation, started, transcript)
             VALUES ('R2', 'T1', 'claude', 'R-T1', 'R-T1', 'resume', 2, '/t')",
            [],
        )
        .unwrap();
    let job = enqueue(&s, "R-T1", 100, 3).unwrap().unwrap();
    let input = claim_job(&mut s, &job, 0).unwrap().unwrap();
    assert!(finish_job(&mut s, &job, &input, 100, Outcome::Unchanged { confirmed: &[] }, 4).unwrap());
    assert!(enqueue(&s, "R2", 100, 5).unwrap().is_none());
    let job = enqueue(&s, "R2", 150, 5).unwrap().unwrap();
    assert_eq!(claim_job(&mut s, &job, 0).unwrap().unwrap().cursor, 100);
}

#[test]
fn a_stale_process_cannot_fail_a_finished_job() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    let input = claim_job(&mut s, &job, 1).unwrap().unwrap();
    assert!(finish_job(&mut s, &job, &input, 100, Outcome::Unchanged { confirmed: &[] }, 2).unwrap());
    fail_job(&s.conn, &job, "the model did not answer", 3).unwrap();
    let state: String = s.conn.query_row("SELECT state FROM job", [], |r| r.get(0)).unwrap();
    assert_eq!(state, "unchanged");
}

#[test]
fn a_failed_or_dead_fold_is_tried_again() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    claim_job(&mut s, &job, 0).unwrap();
    fail_job(&s.conn, &job, "timeout", 2).unwrap();
    assert_eq!(enqueue(&s, "R-T1", 100, 3).unwrap(), Some(job.clone()));
    // Queued again, and a process that died holding it is noticed later.
    assert!(enqueue(&s, "R-T1", 100, 4).unwrap().is_none());
    assert_eq!(enqueue(&s, "R-T1", 100, 4 + STUCK_AFTER_MS).unwrap(), Some(job));
}

#[test]
fn a_failed_fold_is_retried_a_few_times_after_its_run_ended() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    let mut now = 1;
    for attempt in 1..=MAX_ATTEMPTS {
        assert!(claim_job(&mut s, &job, now).unwrap().is_some(), "attempt {attempt}");
        fail_job(&s.conn, &job, "the model call failed", now).unwrap();
        // Not straight away: the model may be having a bad minute.
        assert!(requeue_stuck(&s.conn, now + 1).unwrap().is_empty());
        now += RETRY_AFTER_MS + 1;
        let again = requeue_stuck(&s.conn, now).unwrap();
        assert_eq!(again.len(), usize::from(attempt < MAX_ATTEMPTS), "attempt {attempt}");
    }
    let state: String = s.conn.query_row("SELECT state FROM job", [], |r| r.get(0)).unwrap();
    assert_eq!(state, "failed");
}

#[test]
fn a_job_waiting_on_its_model_is_not_taken_for_dead() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    claim_job(&mut s, &job, 1).unwrap();
    // Four slow model calls take longer than the window; each beats.
    for call in 1..=4 {
        beat(&s.conn, &job, 1 + call * 180_000).unwrap();
        assert!(requeue_stuck(&s.conn, 1 + call * 180_000 + 170_000).unwrap().is_empty());
    }
    assert!(enqueue(&s, "R-T1", 100, 1 + 4 * 180_000 + 170_000).unwrap().is_none());
}

#[test]
fn a_job_that_died_is_queued_again_once() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    claim_job(&mut s, &job, 1).unwrap();
    assert!(requeue_stuck(&s.conn, 1 + STUCK_AFTER_MS).unwrap().is_empty());
    let later = 2 + STUCK_AFTER_MS;
    assert_eq!(requeue_stuck(&s.conn, later).unwrap(), vec![job.clone()]);
    // Its clock starts again, so it is not started twice.
    assert!(requeue_stuck(&s.conn, later + 1).unwrap().is_empty());
    assert!(claim_job(&mut s, &job, 0).unwrap().is_some());
}

#[test]
fn notes_wait_until_a_cairn_holds_them() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    add_note(&s, "R-T1", "keep this", 1).unwrap();
    let job = enqueue(&s, "R-T1", 100, 2).unwrap().unwrap();
    let input = claim_job(&mut s, &job, 0).unwrap().unwrap();
    assert!(finish_job(&mut s, &job, &input, 100, Outcome::Unchanged { confirmed: &[] }, 3).unwrap());
    assert_eq!(pending_notes(&s.conn, "T1").unwrap().len(), 1);
}

#[test]
fn a_job_that_lost_a_race_writes_nothing() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "Fix login", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    let input = claim_job(&mut s, &job, 0).unwrap().unwrap();
    // Someone edits the cairn while the job waits on the model.
    assert!(put_cairn(&s.conn, "T1", &cairn_of("edited", "x"), 0, "user", None, 2).unwrap());
    let c = cairn_of("from job", "y");
    assert!(!finish_job(&mut s, &job, &input, 100, new(&c), 3).unwrap());
    assert_eq!(cairn(&s.conn, "T1").unwrap().unwrap().0.status, "edited");
}

#[test]
fn a_close_out_is_queued_once_per_closing_and_only_with_memory() {
    let (_d, mut s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let size = |_: &str| Some(100);
    let none = |_: &str| None;
    // A task with no memory and nothing new in its transcript.
    assert!(enqueue_close(&s, "T1", &none, 1).unwrap().is_none());
    // Something new in it.
    let job = enqueue_close(&s, "T1", &size, 1).unwrap().unwrap();
    assert!(enqueue_close(&s, "T1", &size, 2).unwrap().is_none());
    let input = claim_job(&mut s, &job, 3).unwrap().unwrap();
    assert_eq!(input.kind, "close");
    assert!(finish_job(&mut s, &job, &input, 100, new(&cairn_of("Done", "x")), 4).unwrap());
    // Closed again later: the same stretch, a new close-out.
    assert_eq!(enqueue_close(&s, "T1", &none, 5).unwrap(), Some(job));
    // A fold of that stretch is not.
    assert!(enqueue(&s, "R-T1", 100, 6).unwrap().is_none());
}

#[test]
fn tokens_add_up_over_a_jobs_calls() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "t", "/p");
    let job = enqueue(&s, "R-T1", 100, 1).unwrap().unwrap();
    record_call(&s.conn, &job, Some(1000), Some(40)).unwrap();
    record_call(&s.conn, &job, Some(1100), Some(30)).unwrap();
    record_call(&s.conn, &job, None, None).unwrap();
    let row: (i64, i64, i64) = s
        .conn
        .query_row("SELECT calls, input_tokens, output_tokens FROM job", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(row, (3, 2100, 70));
}

#[test]
fn folds_come_at_run_end_compaction_or_after_a_while() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "t", "/p");
    assert!(fold_due(&s.conn, "R-T1", "ended", 2).unwrap());
    assert!(fold_due(&s.conn, "R-T1", "compacting", 2).unwrap());
    assert!(!fold_due(&s.conn, "R-T1", "turn_end", 2).unwrap());
    assert!(fold_due(&s.conn, "R-T1", "turn_end", 1 + FOLD_EVERY_MS).unwrap());
    assert!(fold_due(&s.conn, "R-T1", "delegated", 1 + FOLD_EVERY_MS).unwrap());
    assert!(!fold_due(&s.conn, "R-T1", "prompt", 1 + FOLD_EVERY_MS).unwrap());
}

#[test]
fn recall_finds_a_task_by_its_id() {
    let (_d, s) = store();
    for (id, project) in [("01AAAAAAAAAAAAAAAAAAZEF238", "/p"), ("01BBBBBBBBBBBBBBBBBBC925DR", "/q")] {
        task_with_run(&s, id, "a task", project);
        put_cairn(&s.conn, id, &cairn_of("Done", "Something about caching"), 0, "job", None, 1).unwrap();
    }
    // The short id, from another project, and its start.
    let found = |q: &str| -> Vec<String> {
        recall(&s, q, Some("/p"), None, 5, ME, 2).unwrap().hits.into_iter().map(|h| h.short).collect()
    };
    assert_eq!(found("c925dr"), ["c925dr"]);
    assert_eq!(found("what did c925 find"), ["c925dr"]);
    assert_eq!(found("01BBBBBBBBBBBBBBBBBBC925DR"), ["c925dr"]);
    // First, then what the words find.
    assert_eq!(found("zef238 caching"), ["zef238"]);
    assert_eq!(recall(&s, "c925dr caching", None, None, 5, ME, 2).unwrap().hits.len(), 2);
    assert_eq!(recall(&s, "c925dr zef238", None, None, 1, ME, 2).unwrap().hits.len(), 1);
    // Not the task asking.
    let none = recall(&s, "c925dr", None, Some("01BBBBBBBBBBBBBBBBBBC925DR"), 5, ME, 2).unwrap();
    assert!(none.hits.is_empty());
}

#[test]
fn recall_finds_a_literal_identifier() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Staging login", "/p");
    let c = cairn_of("Done", "OAUTH_CLIENT_ID_STAGING was unset in CI");
    put_cairn(&s.conn, "T1", &c, 0, "job", None, 1).unwrap();
    assert_eq!(found(&s, "OAUTH_CLIENT_ID_STAGING", None, None), ["T1"]);
    assert_eq!(found(&s, "oauth_client", None, None), ["T1"]);
    // As the phrase: the words apart, in another order, do not make it.
    task_with_run(&s, "T2", "Other", "/p");
    put_cairn(&s.conn, "T2", &cairn_of("Done", "the staging id of the client"), 0, "job", None, 1).unwrap();
    assert_eq!(found(&s, "OAUTH_CLIENT_ID_STAGING", None, None), ["T1"]);
}

#[test]
fn recall_finds_a_task_by_its_new_title() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Old name", "/p");
    put_cairn(&s.conn, "T1", &cairn_of("Done", "x"), 0, "job", None, 1).unwrap();
    crate::task::set(&s, "T1", Some("Renamed in org"), None).unwrap();
    assert_eq!(found(&s, "renamed", None, None), ["T1"]);
    assert!(found(&s, "old", None, None).is_empty());
}

#[test]
fn recall_finds_by_content_within_a_project_else_everywhere() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Fix login", "/p");
    task_with_run(&s, "T2", "Speed up the board", "/p");
    task_with_run(&s, "T3", "Other project login", "/q");
    let race = cairn_of("Done", "Race in session_refresh at session.rs:88");
    put_cairn(&s.conn, "T1", &race, 0, "job", None, 1).unwrap();
    put_cairn(&s.conn, "T2", &cairn_of("Done", "Rendering spawned git per row"), 0, "job", None, 1).unwrap();
    put_cairn(&s.conn, "T3", &cairn_of("Done", "Race in login too"), 0, "job", None, 1).unwrap();

    assert_eq!(found(&s, "session race", Some("/p"), None), ["T1"]);
    // Prefixes match: "sess" finds "session_refresh"; stems too.
    assert_eq!(found(&s, "sess", Some("/p"), None), ["T1"]);
    assert_eq!(found(&s, "races", Some("/p"), None), ["T1"]);
    assert_eq!(found(&s, "rendered rows", Some("/p"), None), ["T2"]);
    assert_eq!(found(&s, "race", None, None).len(), 2);
    assert_eq!(found(&s, "race", None, Some("T1")), ["T3"]);
    // Stopwords alone find nothing; they once matched every cairn.
    assert!(found(&s, "the of in at", None, None).is_empty());
    // Nothing in this project: the best of every project, said so.
    let f = recall(&s, "login race", Some("/q2"), None, 5, ME, 2).unwrap();
    assert!(f.everywhere);
    assert_eq!(f.hits.len(), 2);
    // FTS syntax in a query is just text.
    assert!(recall(&s, "\"unbalanced AND (", None, None, 5, ME, 2).is_ok());
    assert!(recall(&s, "NEAR( x* ^y", None, None, 5, ME, 2).is_ok());
}

#[test]
fn a_hit_is_the_bullet_or_note_that_matched() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Board picks up sessions", "/p");
    let c = Cairn {
        status: "Complete. Feature merged.".into(),
        findings: vec![Item::new("Ad-hoc sessions become tasks on their first prompt")],
        ..Default::default()
    };
    put_cairn(&s.conn, "T1", &c, 0, "job", None, 1).unwrap();
    add_note(&s, "R-T1", "The hook walks up to the agent process by name", 2).unwrap();
    let hit = |q: &str| recall(&s, q, None, None, 5, ME, 3).unwrap().hits.remove(0);
    let h = hit("ad-hoc sessions");
    assert_eq!((h.kind.as_str(), h.snippet.as_str()), ("findings", c.findings[0].text.as_str()));
    assert!(h.score.unwrap() > 0.0);
    let h = hit("agent process");
    assert_eq!(h.kind, "notes");
    assert_eq!(h.snippet, "The hook walks up to the agent process by name");
}

#[test]
fn history_ranks_below_what_is_current() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "a", "/p");
    task_with_run(&s, "T2", "b", "/p");
    put_cairn(&s.conn, "T1", &cairn_of("s", "The gateway token expires hourly"), 0, "job", None, 1).unwrap();
    put_cairn(&s.conn, "T2", &cairn_of("s", "The gateway token expires hourly"), 0, "job", None, 1).unwrap();
    // T1's moves to history.
    put_cairn(&s.conn, "T1", &cairn_of("s", "Something else"), 1, "job", None, 2).unwrap();
    let f = recall(&s, "gateway token", None, None, 5, ME, 3).unwrap();
    let kinds: Vec<(&str, &str)> = f.hits.iter().map(|h| (h.task.as_str(), h.kind.as_str())).collect();
    assert_eq!(kinds, [("T2", "findings"), ("T1", "history")]);
}

#[test]
fn related_work_needs_more_than_a_shared_word() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Stuck cairn updates", "/p");
    task_with_run(&s, "T2", "Tmux proof of concept", "/p");
    let c = Cairn {
        status: "Done".into(),
        findings: vec![
            Item::new("A double-restart loop requeued the job forever"),
            Item::new("Jobs beat before each model call; a stuck one is requeued after 10 minutes"),
        ],
        ..Default::default()
    };
    put_cairn(&s.conn, "T1", &c, 0, "job", None, 1).unwrap();
    let tmux = cairn_of("Done", "A private tmux server (-L) keeps tests off the user's tmux sessions");
    put_cairn(&s.conn, "T2", &tmux, 0, "job", None, 1).unwrap();
    let asker = Asker { source: "preamble", run: Some("R9") };
    let r = |q: &str| related(&s.conn, q, Some("/p"), None, 3, asker, 2).unwrap().hits;
    assert!(r("Consider loop functionality").is_empty());
    let hits = r("Cairn update job stuck and requeued");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "findings");
    assert!(r("Testing first tmux run in private tmux server").iter().any(|h| h.task == "T2"));
    let logged: (String, String, String) = s
        .conn
        .query_row("SELECT source, run, results FROM recall_log WHERE query LIKE 'Cairn update%'", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((logged.0.as_str(), logged.1.as_str()), ("preamble", "R9"));
    assert!(logged.2.contains("\"task\":\"T1\""), "{}", logged.2);
}

#[test]
fn queries_drop_stopwords_and_keep_identifiers_whole() {
    assert_eq!(fts_query("Prepare for the release").as_deref(), Some("\"prepare\"* OR \"release\"*"));
    assert_eq!(fts_query("OAUTH_CLIENT_ID in CI").as_deref(), Some("\"oauth client id\" OR \"ci\""));
    assert_eq!(
        fts_query("memory::recall src/run.rs").as_deref(),
        Some("\"memory recall\"* OR \"src run rs\"")
    );
    assert_eq!(fts_query("why doesn't it work").as_deref(), Some("\"work\"*"));
    assert_eq!(fts_query("of to a"), None);
}

#[test]
fn anchors_are_checked_per_cairn_and_least_recently_first() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "a", "/p");
    task_with_run(&s, "T2", "b", "/p");
    let c = Cairn {
        status: "s".into(),
        findings: vec![Item::new("`fold_due` decides"), Item::new("`UNFILED_TTL_MS` ages tasks out")],
        ..Default::default()
    };
    put_cairn(&s.conn, "T1", &c, 0, "job", None, 1).unwrap();
    put_cairn(&s.conn, "T2", &cairn_of("s", "`other_fn` too"), 0, "job", None, 1).unwrap();
    let a = anchored(&s.conn, Owner::Task("T1")).unwrap();
    assert_eq!(a.len(), 2);
    assert!(a.iter().all(|x| x.dir == "/p"));
    assert_eq!(anchored_due(&s.conn, 10, 50).unwrap().len(), 3);
    let gone = a.iter().find(|x| x.anchor == "UNFILED_TTL_MS").unwrap().id;
    let here = a.iter().find(|x| x.anchor == "fold_due").unwrap().id;
    set_stale(&s.conn, &[(gone, true), (here, false)], 5).unwrap();
    assert_eq!(stale_count(&s.conn).unwrap(), (1, 1));
    // Never checked comes first; then the least recently; at most `limit`.
    let due = anchored_due(&s.conn, 10, 2).unwrap();
    assert_eq!(due[0].anchor, "other_fn");
    assert_eq!(due.len(), 2);
    assert_eq!(anchored_due(&s.conn, 5, 50).unwrap().len(), 1);
    let (c, _) = cairn(&s.conn, "T1").unwrap().unwrap();
    assert!(c.display().contains("ages tasks out (stale?)"));
    assert!(c.for_update().contains("is no longer in the code"));
}

fn user_write(now: i64) -> Write<'static> {
    Write { author: "user", run: None, reason: None, gone: &[], default_gone: "x", capped: &[], now }
}

#[test]
fn a_project_cairn_is_kept_searched_and_checked_like_a_cairn() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "Fix login", "/p");
    put_cairn(&s.conn, "T1", &cairn_of("Done", "`session_refresh` races logout"), 0, "job", None, 1).unwrap();
    let p = Cairn {
        gotchas: vec![Item::new("`launch.rs` tests fail on macOS before any change")],
        howto: vec![Item::new("Reinstall with `cargo install --path cli/crates/varde`")],
        ..Cairn::project()
    };
    assert!(save_doc(&s.conn, Owner::Project("/p"), 0, &p, &user_write(2)).unwrap());
    // A stale writer loses, as with a cairn.
    assert!(!save_doc(&s.conn, Owner::Project("/p"), 0, &p, &user_write(3)).unwrap());
    let (stored, v) = project_cairn(&s.conn, "/p").unwrap().unwrap();
    assert_eq!((stored.shape, v, stored.gotchas.len()), (Shape::Project, 1, 1));
    assert!(cairn(&s.conn, "T1").unwrap().unwrap().0.gotchas.is_empty());
    assert_eq!(history_of(&s.conn, Owner::Project("/p")).unwrap().len(), 1);

    // Recall finds it, in its project, without a task.
    let f = recall(&s, "reinstall", Some("/p"), None, 5, ME, 4).unwrap();
    assert_eq!((f.hits[0].task.as_str(), f.hits[0].kind.as_str()), ("", "howto"));
    assert_eq!(f.hits[0].project.as_deref(), Some("/p"));
    assert!(recall(&s, "reinstall", Some("/q"), None, 5, ME, 4).unwrap().everywhere);
    // Excluding a task leaves it, and a recall of both finds both.
    assert_eq!(recall(&s, "macos session_refresh", None, Some("T1"), 5, ME, 4).unwrap().hits.len(), 1);
    assert_eq!(recall(&s, "macos session_refresh", None, None, 5, ME, 4).unwrap().hits.len(), 2);
    // It is never offered as related work: it is shown anyway.
    assert!(related(&s.conn, "launch.rs macOS failures", Some("/p"), None, 3, ME, 4)
        .unwrap()
        .hits
        .is_empty());

    // Its anchors are checked in the project.
    let a = anchored(&s.conn, Owner::Project("/p")).unwrap();
    assert_eq!(a.len(), 2);
    assert!(a.iter().any(|x| x.anchor == "cli/crates/varde" && x.dir == "/p"));
    assert_eq!(anchored_due(&s.conn, 10, 50).unwrap().len(), 3);
    let gone = a.iter().find(|x| x.anchor == "launch.rs").unwrap().id;
    set_stale(&s.conn, &[(gone, true)], 5).unwrap();
    assert!(project_cairn(&s.conn, "/p").unwrap().unwrap().0.display().contains("change (stale?)"));
}

#[test]
fn a_conversation_remembers_what_it_was_shown_through_resumes_and_forks() {
    let (_d, s) = store();
    task_with_run(&s, "T1", "t", "/p");
    s.conn
        .execute_batch(
            "INSERT INTO run (id, task, agent, native_id, parent_run, relation, started, holdout)
               VALUES ('R2', 'T1', 'claude', 'R-T1', 'R-T1', 'resume', 2, NULL),
                      ('R3', 'T1', 'claude', 'f', 'R2', 'fork', 3, NULL),
                      ('R4', 'T1', 'claude', 'g', 'R3', 'fresh', 4, NULL);",
        )
        .unwrap();
    let shown = |v| Shown { kind: "cairn", task: Some("T1".into()), version: Some(v), items: vec![] };
    record_shown(&s.conn, "R-T1", "hook", &[shown(2)], 1).unwrap();
    let project = Shown { kind: "project", task: None, version: Some(5), items: vec![] };
    record_shown(&s.conn, "R2", "hook", &[project], 2).unwrap();
    assert_eq!(
        conversation_saw(&s.conn, "R3", "T1").unwrap(),
        Saw { cairn: Some(2), project: Some(5), held: false }
    );
    // A fresh conversation (a `/clear`) starts from nothing.
    assert_eq!(conversation_saw(&s.conn, "R4", "T1").unwrap(), Saw::default());
    s.conn.execute("UPDATE run SET holdout = 1 WHERE id = 'R-T1'", []).unwrap();
    assert!(conversation_saw(&s.conn, "R3", "T1").unwrap().held);
}

#[test]
fn the_index_knows_how_many_rows_it_has() {
    let (_d, s) = store();
    for n in 0..30 {
        let t = format!("T{n}");
        task_with_run(&s, &t, "a task", "/p");
        put_cairn(&s.conn, &t, &cairn_of("s", &format!("finding {n}")), 0, "job", None, 1).unwrap();
    }
    let p = Cairn { gotchas: vec![Item::new("a gotcha")], ..Cairn::project() };
    assert!(save_doc(&s.conn, Owner::Project("/p"), 0, &p, &user_write(2)).unwrap());
    let counted: i64 = s.conn.query_row("SELECT count(*) FROM memory_fts", [], |r| r.get(0)).unwrap();
    assert_eq!(indexed_rows(&s.conn).unwrap(), counted);
    reindex(&s.conn, "T3").unwrap();
    reindex_project(&s.conn, "/p").unwrap();
    assert_eq!(indexed_rows(&s.conn).unwrap(), counted);
    // Each finds its rows by an index, not by scanning every row.
    for filter in ["c7 = 'T3'", "c7 IS NULL AND c10 = '/p'"] {
        let plan: String = s
            .conn
            .query_row(
                &format!("EXPLAIN QUERY PLAN SELECT id FROM memory_fts_content WHERE {filter}"),
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("INDEX memory_fts_"), "{plan}");
    }
}

#[test]
fn holdout_is_decided_by_the_run_id_alone() {
    let ids: Vec<String> = (0..2000).map(|_| ulid::Ulid::new().to_string()).collect();
    assert!(ids.iter().all(|r| !held_out(r, 0.0) && held_out(r, 1.0)));
    let share = ids.iter().filter(|r| held_out(r, 0.2)).count() as f64 / ids.len() as f64;
    assert!((0.15..0.25).contains(&share), "{share}");
    assert_eq!(held_out(&ids[0], 0.5), held_out(&ids[0], 0.5));
}

#[test]
fn what_a_run_was_shown_is_recorded() {
    let (_d, s) = store();
    let shown = [
        Shown {
            kind: "cairn",
            task: Some("T1".into()),
            version: Some(3),
            items: vec!["4".into(), "5".into()],
        },
        Shown { kind: "related", task: Some("T2".into()), version: None, items: vec!["9".into()] },
    ];
    record_shown(&s.conn, "R1", "preamble", &shown, 7).unwrap();
    let rows: Vec<(String, Option<i64>, String)> = s
        .conn
        .prepare("SELECT kind, version, items FROM shown WHERE run = 'R1' AND via = 'preamble' ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows[0], ("cairn".into(), Some(3), "[\"4\",\"5\"]".into()));
    assert_eq!(rows[1].0, "related");
}
