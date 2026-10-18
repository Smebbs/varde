//! Repeating tasks through whole flows, as they are lived: after every
//! step, what the store holds and what every surface says agree. A
//! finding stays unread until it is opened or you `d` it; the board's row,
//! its count and its group follow from that; `show` and `varde schedule`
//! say the same of its schedule; and no session is stopped as a side
//! effect of another run.

mod common;

use std::collections::BTreeSet;
use std::process::Child;

use serde_json::Value;

use common::*;

fn id_of(run: &Value) -> String {
    run["id"].as_str().unwrap().to_string()
}

/// Fire `task` with a session that stays at work, and wait until it is.
fn fire_working(e: &Env, task: &str) -> String {
    let before = runs(e, task).len();
    due_now(e, task);
    e.cmd(&["tick"]).env("FAKE_SLEEP", "60").output().unwrap();
    poll(100, || runs(e, task).len() > before && runs(e, task).last().unwrap()["why"]["alive"] == true);
    let run = id_of(runs(e, task).last().unwrap());
    // Mid-turn, as a real one at work is.
    e.db()
        .execute(
            "INSERT INTO event (run, kind, ts, raw) VALUES (?1, 'prompt', ?2, '{}')",
            (&run, now_ms() + 60_000),
        )
        .unwrap();
    run
}

/// Open `run` as a client does (RET on its session line), the session
/// staying at its prompt.
fn open_live(e: &Env, task: &str, run: &str) -> (String, Child) {
    let plan: Value = serde_json::from_str(&e.ok(&["start", task, "--json", "--run", &run[20..]])).unwrap();
    let id = plan["run"].as_str().unwrap().to_string();
    let child = e.cmd(&["run", &id]).env("FAKE_SLEEP", "60").spawn().unwrap();
    // Alive once its pid is recorded; at its prompt once its turn ends.
    poll(100, || {
        runs(e, task)
            .iter()
            .any(|r| r["id"] == id.as_str() && r["why"]["alive"] == true && r["why"]["event"] == "turn_end")
    });
    (id, child)
}

fn alive(e: &Env, task: &str, run: &str) -> bool {
    runs(e, task).iter().any(|r| r["id"] == run && r["why"]["alive"] == true)
}

/// The task's row on the board, and the group it is in.
fn row(e: &Env, short: &str) -> (String, Value) {
    let board = e.board();
    for g in board["groups"].as_array().unwrap() {
        for r in g["rows"].as_array().unwrap() {
            if r["short"] == short {
                return (g["key"].as_str().unwrap().to_string(), r.clone());
            }
        }
    }
    panic!("{short} is on no board: {board}")
}

/// What must hold after every step: exactly `unread` are unread; the row
/// says so in its count and group; `show` and `schedule` agree.
fn check(e: &Env, t: &str, step: &str, unread: &[&String]) {
    let want: BTreeSet<&str> = unread.iter().map(|s| s.as_str()).collect();
    let runs = runs(e, t);
    let got: BTreeSet<&str> =
        runs.iter().filter(|r| r["unread"] == true).map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(got, want, "{step}: unread runs");

    let (group, r) = row(e, t);
    let results = r["results"].as_u64().unwrap_or(0) as usize;
    let at_work = runs.iter().any(|r| r["why"]["alive"] == true && r["status"] == "working");
    // A closed task's row is folded away; `park` says what waits in it.
    if e.show(t)["state"] == "open" {
        assert_eq!(results, want.len(), "{step}: the row's count, {r}");
        match (want.is_empty(), at_work) {
            (_, true) => assert_eq!(group, "working", "{step}: a run at work is where the task is, {r}"),
            (false, false) => assert_eq!(group, "needs_you", "{step}: a finding needs you, {r}"),
            (true, false) => assert_ne!(group, "needs_you", "{step}: nothing needs you, {r}"),
        }
        // RET opens the newest finding, unless a session of yours waits
        // at its prompt, which the row is then.
        let live_row = runs.iter().any(|x| x["id"] == r["run"]["id"] && x["why"]["alive"] == true);
        if !want.is_empty() && group == "needs_you" && !live_row {
            let newest = runs.iter().rev().find(|r| r["unread"] == true).unwrap();
            assert_eq!(r["run"]["id"], newest["id"], "{step}: RET opens the newest finding");
        }
    }
    // What the list shows of it.
    let list = e.ok(&["list", "--all"]);
    let line = list.lines().find(|l| l.ends_with(t)).unwrap_or_else(|| panic!("{step}: {list}"));
    let says = |n: usize| line.contains(&format!("↻ {n} unread"));
    if e.show(t)["state"] == "open" && (results > 1 || (results == 1 && r["unread"] != true)) {
        assert!(says(results), "{step}: the row says how many wait: {line}");
    }
    // `show` and `schedule` say the same of it.
    let shown = e.ok(&["show", t, "--no-git"]);
    if let Some(s) = shown.lines().find_map(|l| l.strip_prefix("scheduled ")) {
        assert_eq!(e.ok(&["schedule", t]).trim(), s, "{step}: show and schedule");
    }
    // `show` marks each run as the store has it.
    for run in &runs {
        let short = &run["id"].as_str().unwrap()[20..].to_lowercase();
        if let Some(l) = shown.lines().find(|l| l.trim_start().starts_with(short.as_str())) {
            assert_eq!(l.contains("· unread"), run["unread"] == true, "{step}: show says of {short}: {l}");
        }
    }
}

