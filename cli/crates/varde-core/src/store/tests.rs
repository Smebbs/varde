use super::temp as store;
use super::*;

fn ev(id: &str, kind: Kind) -> Norm {
    Norm { native_id: id.into(), kind, cwd: Some("/tmp/proj".into()), transcript: None, prompt: None }
}

fn prompt(id: &str, text: &str) -> Norm {
    Norm { prompt: Some(text.into()), ..ev(id, Kind::Prompt) }
}

fn count(s: &Store, sql: &str) -> i64 {
    s.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn latest_run(s: &Store, native: &str) -> String {
    s.conn
        .query_row(
            "SELECT id FROM run WHERE native_id=?1 ORDER BY started DESC, rowid DESC LIMIT 1",
            [native],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn a_warm_worker_never_becomes_a_task() {
    let (_d, mut s) = store();
    s.ingest("claude", &ev("spare", Kind::Started(Source::Startup)), "{}", 1, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 0);
    // …and is swept once it is old.
    s.ingest("claude", &ev("next", Kind::Started(Source::Startup)), "{}", 1 + SPARE_TTL_MS + 1, None)
        .unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE native_id='spare'"), 0);
}

#[test]
fn the_first_prompt_makes_the_task() {
    let (_d, mut s) = store();
    s.ingest("claude", &ev("a", Kind::Started(Source::Startup)), "{}", 1, None).unwrap();
    s.ingest("claude", &prompt("a", "Fix the flaky login test please"), "{}", 2, None).unwrap();
    s.ingest("claude", &prompt("a", "and another thing"), "{}", 3, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 1);
    let title: String = s.conn.query_row("SELECT title FROM task", [], |r| r.get(0)).unwrap();
    assert_eq!(title, "Fix the flaky login test please");
}

#[test]
fn a_resume_is_a_new_run_of_the_same_task() {
    let (_d, mut s) = store();
    s.ingest("claude", &ev("a", Kind::Started(Source::Startup)), "{}", 1, None).unwrap();
    s.ingest("claude", &prompt("a", "work"), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Ended), "{}", 3, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Started(Source::Resume)), "{}", 4, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM run"), 2);
    assert_eq!(count(&s, "SELECT count(DISTINCT task) FROM run"), 1);
    assert_eq!(run_status(&s.conn, &latest_run(&s, "a")).unwrap(), Some(Status::Waiting));
}

#[test]
fn resuming_a_closed_tasks_conversation_reopens_it() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Ended), "{}", 3, None).unwrap();
    s.conn.execute("UPDATE task SET state = 'done', closed = 3", []).unwrap();
    s.ingest("claude", &ev("a", Kind::Started(Source::Resume)), "{}", 4, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task WHERE state = 'open' AND closed IS NULL"), 1);
}

#[test]
fn a_turn_end_records_what_it_left_running_until_work_resumes() {
    let (_d, mut s) = store();
    let end = |agents, shells| ev("a", Kind::TurnEnd { agents, shells });
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    s.ingest("claude", &end(2, 1), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Idle), "{}", 3, None).unwrap();
    let run = latest_run(&s, "a");
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Delegated));
    assert_eq!(
        {
            let v = view(&s.conn, &run).unwrap();
            (v.agents, v.shells)
        },
        (2, 1)
    );
    // A sub-agent reports back: the session is at work, the counts stale.
    s.ingest("claude", &prompt("a", "<task-notification>"), "{}", 4, None).unwrap();
    assert_eq!(
        {
            let v = view(&s.conn, &run).unwrap();
            (v.agents, v.shells)
        },
        (0, 0)
    );
    // A server left running holds nothing up, but is still counted.
    s.ingest("claude", &end(0, 1), "{}", 5, None).unwrap();
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Waiting));
    assert_eq!(
        {
            let v = view(&s.conn, &run).unwrap();
            (v.agents, v.shells)
        },
        (0, 1)
    );
}

#[test]
fn a_fork_is_its_own_run_without_a_task_yet() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("b", Kind::Started(Source::Fork)), "{}", 2, None).unwrap();
    let relation: String =
        s.conn.query_row("SELECT relation FROM run WHERE native_id='b'", [], |r| r.get(0)).unwrap();
    assert_eq!(relation, "fork");
    s.ingest("claude", &prompt("b", "fork work"), "{}", 3, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 2);
}

