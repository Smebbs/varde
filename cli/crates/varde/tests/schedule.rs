//! Scheduled tasks: `varde schedule`, `new --when`, and `varde tick`
//! starting a task in the background, as an ordinary session.

mod common;

use serde_json::Value;

use common::*;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn due_now(e: &Env, task: &str) {
    e.db()
        .execute(
            "UPDATE schedule SET next_due = ?2 WHERE task LIKE '%' || upper(?1)",
            (task, now_ms() - 1000),
        )
        .unwrap();
}

fn runs(e: &Env, task: &str) -> Vec<Value> {
    e.show(task)["runs"].as_array().unwrap().clone()
}

#[test]
fn a_schedule_settles_what_nobody_will_be_there_to_answer() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Triage mentions");
    let out = e.run(&["schedule", &t, "every", "weekday", "09:00"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--worktree"), "{out:?}");

    let said = e.ok(&["schedule", &t, "every", "weekday", "09:00", "--worktree", "never"]);
    assert!(said.contains("repeats (every weekday 09:00)"), "{said}");
    assert_eq!(e.show(&t)["schedule"]["rule"], "every weekday 09:00");
    assert!(e.ok(&["show", &t, "--no-git"]).contains("scheduled every weekday 09:00; next "));
    let rows = group(&e.board(), "scheduled").clone();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["schedule"]["label"].as_str().unwrap().starts_with('↻'), "{}", rows[0]);
    assert_eq!(rows[0]["schedule"]["repeats"], true);
    assert!(group(&e.board(), "open").is_empty());

    assert!(e.ok(&["schedule", &t, "--off"]).contains("no longer scheduled"));
    assert_eq!(e.show(&t)["schedule"], Value::Null);
    assert_eq!(group(&e.board(), "open").len(), 1);

    // A rule it cannot read makes no task.
    let repo = e.repo().to_string_lossy().into_owned();
    let out = e.run(&["new", "Someday", "--project", &repo, "--when", "soonish", "--worktree", "never"]);
    assert!(!out.status.success());
    let all: usize =
        e.board()["groups"].as_array().unwrap().iter().map(|g| g["rows"].as_array().unwrap().len()).sum();
    assert_eq!(all, 1);
    let one = e.ok(&["new", "Monday", "--project", &repo, "--when", "at mon 09:00", "--worktree", "never"]);
    assert_eq!(group(&e.board(), "scheduled")[0]["short"], one.trim());
    assert!(group(&e.board(), "scheduled")[0]["schedule"]["label"].as_str().unwrap().starts_with('◷'));
}

