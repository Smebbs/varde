//! Loops and trails, end to end: `varde loop` starting fresh background
//! rounds until a check passes, `varde check` running in the task's
//! checkout, and a recipe's trail moving step by step. The agent is the
//! fake `claude`, wrapped so a background round can change files as a
//! real session would.

mod common;

use serde_json::Value;

use common::*;

/// The fake agent, after `prelude` runs in each background round.
fn agent(prelude: &str) -> String {
    format!(
        "#!/bin/sh\nif [ -n \"$VARDE_BACKGROUND\" ]; then\n{prelude}\nfi\n{}",
        &AGENT["#!/bin/sh\n".len()..]
    )
}

/// A repository whose agent runs `prelude` in each round.
fn repo(prelude: &str) -> Env {
    let e = Env::with_repo(&agent(prelude));
    e.script("claude", &agent(prelude));
    e
}

fn flow(e: &Env, t: &str) -> Value {
    e.show(t)["flow"].clone()
}

fn loop_state(e: &Env, t: &str) -> String {
    flow(e, t)["loop"]["state"]["state"].as_str().unwrap_or("").to_string()
}

fn rounds(e: &Env, t: &str) -> Vec<Value> {
    flow(e, t)["loop"]["rounds"].as_array().cloned().unwrap_or_default()
}

/// Wait for a loop to stop at anything but work.
fn settle(e: &Env, t: &str) -> String {
    poll(600, || !matches!(loop_state(e, t).as_str(), "working" | "checking" | "due" | ""));
    loop_state(e, t)
}

fn row<'a>(board: &'a Value, short: &str) -> &'a Value {
    all_rows(board).into_iter().find(|r| r["short"] == short).unwrap()
}

#[test]
fn a_loop_starts_fresh_rounds_until_its_check_passes() {
    // Each round counts itself; the check passes from the second.
    let e = repo("n=$(cat .rounds 2>/dev/null || echo 0); echo $((n+1)) > .rounds");
    let t = e.new_task("Make it pass");
    let said = e.ok(&["loop", &t, "--check", "test \"$(cat .rounds 2>/dev/null)\" -ge 2", "--max", "3"]);
    assert!(said.contains("loops until"), "{said}");
    assert_eq!(settle(&e, &t), "verified");
    let rs = rounds(&e, &t);
    assert_eq!(rs.len(), 2, "{rs:?}");
    // Each round is a fresh conversation, in the background, in a
    // worktree of its own; the second is told how the first failed.
    let runs = runs(&e, &t);
    assert!(runs.iter().all(|r| r["relation"] == "fresh" && r["background"] == true), "{runs:?}");
    let argv = e.argv();
    assert!(argv[0].contains("This task runs as a loop"), "{argv:?}");
    assert!(argv[1].contains("Round 2 of 3"), "{argv:?}");
    let checks = flow(&e, &t)["loop"]["checks"].as_array().unwrap().clone();
    assert_eq!(checks.iter().map(|c| c["verdict"].as_str().unwrap()).collect::<Vec<_>>(), ["fail", "pass"]);
    // It is yours now, saying so.
    let board = e.board();
    let r = row(&board, &t);
    assert_eq!(r["flow"]["label"], "✓ verified in 2", "{r}");
    assert!(group(&board, "needs_you").iter().any(|x| x["short"] == t.as_str()), "{board}");
    assert!(e.ok(&["show", &t]).contains("Loop    verified after 2 rounds"));
}

#[test]
fn the_same_failure_twice_moves_up_a_profile_then_stops() {
    // Nothing changes: every round fails the same way.
    let e = repo(":");
    let t = e.new_task("Never passes");
    e.ok(&["loop", &t, "--check", "echo the test failed; false", "--max", "5", "--escalate", "deep"]);
    assert_eq!(settle(&e, &t), "stuck");
    let rs = rounds(&e, &t);
    assert_eq!(rs.len(), 3, "{rs:?}");
    assert_eq!(rs[2]["profile"], "deep", "{rs:?}");
    // The escalated round ran on the deep profile's model and effort.
    let argv = e.argv();
    assert!(argv[2].contains("--model opus --effort high"), "{argv:?}");
    assert!(!argv[0].contains("--model"), "{argv:?}");
    assert!(row(&e.board(), &t)["flow"]["label"].as_str().unwrap().starts_with("■ stuck after 3"));
    // Run again, it keeps how it was set.
    e.ok(&["loop", &t, "--pause"]);
    e.ok(&["loop", &t]);
    let def = flow(&e, &t)["loop"]["def"].clone();
    assert_eq!((def["max"].as_i64(), def["escalate"][0].as_str()), (Some(5), Some("deep")), "{def}");
}