#[test]
fn compaction_continues_the_run() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Started(Source::Compact)), "{}", 2, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM run"), 1);
    assert_eq!(run_status(&s.conn, &latest_run(&s, "a")).unwrap(), Some(Status::Working));
}

#[test]
fn a_prenamed_run_is_claimed_by_its_start() {
    let (_d, mut s) = store();
    // `varde start` will insert the run before launching; simulate it.
    s.conn
        .execute(
            "INSERT INTO run (id, agent, native_id, relation, started)
             VALUES ('r1','claude','a','fresh',1)",
            [],
        )
        .unwrap();
    s.ingest("claude", &ev("a", Kind::Started(Source::Startup)), "{}", 2, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM run"), 1);
}

#[test]
fn only_the_tool_run_after_a_dialog_is_kept() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 3, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE kind='unblocked'"), 0);
    s.ingest("claude", &ev("a", Kind::Blocked), "{}", 4, None).unwrap();
    assert_eq!(run_status(&s.conn, &latest_run(&s, "a")).unwrap(), Some(Status::Blocked));
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 5, None).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 6, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE kind='unblocked'"), 1);
    assert_eq!(run_status(&s.conn, &latest_run(&s, "a")).unwrap(), Some(Status::Working));
}

#[test]
fn a_tool_run_after_a_turn_end_is_the_next_turn_starting() {
    // `/goal` starts the next turn itself, with no prompt.
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "/goal tests pass"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("a", Kind::TurnEnd { agents: 0, shells: 0 }), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 3, None).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 4, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE kind='unblocked'"), 1);
    assert_eq!(run_status(&s.conn, &latest_run(&s, "a")).unwrap(), Some(Status::Working));
    // An interrupted tool may still report; you are at the prompt.
    s.record_interrupt(&latest_run(&s, "a"), 5).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 6, None).unwrap();
    assert_eq!(run_status(&s.conn, &latest_run(&s, "a")).unwrap(), Some(Status::Waiting));
}

#[test]
fn an_event_replayed_late_counts_where_it_happened() {
    // A tool run and a prompt waited in the pending file while the
    // turn's end was recorded.
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "/goal tests pass"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Blocked), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::TurnEnd { agents: 0, shells: 0 }), "{}", 5, None).unwrap();
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 3, None).unwrap();
    s.ingest("claude", &prompt("a", "more"), "{}", 4, None).unwrap();
    let run = latest_run(&s, "a");
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Waiting));
    // The tool run did answer the dialog, before the turn ended.
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE kind='unblocked'"), 1);
    // One that comes late after a turn end it followed still counts.
    s.ingest("claude", &ev("a", Kind::ToolRan), "{}", 6, None).unwrap();
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Working));
}

#[test]
fn a_dialog_answered_between_turns_leaves_the_session_waiting() {
    // pi says when a dialog closes; one an extension opens while the
    // session is idle is not a turn.
    let (_d, mut s) = store();
    let pi = |s: &mut Store, kind: Kind, ts: i64| s.ingest("pi", &ev("a", kind), "{}", ts, None).unwrap();
    s.ingest("pi", &prompt("a", "work"), "{}", 1, None).unwrap();
    pi(&mut s, Kind::Blocked, 2);
    pi(&mut s, Kind::Answered, 3);
    let run = latest_run(&s, "a");
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Working));
    pi(&mut s, Kind::TurnEnd { agents: 0, shells: 0 }, 4);
    pi(&mut s, Kind::Blocked, 5);
    pi(&mut s, Kind::Answered, 6);
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Waiting));
    // A close with no dialog open says nothing.
    assert!(pi(&mut s, Kind::Answered, 7).is_none());
    // Two dialogs open at once: the turn goes on once both are closed.
    s.ingest("pi", &prompt("a", "more"), "{}", 8, None).unwrap();
    pi(&mut s, Kind::Blocked, 9);
    pi(&mut s, Kind::Blocked, 10);
    pi(&mut s, Kind::Answered, 11);
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Blocked));
    pi(&mut s, Kind::Answered, 12);
    assert_eq!(run_status(&s.conn, &run).unwrap(), Some(Status::Working));
    assert!(pi(&mut s, Kind::Answered, 13).is_none());
}