#[test]
fn a_monday_update_over_weeks_keeps_every_finding_until_opened() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Monday update");
    e.ok(&["schedule", &t, "every", "mon", "09:00", "--worktree", "never"]);
    let nothing = format!("done {t}");
    let quiet = [("FAKE_DONE", nothing.as_str())];

    // Tried out at once.
    assert!(e.ok(&["schedule", &t, "--now"]).contains("started in the background"));
    poll(100, || runs(&e, &t).first().is_some_and(|r| r["status"] == "exited"));
    let w0 = id_of(&runs(&e, &t)[0]);
    check(&e, &t, "tried out", &[&w0]);

    // Read: RET opens it, the session ends.
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json"])).unwrap();
    e.ok(&["run", plan["run"].as_str().unwrap()]);
    check(&e, &t, "read", &[]);

    // A finding, then a week with nothing to report: the finding waits.
    let w1 = fire(&e, &t, &[]);
    check(&e, &t, "week 1 found something", &[&w1]);
    fire(&e, &t, &quiet);
    check(&e, &t, "week 2 found nothing", &[&w1]);

    // Three weeks away.
    let w3 = fire(&e, &t, &[]);
    let w4 = fire(&e, &t, &[]);
    check(&e, &t, "away", &[&w1, &w3, &w4]);

    // The oldest first, by its line; then RET twice: newest, then next.
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json", "--run", &w1[20..]])).unwrap();
    e.ok(&["run", plan["run"].as_str().unwrap()]);
    check(&e, &t, "the oldest opened", &[&w3, &w4]);
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json"])).unwrap();
    e.ok(&["run", plan["run"].as_str().unwrap()]);
    check(&e, &t, "RET: the newest", &[&w3]);

    // The time changes while one waits.
    assert!(e.ok(&["schedule", &t, "every", "tue", "10:00"]).contains("; next "));
    check(&e, &t, "rule changed", &[&w3]);

    // Away again: parked, it says what waits; reopened, it is all there.
    let said = e.ok(&["park", &t]);
    assert!(
        said.contains("its schedule (every tue 10:00) waits") && said.contains("1 unread run waits in it"),
        "{said}"
    );
    check(&e, &t, "parked", &[&w3]);
    e.ok(&["reopen", &t]);
    check(&e, &t, "reopened", &[&w3]);

    // d from the terminal marks what waits read, and says so.
    fire(&e, &t, &[]);
    let said = e.ok(&["done", &t]);
    assert!(
        said.starts_with(&format!("marked 2 unread runs of {t} read; it repeats (every tue 10:00")),
        "{said}"
    );
    check(&e, &t, "d", &[]);

    // Ending the series.
    e.ok(&["schedule", &t, "--off"]);
    e.ok(&["done", &t]);
    assert_eq!(e.show(&t)["state"], "done");
}