#[test]
fn a_due_task_runs_in_the_background_and_comes_round_again_fresh() {
    let e = Env::with_repo(AGENT);
    // Something to fold, so the next run waits for the update.
    let tick = || {
        assert!(e.cmd(&["tick"]).env("FAKE_TALK", "Checked up to #12.").output().unwrap().status.success())
    };

    let t = e.new_task("Triage mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    tick();
    assert!(runs(&e, &t).is_empty(), "not due yet");

    due_now(&e, &t);
    tick();
    // In print mode, with nobody at it, tracked as any session: it ends,
    // and what it did is yours to read.
    poll(100, || runs(&e, &t).first().is_some_and(|r| r["status"] == "exited"));
    let run = &runs(&e, &t)[0];
    assert_eq!((run["relation"].as_str(), run["background"].as_bool()), (Some("fresh"), Some(true)), "{run}");
    assert!(e.argv()[0].starts_with("-p "), "{:?}", e.argv());
    let board = e.board();
    let row = &group(&board, "needs_you")[0];
    assert_eq!((row["short"].as_str(), row["unread"].as_bool()), (Some(t.as_str()), Some(true)), "{row}");
    let next: i64 = e.db().query_row("SELECT next_due FROM schedule", [], |r| r.get(0)).unwrap();
    assert!(next > now_ms());
    let context = std::fs::read_to_string(e.path("context.log")).unwrap();
    assert!(context.contains("This task repeats (every day 06:00)"), "{context}");
    assert!(context.contains("Nobody is watching"), "{context}");
    // Looking at the task does not read what a run found: only opening
    // that run does, or `done`, which ends the run and waits for its next.
    e.ok(&["seen", &t]);
    assert_eq!(group(&e.board(), "needs_you")[0]["short"], t.as_str());
    // Nothing live to end: it says what it did, which is read the result.
    assert!(e.ok(&["done", &t]).starts_with(&format!("marked 1 unread run of {t} read; it repeats")));
    assert_eq!(e.show(&t)["state"], "open");
    assert_eq!(group(&e.board(), "scheduled")[0]["short"], t.as_str());

    // The next run starts fresh once the last one's update is in.
    due_now(&e, &t);
    poll(200, || {
        tick();
        runs(&e, &t).len() == 2
    });
    assert_eq!(runs(&e, &t)[1]["relation"], "fresh");
    let (finished, started): (i64, i64) = e
        .db()
        .query_row(
            "SELECT j.finished, (SELECT max(started) FROM run) FROM job j
             WHERE j.run = (SELECT min(id) FROM run) AND j.state NOT IN ('queued', 'running')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(finished <= started, "update finished {finished}, next session started {started}");
}

#[test]
fn a_session_at_work_in_the_background_is_taken_over() {
    let e = Env::with_repo(AGENT);
    std::fs::write(
        e.path("config.toml"),
        "[agents.claude]\nbackground = [\"--permission-mode\", \"auto\"]\n",
    )
    .unwrap();
    let t = e.new_task("Triage mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    due_now(&e, &t);
    assert!(e.cmd(&["tick"]).env("FAKE_SLEEP", "30").output().unwrap().status.success());
    poll(100, || runs(&e, &t).first().is_some_and(|r| r["why"]["alive"] == true));

    // It has no terminal to go to: a client asks first.
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json"])).unwrap();
    assert_eq!((plan["action"].as_str(), plan["ask"].as_str()), (Some("ask"), Some("takeover")), "{plan}");
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json", "--take-over"])).unwrap();
    assert_eq!(plan["action"], "run", "{plan}");
    let runs = runs(&e, &t);
    assert_eq!(runs[0]["why"]["alive"], false, "the background one stopped");
    assert_eq!(
        (runs[1]["relation"].as_str(), runs[1]["parent_run"].as_str()),
        (Some("resume"), runs[0]["id"].as_str())
    );
    assert!(runs[1].get("background").is_none());
    // The user's arguments for a background session, and only for one.
    e.ok(&["run", plan["run"].as_str().unwrap()]);
    let argv = e.argv();
    assert!(argv[0].starts_with("--permission-mode auto -p "), "{argv:?}");
    assert!(!argv[1].contains("--permission-mode"), "{argv:?}");
}

#[test]
fn a_one_shot_is_spent_once_it_starts_and_a_failure_says_why() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Monday morning");
    e.ok(&["schedule", &t, "at", "mon", "09:00", "--worktree", "never"]);
    // As if made before its worktree was settled: the start would ask.
    e.db().execute("UPDATE schedule SET worktree = NULL", []).unwrap();
    due_now(&e, &t);
    assert!(e.run(&["tick"]).status.success());
    let show = e.show(&t);
    assert!(show["schedule"]["error"].as_str().unwrap().contains("--worktree"), "{show}");
    assert_eq!(group(&e.board(), "scheduled")[0]["schedule"]["late"], true);

    // Tried again after a while.
    e.db().execute("UPDATE schedule SET worktree = 0, tried = tried - 3600000", []).unwrap();
    assert!(e.run(&["tick"]).status.success());
    poll(100, || runs(&e, &t).first().is_some_and(|r| r["status"] == "exited"));
    assert_eq!(e.show(&t)["schedule"], Value::Null);
}

/// Fire `task` now and wait for that run to end.
fn fire(e: &Env, task: &str, env: &[(&str, &str)]) {
    let before = runs(e, task).len();
    due_now(e, task);
    poll(200, || {
        let mut cmd = e.cmd(&["tick"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        assert!(cmd.output().unwrap().status.success());
        let runs = runs(e, task);
        runs.len() > before && runs.last().is_some_and(|r| r["status"] == "exited")
    });
}

#[test]
fn a_run_with_nothing_to_report_goes_back_to_waiting_without_asking() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Scan for mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    let done = format!("done {t}");
    fire(&e, &t, &[("FAKE_DONE", &done)]);
    let context = std::fs::read_to_string(e.path("context.log")).unwrap();
    assert!(context.contains(&format!("and found nothing new, run varde done {t}")));
    assert!(context.contains("Anything you changed, however routine, is for them to see"));
    assert!(std::fs::read_to_string(e.path("done.log")).unwrap().contains("ended this run"));
    let run = &runs(&e, &t)[0];
    assert!(run["done"].is_i64() && run.get("unread").is_none(), "{run}");
    assert_eq!(e.show(&t)["state"], "open");
    assert!(group(&e.board(), "needs_you").is_empty());
    assert_eq!(group(&e.board(), "scheduled")[0]["short"], t.as_str());
    // Nothing left to end: the series goes on until its schedule is off.
    let out = e.run(&["done", &t]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("--off ends the series"), "{out:?}");
    e.ok(&["schedule", &t, "--off"]);
    e.ok(&["done", &t]);
    assert_eq!(e.show(&t)["state"], "done");
}

#[test]
fn every_unread_run_keeps_its_row_in_needs_you_until_it_is_opened() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Scan for mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    fire(&e, &t, &[]);
    fire(&e, &t, &[]);
    let ids: Vec<String> = runs(&e, &t).iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    let rows = group(&e.board(), "needs_you").clone();
    assert_eq!(rows.len(), 1, "one row, however many runs");
    assert_eq!(rows[0]["run"]["id"], ids[1].as_str(), "the newest first");
    assert_eq!(rows[0]["results"], 2, "and how many wait");
    let shown = e.ok(&["show", &t, "--no-git"]);
    assert_eq!(shown.matches("in the background · unread").count(), 2, "{shown}");

    // RET opens the newest unread run's conversation, and reads it.
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json"])).unwrap();
    let opened =
        |plan: &Value| -> Value { runs(&e, &t).into_iter().find(|r| r["id"] == plan["run"]).unwrap() };
    assert_eq!(opened(&plan)["parent_run"], ids[1].as_str(), "{plan}");
    // The older one still needs you.
    let rows = group(&e.board(), "needs_you").clone();
    assert_eq!(rows[0]["run"]["id"], ids[0].as_str(), "{rows:?}");
    assert_eq!(rows[0]["results"], 1);
    // And any run opens by its id.
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json", "--run", &ids[0][20..]])).unwrap();
    assert_eq!(opened(&plan)["parent_run"], ids[0].as_str(), "{plan}");
    assert!(group(&e.board(), "needs_you").is_empty());
    assert!(e.run(&["start", &t, "--run", "zzzzzz"]).status.code() != Some(0));
}

#[test]
fn done_on_a_repeating_task_keeps_its_worktree() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Scan for mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    fire(&e, &t, &[]);
    let out = e.run(&["done", &t, "--remove"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("its worktree is the series'"), "{out:?}");
    assert_eq!(group(&e.board(), "needs_you").len(), 1);
}

#[test]
fn done_says_how_many_runs_it_read() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Monday update");
    e.ok(&["schedule", &t, "every", "mon", "09:00", "--worktree", "never"]);
    fire(&e, &t, &[]);
    fire(&e, &t, &[]);
    fire(&e, &t, &[]);
    let said = e.ok(&["done", &t]);
    assert!(
        said.starts_with(&format!("marked 3 unread runs of {t} read; it repeats (every mon 09:00")),
        "{said}"
    );
    assert!(group(&e.board(), "needs_you").is_empty());
}

#[test]
fn a_run_that_found_nothing_says_so_in_show() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Scan for mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    let done = format!("done {t}");
    fire(&e, &t, &[("FAKE_DONE", &done)]);
    let shown = e.ok(&["show", &t, "--no-git"]);
    assert!(shown.contains("in the background · nothing to report"), "{shown}");
}