#[test]
fn one_tidy_deletes_one_bounded_batch() {
    // A first pass over a large store must not hold the write lock
    // for long: hooks wait at most 100 ms for it.
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    let run = latest_run(&s, "a");
    s.conn
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2)
             INSERT INTO event (run, kind, ts, raw) SELECT ?1, 'blocked', i + 1, '{}' FROM n",
            params![run, TIDY_BATCH as i64 + 10],
        )
        .unwrap();
    let later = 1_000_000 + KEEP_EVENTS_MS;
    assert_eq!(tidy(&s.conn, later).unwrap(), TIDY_BATCH);
    assert_eq!(tidy(&s.conn, later).unwrap(), 10);
    assert_eq!(tidy(&s.conn, later).unwrap(), 0);
}

#[test]
fn tidying_ages_out_jobs_given_up_on_but_not_ones_to_retry() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    let run = latest_run(&s, "a");
    s.conn
        .execute(
            "INSERT INTO job (id, run, upto, state, created, finished, attempts)
             VALUES ('given-up', ?1, 1, 'failed', 1, 2, 3), ('to-retry', ?1, 2, 'failed', 1, 2, 1)",
            [&run],
        )
        .unwrap();
    tidy(&s.conn, 3 + KEEP_EVENTS_MS).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM job WHERE id = 'to-retry'"), 1);
    assert_eq!(count(&s, "SELECT count(*) FROM job"), 1);
}

#[test]
fn tidying_ages_out_what_runs_were_shown_and_the_recall_log() {
    let (_d, s) = store();
    s.conn
        .execute_batch(
            "INSERT INTO shown (run, ts, via, kind) VALUES ('old', 1, 'hook', 'cairn'), ('new', 5, 'hook', 'cairn');
             INSERT INTO recall_log (ts, query, hits, source) VALUES (1, 'an old first prompt', 0, 'hook'), (5, 'new', 0, 'agent');",
        )
        .unwrap();
    tidy(&s.conn, 2 + KEEP_LOG_MS).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM shown WHERE run = 'new'"), 1);
    assert_eq!(count(&s, "SELECT count(*) FROM shown"), 1);
    assert_eq!(count(&s, "SELECT count(*) FROM recall_log WHERE query = 'new'"), 1);
    assert_eq!(count(&s, "SELECT count(*) FROM recall_log"), 1);
}

#[test]
fn tidying_ages_out_old_events_but_never_a_status() {
    let (_d, mut s) = store();
    let old = 1000;
    s.ingest("claude", &prompt("a", "work"), "{}", old, None).unwrap();
    for i in 1..20 {
        s.ingest("claude", &ev("a", Kind::Blocked), "{}", old + 2 * i, None).unwrap();
        s.ingest("claude", &ev("a", Kind::ToolRan), "{}", old + 2 * i + 1, None).unwrap();
    }
    s.ingest("claude", &ev("a", Kind::TurnEnd { agents: 1, shells: 2 }), "{}", old + 100, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Idle), "{}", old + 101, None).unwrap();
    let run = latest_run(&s, "a");
    let before = view(&s.conn, &run).unwrap();
    // Recent events stay.
    assert_eq!(tidy(&s.conn, old + 200).unwrap(), 0);
    let gone = tidy(&s.conn, old + 200 + KEEP_EVENTS_MS).unwrap();
    assert_eq!(gone, 39);
    let after = view(&s.conn, &run).unwrap();
    assert_eq!(
        (after.status, after.agents, after.shells, after.unread, after.last),
        (before.status, before.agents, before.shells, before.unread, before.last)
    );
    assert_eq!(count(&s, "SELECT count(*) FROM event"), 2);
}

fn managed(s: &Store, id: &str, native: &str, relation: &str) {
    s.conn
        .execute(
            "INSERT INTO run (id, agent, native_id, relation, started, launch)
             VALUES (?1,'claude',?2,?3,10,'managed')",
            params![id, native, relation],
        )
        .unwrap();
}

#[test]
fn a_launched_resume_claims_its_prepared_run() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "work"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Ended), "{}", 2, None).unwrap();
    managed(&s, "R2", "a", "resume");
    s.ingest("claude", &ev("a", Kind::Started(Source::Resume)), "{}", 11, Some("R2")).unwrap();
    s.ingest("claude", &prompt("a", "more"), "{}", 12, Some("R2")).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM run"), 2);
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE run='R2'"), 2);
}