#[test]
fn a_round_that_touches_the_guard_is_flagged() {
    let e = repo("echo weakened >> tests/a.txt");
    std::fs::create_dir(e.repo().join("tests")).unwrap();
    std::fs::write(e.repo().join("tests/a.txt"), "a real test\n").unwrap();
    git(&e.repo(), &["add", "."]);
    git(&e.repo(), &["commit", "-q", "-m", "tests"]);
    let t = e.new_task("Do not cheat");
    e.ok(&["check", &t, "--guard", "tests/*", "--", "true"]);
    e.ok(&["loop", &t]);
    assert_eq!(settle(&e, &t), "flagged");
    let label = row(&e.board(), &t)["flow"]["label"].as_str().unwrap().to_string();
    assert_eq!(label, "⚑ touched tests/a.txt");
}

#[test]
fn a_check_runs_on_demand_and_says_what_it_found() {
    let e = repo(":");
    let t = e.new_task("Just check");
    // No checkout yet: the project is checked.
    e.ok(&["check", &t, "--", "echo 3 passed; echo 1 failed; exit 1"]);
    let said = e.ok(&["check", &t, "--now"]);
    assert!(said.contains("✗ fail · 1 failed"), "{said}");
    assert_eq!(row(&e.board(), &t)["flow"]["label"], "✗ 1 failed");
    e.ok(&["check", &t, "--", "true"]);
    assert!(e.ok(&["check", &t, "--now"]).contains("✓ pass"));
    assert!(e.show(&t)["flow"]["verified"].as_bool().unwrap());
}

#[test]
fn a_start_of_yours_pauses_the_loop() {
    let e = repo(":");
    let t = e.new_task("Mine now");
    e.ok(&["check", &t, "--", "false"]);
    e.db().execute("INSERT INTO loop (task, max, started, paused) SELECT id, 3, 0, 0 FROM task", []).unwrap();
    e.ok(&["start", &t, "--worktree", "never", "--json"]);
    assert_eq!(loop_state(&e, &t), "paused");
    assert!(row(&e.board(), &t)["flow"]["label"].as_str().unwrap().starts_with("↺ paused"));
}

#[test]
fn a_recipe_makes_a_trail_that_moves_step_by_step() {
    let e = repo("echo done > \"step-$(basename \"$PWD\")\"");
    let recipe = e.path("two.toml");
    std::fs::write(
        &recipe,
        r#"
label = "two"
[[steps]]
id = "first"
check = { cmd = "true" }
[[steps]]
id = "second"
needs = ["first"]
profile = "quick"
check = { cmd = "true" }
output = "report"
"#,
    )
    .unwrap();
    let out = e.ok(&["recipe", "check", recipe.to_str().unwrap()]);
    assert!(out.contains("is a usable recipe"), "{out}");
    let made = e.ok(&["new", "Ship it", "--recipe", recipe.to_str().unwrap(), "--json"]);
    let made: Value = serde_json::from_str(made.lines().next().unwrap()).unwrap();
    let (a, b) = (
        made["tasks"][0]["short"].as_str().unwrap().to_string(),
        made["tasks"][1]["short"].as_str().unwrap().to_string(),
    );
    // Nothing starts until the trail is set going; the second waits.
    assert!(runs(&e, &a).is_empty());
    assert_eq!(row(&e.board(), &b)["flow"]["label"], format!("⧗ {a}"));
    e.ok(&["trail", &a, "--start"]);
    poll(400, || e.show(&b)["flow"]["verified"].as_bool().unwrap_or(false));
    // The second ran on its profile, in the first's worktree, told it follows it.
    let b_runs = runs(&e, &b);
    let a_runs = runs(&e, &a);
    assert_eq!(b_runs[0]["cwd"], a_runs[0]["cwd"], "shares its upstream's tree");
    assert!(e.argv()[1].contains("--model haiku"), "{:?}", e.argv());
    assert!(e.argv()[1].contains("This task follows"), "{:?}", e.argv());
    let drawn = e.ok(&["trail", &a]);
    assert!(drawn.contains("├─") || drawn.contains("└─"), "{drawn}");
    assert!(e.ok(&["show", &b]).contains("made from recipe two@"));
}