#[test]
fn a_start_held_up_or_paused_says_why() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Scan for mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    e.db()
        .execute(
            "UPDATE schedule SET next_due = ?2 WHERE task LIKE '%' || upper(?1)",
            (&t, now_ms() - 3_600_000),
        )
        .unwrap();
    // Nothing has run `varde tick` in this store.
    let said = e.ok(&["schedule", &t]);
    assert!(
        said.contains("; due ") && said.trim_end().ends_with("but varde tick is not running: varde doctor"),
        "{said}"
    );

    let said = e.ok(&["park", &t]);
    assert!(said.contains("its schedule (every day 06:00) waits until it is reopened"), "{said}");
    assert!(e.ok(&["show", &t, "--no-git"]).contains("scheduled every day 06:00; paused while it is parked"));
    let said = e.ok(&["reopen", &t]);
    assert!(said.starts_with(&format!("reopened {t}; scheduled every day 06:00; due ")), "{said}");
}

#[test]
fn a_start_held_by_a_session_says_due_and_after_a_day_late() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Triage mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    due_now(&e, &t);
    assert!(e.cmd(&["tick"]).env("FAKE_SLEEP", "30").output().unwrap().status.success());
    poll(100, || runs(&e, &t).first().is_some_and(|r| r["why"]["alive"] == true));
    let due = |ago: i64| {
        e.db()
            .execute(
                "UPDATE schedule SET next_due = ?2 WHERE task LIKE '%' || upper(?1)",
                (&t, now_ms() - ago),
            )
            .unwrap();
    };
    due(600_000);
    let list = e.ok(&["list"]);
    assert!(list.contains("repo · ↻ due · "), "{list}");
    assert!(e.ok(&["schedule", &t]).contains("waiting for session "));
    due(2 * 86_400_000);
    let list = e.ok(&["list"]);
    assert!(list.contains("repo · ↻ late · "), "{list}");
    let said = e.ok(&["done", &t]);
    assert!(said.contains("it repeats (every day 06:00, the next run starts now)"), "{said}");
}