#[test]
fn a_session_started_inside_a_launched_one_is_not_claimed() {
    let (_d, mut s) = store();
    managed(&s, "R1", "outer", "fresh");
    // A tool in the outer session runs `claude -p`, which inherits VARDE_RUN.
    s.ingest("claude", &ev("inner", Kind::Started(Source::Startup)), "{}", 10, Some("R1")).unwrap();
    s.ingest("claude", &prompt("inner", "summarise this"), "{}", 11, Some("R1")).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE run='R1'"), 0);
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE native_id='inner'"), 1);
    // Nor is it a task of its own: nobody comes back to it.
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 0);
}

#[test]
fn clearing_a_launched_session_carries_on_its_task() {
    let (_d, mut s) = store();
    s.conn.execute("INSERT INTO task (id, title, created) VALUES ('T', 'the task', 1)", []).unwrap();
    managed(&s, "R1", "x", "fresh");
    s.conn.execute("UPDATE run SET task = 'T', pid = 42, pid_start = 7, tty = '/dev/pts/9'", []).unwrap();
    s.ingest("claude", &prompt("x", "work"), "{}", 11, Some("R1")).unwrap();
    // `/clear`: the old conversation ends, a new one starts, in the
    // same process, still carrying VARDE_RUN=R1.
    s.ingest("claude", &ev("x", Kind::Ended), "{}", 12, Some("R1")).unwrap();
    s.ingest("claude", &ev("y", Kind::Started(Source::Clear)), "{}", 13, Some("R1")).unwrap();
    s.ingest("claude", &prompt("y", "something else"), "{}", 14, Some("R1")).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 1);
    let (task, parent, relation, launch, pid, tty): (String, String, String, String, i64, String) = s
        .conn
        .query_row(
            "SELECT task, parent_run, relation, launch, pid, tty FROM run WHERE native_id = 'y'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(
        (task.as_str(), parent.as_str(), relation.as_str(), launch.as_str(), pid, tty.as_str()),
        ("T", "R1", "fresh", "managed", 42, "/dev/pts/9")
    );
    assert_eq!(run_status(&s.conn, &latest_run(&s, "y")).unwrap(), Some(Status::Working));
}

#[test]
fn a_resume_inside_a_launched_session_is_a_run_in_the_same_process() {
    let (_d, mut s) = store();
    s.conn.execute("INSERT INTO task (id, title, created) VALUES ('T', 'the task', 1)", []).unwrap();
    managed(&s, "R1", "x", "fresh");
    s.conn.execute("UPDATE run SET task = 'T', pid = 42, pid_start = 7, tty = '/dev/pts/9'", []).unwrap();
    let me = || Some((42, 7));
    let hook =
        |s: &mut Store, n: Norm, ts: i64| s.ingest_from("claude", &n, "{}", ts, Some("R1"), &me).unwrap();
    hook(&mut s, prompt("x", "work"), 11);
    // `/resume` into a conversation varde never saw: the old one ends,
    // the other starts, in the same process.
    hook(&mut s, ev("x", Kind::Ended), 12);
    hook(&mut s, ev("z", Kind::Started(Source::Resume)), 13);
    hook(&mut s, prompt("z", "carry on"), 14);
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 1);
    let row: (String, String, String, String, i64) = s
        .conn
        .query_row("SELECT task, parent_run, relation, launch, pid FROM run WHERE native_id = 'z'", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap();
    assert_eq!(row, ("T".into(), "R1".into(), "resume".into(), "managed".into(), 42));
    assert_eq!(run_status(&s.conn, &latest_run(&s, "z")).unwrap(), Some(Status::Working));
    // A conversation with a task of its own goes on in that task.
    s.conn.execute("INSERT INTO task (id, title, created) VALUES ('U', 'other', 1)", []).unwrap();
    s.ingest("claude", &prompt("u", "elsewhere"), "{}", 15, None).unwrap();
    s.conn.execute("UPDATE run SET task = 'U' WHERE native_id = 'u'", []).unwrap();
    s.conn.execute("UPDATE task SET state = 'done', closed = 15 WHERE id = 'U'", []).unwrap();
    hook(&mut s, ev("u", Kind::Started(Source::Resume)), 16);
    let task: String =
        s.conn.query_row("SELECT task FROM run WHERE id = ?1", [latest_run(&s, "u")], |r| r.get(0)).unwrap();
    assert_eq!(task, "U");
    // And picks it up again: a live session is not left under Done.
    assert_eq!(count(&s, "SELECT count(*) FROM task WHERE id = 'U' AND state = 'open'"), 1);
}