#[test]
fn an_approved_gate_waits_for_you() {
    // Each round changes the shared worktree: the approval it started on
    // lapses, which must not stop the step that started.
    let e = repo("echo more >> notes.txt");
    let recipe = e.path("gated.toml");
    std::fs::write(
        &recipe,
        r#"
[[steps]]
id = "spec"
gate = "approved"
[[steps]]
id = "build"
needs = ["spec"]
check = { cmd = "true" }
"#,
    )
    .unwrap();
    let made = e.ok(&["new", "Gated", "--recipe", recipe.to_str().unwrap(), "--start", "--json"]);
    let made: Value = serde_json::from_str(made.lines().next().unwrap()).unwrap();
    let (spec, build) = (
        made["tasks"][0]["short"].as_str().unwrap().to_string(),
        made["tasks"][1]["short"].as_str().unwrap().to_string(),
    );
    poll(200, || runs(&e, &spec).first().is_some_and(|r| r["status"] == "exited"));
    e.ok(&["tick"]);
    assert!(runs(&e, &build).is_empty(), "waits for your approval");
    assert_eq!(row(&e.board(), &build)["flow"]["label"], format!("⧗ your approval of {spec}"));
    assert!(e.ok(&["approve", &spec]).contains(&build));
    poll(400, || e.show(&build)["flow"]["verified"].as_bool().unwrap_or(false));
    assert_eq!(runs(&e, &build)[0]["cwd"], runs(&e, &spec)[0]["cwd"], "it works in the spec's tree");
    assert!(e.show(&build)["flow"]["after"][0]["met"].as_bool().unwrap(), "behind it once it started");
}

#[test]
fn a_tasks_own_session_may_not_approve() {
    // A session that approves its own gate, or the one it waits on, as
    // the fake agent does when told to.
    let e = repo(":");
    let t = e.new_task("Gate");
    let out = e.cmd(&["new", "Asks", "--json"]).output().unwrap();
    let other: Value = serde_json::from_slice(&out.stdout).unwrap();
    let other = other["short"].as_str().unwrap().to_string();
    e.ok(&["start", &other, "--worktree", "never", "--json"]);
    let run = runs(&e, &other)[0]["id"].as_str().unwrap().to_string();
    e.cmd(&["run", &run]).env("FAKE_DONE", format!("approve {t}")).output().unwrap();
    let said = std::fs::read_to_string(e.path("done.log")).unwrap();
    assert!(said.contains("is the user's to do"), "{said}");
    assert!(!e.show(&t)["flow"]["approved"].as_bool().unwrap());
    // You may.
    assert!(e.ok(&["approve", &t]).contains("approved"));
}

#[test]
fn recipes_list_show_and_say_how_to_write_one() {
    let e = Env::new();
    let list = e.ok(&["recipe", "list"]);
    for name in ["fix-until-green", "bugfix", "feature", "draft-revise", "research"] {
        assert!(list.contains(name), "{list}");
    }
    let shown = e.ok(&["recipe", "show", "bugfix", "--origin", "--set", "fix.output=draft-pr"]);
    assert!(shown.contains("output draft-pr"), "{shown}");
    assert!(shown.contains("output: --set"), "{shown}");
    assert!(shown.contains("loop: from fix-until-green"), "{shown}");
    let guide = e.ok(&["recipe", "guide"]);
    assert!(guide.contains("[[steps]]") && guide.contains("varde recipe check"), "{guide}");
    let profiles = e.ok(&["profiles"]);
    assert!(profiles.contains("deep") && profiles.contains("opus"), "{profiles}");
}