#[test]
fn show_folds_a_schedules_older_runs_that_found_nothing() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Watch the queue");
    e.ok(&["schedule", &t, "every", "30m", "--worktree", "never"]);
    let done = format!("done {t}");
    for _ in 0..12 {
        fire(&e, &t, &[("FAKE_DONE", &done)]);
    }
    let shown = e.ok(&["show", &t, "--no-git"]);
    assert_eq!(shown.matches("nothing to report").count(), 10, "{shown}");
    assert!(shown.contains("and 2 earlier that found nothing"), "{shown}");

    e.ok(&["drop", &t]);
    assert!(e.ok(&["schedule", &t]).contains("not while it is dropped, and again if it is reopened"));
}

#[test]
fn now_starts_the_next_run_to_try_a_schedule_out() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Monday update");
    assert!(!e.run(&["schedule", &t, "--now"]).status.success(), "no schedule");
    e.ok(&["schedule", &t, "every", "mon", "09:00", "--worktree", "never"]);
    let said = e.ok(&["schedule", &t, "--now"]);
    assert!(
        said.starts_with(&format!("{t} started in the background; every mon 09:00; next Mon 09:00")),
        "{said}"
    );
    poll(100, || runs(&e, &t).first().is_some_and(|r| r["status"] == "exited"));
    assert_eq!(runs(&e, &t)[0]["background"], true);
    assert_eq!(group(&e.board(), "needs_you").len(), 1);
    // Changed once it has run, the series goes on: from its next start.
    let said = e.ok(&["schedule", &t, "every", "mon", "08:30"]);
    assert!(said.contains("repeats (every mon 08:30); next "), "{said}");
    e.ok(&["park", &t]);
    let out = e.run(&["schedule", &t, "--now"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("is parked: varde reopen"), "{out:?}");
}

#[test]
fn a_run_with_nothing_to_report_leaves_an_earlier_finding_unread() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Scan for mentions");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    fire(&e, &t, &[]);
    let found = runs(&e, &t)[0]["id"].as_str().unwrap().to_string();
    let done = format!("done {t}");
    fire(&e, &t, &[("FAKE_DONE", &done)]);
    // Its own `done` ends its own run, and reads nothing of the others.
    assert!(
        runs(&e, &t).iter().any(|r| r["id"] == found.as_str() && r["unread"] == true),
        "the earlier finding is still unread"
    );
    let rows = group(&e.board(), "needs_you").clone();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["run"]["id"], found.as_str());
}