#[test]
fn a_resume_started_from_inside_a_launched_session_is_not_its_run() {
    // A `claude --resume` a tool runs inherits VARDE_RUN, in a process
    // of its own.
    let (_d, mut s) = store();
    s.conn.execute("INSERT INTO task (id, title, created) VALUES ('T', 'the task', 1)", []).unwrap();
    managed(&s, "R1", "x", "fresh");
    s.conn.execute("UPDATE run SET task = 'T', pid = 42, pid_start = 7", []).unwrap();
    let other = || Some((43, 7));
    for (n, ts) in [(ev("z", Kind::Started(Source::Resume)), 13), (prompt("z", "inner"), 14)] {
        s.ingest_from("claude", &n, "{}", ts, Some("R1"), &other).unwrap();
    }
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 1);
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE native_id = 'z' AND task IS NOT NULL"), 0);
}

#[test]
fn a_fork_inside_a_launched_session_is_a_run_of_its_task() {
    // pi's `/fork` forks in the same process.
    let (_d, mut s) = store();
    s.conn.execute("INSERT INTO task (id, title, created) VALUES ('T', 'the task', 1)", []).unwrap();
    managed(&s, "R1", "x", "fresh");
    s.conn.execute("UPDATE run SET task = 'T'", []).unwrap();
    s.ingest("claude", &ev("f", Kind::Started(Source::Fork)), "{}", 13, Some("R1")).unwrap();
    s.ingest("claude", &prompt("f", "try the other way"), "{}", 14, Some("R1")).unwrap();
    let row: (String, String, String) = s
        .conn
        .query_row("SELECT task, parent_run, relation FROM run WHERE native_id = 'f'", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(row, ("T".into(), "R1".into(), "fork".into()));
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 1);
}

#[test]
fn a_run_without_a_native_id_takes_the_first_it_is_claimed_with() {
    let (_d, mut s) = store();
    managed(&s, "R1", "", "fresh");
    s.ingest("claude", &ev("picked", Kind::Started(Source::Startup)), "{}", 11, Some("R1")).unwrap();
    s.ingest("claude", &prompt("picked", "work"), "{}", 12, Some("R1")).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE run='R1'"), 2);
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE id='R1' AND native_id='picked'"), 1);
    // Once it has one, another session inheriting VARDE_RUN is not it.
    s.ingest("claude", &prompt("inner", "summarise this"), "{}", 13, Some("R1")).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE run='R1'"), 2);
}

#[test]
fn a_claimed_run_learns_its_transcript() {
    let (_d, mut s) = store();
    managed(&s, "R1", "a", "fresh");
    let n = Norm { transcript: Some("/t/a.jsonl".into()), ..ev("a", Kind::Started(Source::Startup)) };
    s.ingest("claude", &n, "{}", 11, Some("R1")).unwrap();
    let t: String = s.conn.query_row("SELECT transcript FROM run WHERE id='R1'", [], |r| r.get(0)).unwrap();
    assert_eq!(t, "/t/a.jsonl");
}

#[test]
fn why_names_the_events_behind_a_status() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "hi"), "{}", 1, None).unwrap();
    let run = latest_run(&s, "a");
    assert_eq!(view(&s.conn, &run).unwrap().why.event.as_deref(), Some("prompt"));
    s.ingest("claude", &ev("a", Kind::TurnEnd { agents: 0, shells: 0 }), "{}", 2, None).unwrap();
    s.ingest("claude", &ev("a", Kind::Idle), "{}", 3, None).unwrap();
    let task: String = s.conn.query_row("SELECT task FROM run WHERE id=?1", [&run], |r| r.get(0)).unwrap();
    mark_seen(&s.conn, &task, 4).unwrap();
    let w = view(&s.conn, &run).unwrap().why;
    assert_eq!(
        (w.event.as_deref(), w.at, w.idle_at, w.seen_at, w.alive),
        (Some("turn_end"), Some(2), Some(3), Some(4), None)
    );
    assert!(view(&s.conn, "nope").unwrap().why.event.is_none());
}

