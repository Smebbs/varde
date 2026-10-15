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
    // Seen, it waits for its next time.
    e.ok(&["seen", &t]);
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