#[test]
fn runs_that_overlap_never_read_or_stop_each_other() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Watch the error rate");
    e.ok(&["schedule", &t, "every", "30m", "--worktree", "never"]);
    let nothing = format!("done {t}");

    // A finding, then a run at work: the row is where the work is, and
    // says the finding waits.
    let f1 = fire(&e, &t, &[]);
    let busy = fire_working(&e, &t);
    check(&e, &t, "a run at work, a finding waiting", &[&f1]);
    let (group, r) = row(&e, &t);
    assert_eq!(group, "working");
    let list = e.ok(&["list"]);
    assert!(list.contains("working · repo · ↻ 1 unread"), "{list}\n{r}");

    // d from the terminal while it works: it stops, and is not then a
    // finding of its own.
    let said = e.ok(&["done", &t]);
    assert!(said.contains("its session was stopped"), "{said}");
    poll(100, || !alive(&e, &t, &busy));
    std::thread::sleep(std::time::Duration::from_millis(300));
    check(&e, &t, "d while it worked", &[]);

    // Parked while one works: on reopen, nothing it did not find waits.
    let busy = fire_working(&e, &t);
    e.ok(&["park", &t]);
    poll(100, || !alive(&e, &t, &busy));
    std::thread::sleep(std::time::Duration::from_millis(300));
    e.ok(&["reopen", &t]);
    check(&e, &t, "parked while it worked, reopened", &[]);

    // A finding you have open, and a run that finds nothing meanwhile:
    // your session goes on, and the older finding still waits.
    let f2 = fire(&e, &t, &[]);
    let f3 = fire(&e, &t, &[]);
    let (mine, mut child) = open_live(&e, &t, &f3);
    check(&e, &t, "you opened the newest", &[&f2]);
    // The tick holds the next start for your session: start one by hand,
    // as a run that began before you opened yours would be.
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json", "--fresh"])).unwrap();
    let bg = plan["run"].as_str().unwrap().to_string();
    e.db().execute("UPDATE run SET background = 1 WHERE id = ?1", [&bg]).unwrap();
    e.cmd(&["run", &bg]).env("FAKE_DONE", &nothing).output().unwrap();
    assert!(alive(&e, &t, &mine), "a run's own done stopped your session");
    check(&e, &t, "a run found nothing while you read", &[&f2]);

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn done_kept_running_ends_the_runs_and_says_so() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Nightly triage");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    let busy = fire_working(&e, &t);
    let said = e.ok(&["done", &t, "--keep-running"]);
    assert!(said.starts_with(&format!("ended this run of {t}; its session keeps running")), "{said}");
    assert!(alive(&e, &t, &busy));
    e.ok(&["done", &t]);
    poll(100, || !alive(&e, &t, &busy));
    std::thread::sleep(std::time::Duration::from_millis(300));
    check(&e, &t, "kept, then stopped", &[]);
}

#[test]
fn an_agent_closing_a_task_leaves_your_session_and_its_worktree() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Fix it");
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json", "--worktree", "always"])).unwrap();
    let mine = plan["run"].as_str().unwrap().to_string();
    let mut child = e.cmd(&["run", &mine]).env("FAKE_SLEEP", "60").spawn().unwrap();
    poll(100, || alive(&e, &t, &mine));
    let path = e.show(&t)["worktree"]["path"].as_str().unwrap().to_string();

    // An agent elsewhere drops it: refused while your session is in it.
    let out = e.cmd(&["drop", &t]).env("CLAUDECODE", "1").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("a session of yours runs in"), "{out:?}");
    assert!(alive(&e, &t, &mine) && std::path::Path::new(&path).exists());

    // It closes it with --remove: your session goes on, and so does its
    // worktree, and it says so.
    let out = e.cmd(&["done", &t, "--remove"]).env("CLAUDECODE", "1").output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("a session of it is left running"), "{out:?}");
    assert!(alive(&e, &t, &mine), "your session was stopped");
    assert!(std::path::Path::new(&path).exists(), "its worktree was removed under it");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_run_that_failed_says_so_and_still_waits_to_be_read() {
    let e = Env::with_repo(AGENT);
    let t = e.new_task("Nightly triage");
    e.ok(&["schedule", &t, "every", "day", "06:00", "--worktree", "never"]);
    let run = fire(&e, &t, &[("FAKE_EXIT", "1")]);
    let shown = e.ok(&["show", &t, "--no-git"]);
    assert!(shown.contains("· in the background · unread · failed (exit 1): "), "{shown}");
    check(&e, &t, "a run failed", &[&run]);
}