#[test]
fn an_exit_ends_the_run() {
    let (_d, mut s) = store();
    managed(&s, "R1", "a", "fresh");
    s.ingest("claude", &prompt("a", "work"), "{}", 11, Some("R1")).unwrap();
    s.record_exit("R1", 130, 12).unwrap();
    assert_eq!(run_status(&s.conn, "R1").unwrap(), Some(Status::Exited));
}

#[test]
fn an_exit_ends_every_run_its_process_moved_through() {
    let (_d, mut s) = store();
    s.conn.execute("INSERT INTO task (id, title, created) VALUES ('T', 'the task', 1)", []).unwrap();
    managed(&s, "R1", "x", "fresh");
    s.conn.execute("UPDATE run SET task = 'T', pid = 42, pid_start = 7", []).unwrap();
    s.ingest("claude", &prompt("x", "work"), "{}", 11, Some("R1")).unwrap();
    s.ingest("claude", &ev("y", Kind::Started(Source::Clear)), "{}", 12, Some("R1")).unwrap();
    s.ingest("claude", &prompt("y", "more"), "{}", 13, Some("R1")).unwrap();
    // Another process that once had the same pid.
    managed(&s, "OLD", "o", "fresh");
    s.conn.execute("UPDATE run SET pid = 42, pid_start = 3 WHERE id = 'OLD'", []).unwrap();
    let y = latest_run(&s, "y");
    let mut ended = s.record_exit("R1", 0, 14).unwrap();
    ended.sort();
    let mut want = vec!["R1".to_string(), y.clone()];
    want.sort();
    assert_eq!(ended, want);
    assert_eq!(run_status(&s.conn, &y).unwrap(), Some(Status::Exited));
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE exit_code = 0"), 2);
    assert_eq!(count(&s, "SELECT count(*) FROM event WHERE run = 'OLD'"), 0);
}

#[test]
fn a_dead_process_is_exited_whatever_its_events_say() {
    let dead = |_: i64, _: i64| false;
    let alive = |_: i64, _: i64| true;
    assert_eq!(effective_status(Some(Status::Waiting), Some(1), Some(1), None, &dead), Some(Status::Exited));
    assert_eq!(
        effective_status(Some(Status::Waiting), Some(1), Some(1), None, &alive),
        Some(Status::Waiting)
    );
    // A session varde only hears about has no process to check.
    assert_eq!(effective_status(Some(Status::Waiting), None, None, None, &dead), Some(Status::Waiting));
    // One whose process varde could not find is gone with its terminal,
    // and on Linux with it once the terminal's number is another's.
    let made = tty_ctime("/dev/null");
    let gone = Some(("/dev/pts/99999", Some(1)));
    assert_eq!(effective_status(Some(Status::Working), None, None, gone, &alive), Some(Status::Exited));
    assert_eq!(
        effective_status(Some(Status::Working), None, None, Some(("/dev/null", made)), &dead),
        Some(Status::Working)
    );
    let reused = Some(("/dev/null", made.map(|t| t - 1)));
    let want = if cfg!(target_os = "linux") { Status::Exited } else { Status::Working };
    assert_eq!(effective_status(Some(Status::Working), None, None, reused, &alive), Some(want));
    // A terminal recorded without its ctime is judged by its path alone.
    let unknown = Some(("/dev/null", None));
    assert_eq!(effective_status(Some(Status::Working), None, None, unknown, &alive), Some(Status::Working));
    let unknown_gone = Some(("/dev/pts/99999", None));
    assert_eq!(
        effective_status(Some(Status::Working), None, None, unknown_gone, &alive),
        Some(Status::Exited)
    );
}

#[test]
fn a_fork_started_elsewhere_joins_its_familys_task() {
    let (d, mut s) = store();
    let t = |name: &str| d.path().join(name).display().to_string();
    let with_t = |n: Norm, path: String| Norm { transcript: Some(path), ..n };
    s.ingest("claude", &with_t(prompt("a", "the original"), t("a.jsonl")), "{}", 1, None).unwrap();
    s.ingest("claude", &with_t(ev("b", Kind::Started(Source::Fork)), t("b.jsonl")), "{}", 2, None).unwrap();
    let key = |p: &Path| Some(format!("key-of-{}", p.file_name()?.to_str()?.starts_with('b')));
    let family = |p: &Path, k: &str| k == "key-of-true" && p.ends_with("a.jsonl");
    s.link_fork("claude", "b", &key, &family).unwrap();
    s.ingest("claude", &prompt("b", "continue"), "{}", 3, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 1);
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE native_id='b' AND parent_run IS NOT NULL"), 1);
}

#[test]
fn an_unrecognised_fork_becomes_its_own_task() {
    let (_d, mut s) = store();
    s.ingest("claude", &prompt("a", "the original"), "{}", 1, None).unwrap();
    s.ingest("claude", &ev("b", Kind::Started(Source::Fork)), "{}", 2, None).unwrap();
    s.link_fork("claude", "b", &|_| None, &|_, _| true).unwrap();
    s.ingest("claude", &prompt("b", "continue"), "{}", 3, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM task"), 2);
}

#[test]
fn the_sweep_spares_task_less_runs_with_notes() {
    let (_d, mut s) = store();
    s.ingest("claude", &ev("q", Kind::Started(Source::Startup)), "{}", 1, None).unwrap();
    let run = latest_run(&s, "q");
    s.conn.execute("INSERT INTO note (id, run, text, ts) VALUES ('N1', ?1, 'x', 2)", [&run]).unwrap();
    // A day later, any start sweeps; it must not trip over the note.
    s.ingest("claude", &ev("r", Kind::Started(Source::Startup)), "{}", 3 + SPARE_TTL_MS, None).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM note"), 1);
}

#[test]
fn a_fork_starts_folding_after_its_copied_history() {
    let (d, mut s) = store();
    let t = d.path().join("b.jsonl");
    std::fs::write(&t, "x".repeat(321)).unwrap();
    let n = Norm { transcript: Some(t.display().to_string()), ..ev("b", Kind::Started(Source::Fork)) };
    s.ingest("claude", &n, "{}", 1, None).unwrap();
    let c: i64 = s.conn.query_row("SELECT cairn_cursor FROM run", [], |r| r.get(0)).unwrap();
    assert_eq!(c, 321);
}

#[test]
fn fork_linking_looks_at_recent_runs_in_a_busy_directory() {
    let (d, mut s) = store();
    let t = |name: &str| d.path().join(name).display().to_string();
    for i in 0..250 {
        let n = Norm { transcript: Some(t(&format!("old{i}.jsonl"))), ..prompt(&format!("old{i}"), "old") };
        s.ingest("claude", &n, "{}", i, None).unwrap();
    }
    s.ingest("claude", &Norm { transcript: Some(t("a.jsonl")), ..prompt("a", "original") }, "{}", 1000, None)
        .unwrap();
    s.ingest(
        "claude",
        &Norm { transcript: Some(t("b.jsonl")), ..ev("b", Kind::Started(Source::Fork)) },
        "{}",
        1001,
        None,
    )
    .unwrap();
    s.link_fork("claude", "b", &|_| Some("k".into()), &|p, _| p.ends_with("a.jsonl")).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM run WHERE native_id='b' AND parent_run IS NOT NULL"), 1);
}

#[test]
fn a_newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("varde.db");
    drop(Store::open(&path, Duration::from_millis(200)).unwrap());
    Connection::open(&path).unwrap().pragma_update(None, "user_version", 99).unwrap();
    let err = Store::open(&path, Duration::from_millis(200)).err().unwrap();
    assert!(err.to_string().contains("newer"));
}

#[test]
fn a_store_from_before_naming_names_only_its_open_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("varde.db");
    let c = Connection::open(&path).unwrap();
    for sql in &MIGRATIONS[..2] {
        c.execute_batch(sql).unwrap();
    }
    c.pragma_update(None, "user_version", 2).unwrap();
    c.execute_batch(
        "INSERT INTO task (id, title, prompt_title, state, created) VALUES
           ('OPEN', 'fix the', 'fix the', 'open', 1), ('DONE', 'old', 'old', 'done', 1);
         INSERT INTO task (id, title, created) VALUES ('TYPED', 'Typed', 1);",
    )
    .unwrap();
    drop(c);
    let s = Store::open(&path, Duration::from_millis(200)).unwrap();
    let named = |id: &str| -> Option<i64> {
        s.conn.query_row("SELECT named FROM task WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    };
    assert_eq!((named("OPEN"), named("DONE"), named("TYPED")), (None, Some(0), None));
}
