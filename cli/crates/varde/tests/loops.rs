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
    assert!(said.contains("loops in the background until its check passes"), "{said}");
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
    assert_eq!(r["flow"]["label"], "✓ verified", "{r}");
    assert!(group(&board, "needs_you").iter().any(|x| x["short"] == t.as_str()), "{board}");
    assert!(e.ok(&["show", &t]).contains("Next    ✓ verified"));
    // Followed once it is over: what it ended on, and then it stops.
    let followed = e.ok(&["follow", &t]);
    assert!(followed.contains("── round 2") && followed.contains("── check: ✓ pass"), "{followed}");
    assert!(followed.contains("── loop: verified after 2 rounds"), "{followed}");
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
    assert!(row(&e.board(), &t)["flow"]["label"].as_str().unwrap().starts_with("■ stuck"));
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
fn a_guarded_test_you_fix_is_yours_once_you_say_the_guard_again() {
    let e = repo(":");
    std::fs::create_dir(e.repo().join("tests")).unwrap();
    std::fs::write(e.repo().join("tests/a.txt"), "expects 29\n").unwrap();
    git(&e.repo(), &["add", "."]);
    git(&e.repo(), &["commit", "-q", "-m", "tests"]);
    let t = e.new_task("A test of mine was wrong");
    e.ok(&["check", &t, "--guard", "tests/*", "--", "true"]);
    // (Of another size: git cannot tell a change of the same size made
    // within the second.)
    std::fs::write(e.repo().join("tests/a.txt"), "expects 39, fixed\n").unwrap();
    assert!(e.ok(&["check", &t, "--now"]).contains("touched tests/a.txt"));
    e.ok(&["check", &t, "--guard", "tests/*"]);
    let said = e.ok(&["check", &t, "--now"]);
    assert!(said.contains("✓ pass"), "{said}");
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
fn a_session_of_yours_attends_the_loop() {
    let e = repo(":");
    let t = e.new_task("With you");
    e.ok(&["check", &t, "--", "echo the test failed; false"]);
    // Set, but not going: nothing starts in the background meanwhile.
    e.db().execute("INSERT INTO loop (task, max, started, paused) SELECT id, 3, 0, 0 FROM task", []).unwrap();
    e.ok(&["start", &t, "--worktree", "never", "--json"]);
    let run = runs(&e, &t)[0]["id"].as_str().unwrap().to_string();
    let mut session = e.cmd(&["run", &run]).env("FAKE_SLEEP", "30").spawn().unwrap();
    // Its turn ended: checked, and the failure is its next prompt.
    let said = || -> Option<String> {
        e.db().query_row("SELECT text FROM message WHERE sender = 'varde''s loop'", [], |r| r.get(0)).ok()
    };
    poll(200, || said().is_some());
    let text = said().unwrap();
    assert!(text.contains("Round 2 of 3") && text.contains("the test failed"), "{text}");
    assert_eq!(loop_state(&e, &t), "attended");
    assert_eq!(row(&e.board(), &t)["flow"]["label"], "↺ with you");
    assert!(e.ok(&["show", &t]).contains("↺ with you"));
    // Left, the loop goes on in the background.
    unsafe { libc::kill(session.id() as i32, libc::SIGTERM) };
    session.wait().unwrap();
    e.db().execute("UPDATE task SET auto = 1", []).unwrap();
    poll(200, || {
        e.ok(&["tick"]);
        runs(&e, &t).iter().any(|r| r["background"] == true)
    });
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
    assert_eq!(row(&e.board(), &b)["flow"]["label"], format!("⧗ after {a}"));
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
    assert!(e.ok(&["show", &b]).contains("recipe two"));
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
    assert_eq!(row(&e.board(), &build)["flow"]["label"], format!("⧗ after {spec}"));
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

/// Whether this system can make a sandbox.
fn bwrap_works() -> bool {
    std::process::Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "true"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn a_round_runs_in_the_sandbox_and_reaches_only_its_checkout() {
    if !bwrap_works() {
        eprintln!("skipped: no bwrap here");
        return;
    }
    let outside = format!("/var/tmp/varde-sandbox-test-{}", std::process::id());
    // Each round writes its checkout, tries a file outside it, and tries
    // to change its own check.
    let e = repo(&format!(
        "echo ok > done.txt\ntouch {outside} 2> outside.txt\n\"$VARDE_BIN\" check \"$(cat \"$FAKE_HOME/task\")\" -- true 2> refused.txt"
    ));
    // The test's own files live in its directory, which a sandbox would hide.
    std::fs::write(e.path("config.toml"), format!("[sandbox]\nwrite = [\"{}\"]\n", e.path("").display()))
        .unwrap();
    let t = e.new_task("Fenced");
    std::fs::write(e.path("task"), &t).unwrap();
    let out = e
        .cmd(&["loop", &t, "--check", "test -f done.txt", "--max", "2"])
        .env("VARDE_SANDBOX", "on")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(settle(&e, &t), "verified");
    let fenced: bool = e.db().query_row("SELECT fenced FROM run", [], |r| r.get(0)).unwrap();
    assert!(fenced, "the round ran in the sandbox");
    assert!(!std::path::Path::new(&outside).exists(), "nothing outside its checkout was written");
    let _ = std::fs::remove_file(&outside);
    let dir = runs(&e, &t)[0]["cwd"].as_str().unwrap().to_string();
    let said = |f: &str| std::fs::read_to_string(std::path::Path::new(&dir).join(f)).unwrap_or_default();
    assert!(said("outside.txt").contains("Read-only"), "{}", said("outside.txt"));
    assert!(said("refused.txt").contains("not for a session in a sandbox"), "{}", said("refused.txt"));
    // Inside one, its agent may use every tool, and is told where it stands.
    let argv = e.argv();
    assert!(argv[0].contains("--allowedTools Bash WebFetch WebSearch"), "{argv:?}");
    assert!(argv[0].contains("You work in a sandbox"), "{argv:?}");
    let shown = e.cmd(&["show", &t]).env("VARDE_SANDBOX", "on").output().unwrap();
    assert!(String::from_utf8_lossy(&shown.stdout).contains("sandbox default, in bwrap"), "{shown:?}");
}

#[test]
fn a_sandbox_of_yours_denies_commands_and_a_task_made_inside_one_keeps_it() {
    let e = repo(":");
    std::fs::write(
        e.path("config.toml"),
        "[sandboxes.look]\nshow = [\"~/.azure\"]\ndeny = [\"az * delete*\"]\n",
    )
    .unwrap();
    let shown = e.ok(&["sandbox", "look"]);
    assert!(shown.contains("denies   az * delete*") && shown.contains("shows    ~/.azure"), "{shown}");
    assert!(!shown.contains("hides    ~/.ssh ~/.gnupg ~/.aws ~/.azure"), "{shown}");
    let out = e.cmd(&["new", "Nope", "--sandbox", "nowhere"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("no sandbox nowhere"), "{out:?}");
    let t = e.ok(&["new", "Look around", "--sandbox", "look"]);
    e.ok(&["loop", &t, "--check", "true", "--max", "1"]);
    settle(&e, &t);
    // Without a sandbox here, the grant still denies it, and the round is told.
    let argv = e.argv();
    assert!(argv[0].contains("--disallowedTools Bash(az * delete*)"), "{argv:?}");
    assert!(argv[0].contains("Never run `az * delete*`"), "{argv:?}");
    assert!(argv[0].contains("Bash(true)"), "it may run its check: {argv:?}");
    // A task made from inside a sandbox is in it, whatever it asks.
    let made =
        e.cmd(&["new", "Escape", "--sandbox", "off", "--json"]).env("VARDE_FENCED", "look").output().unwrap();
    let made: Value = serde_json::from_slice(&made.stdout).unwrap();
    let short = made["short"].as_str().unwrap();
    assert_eq!(e.show(short)["flow"]["sandbox"]["name"], "look");
}

#[test]
fn a_recipe_whose_check_command_is_empty_is_refused() {
    let e = repo(":");
    let out = e.cmd(&["new", "Thing", "--recipe", "feature"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has no check command"), "{out:?}");
}

#[test]
fn a_plan_whose_output_is_a_trail_makes_it_once_approved() {
    // The plan writes a trail of two pieces and a merge; each piece
    // writes a file named for its directory.
    let trail = r#"
[[steps]]
id = "left"
check = { cmd = "true" }
[[steps]]
id = "right"
check = { cmd = "true" }
[[steps]]
id = "join"
needs = ["left", "right"]
check = { cmd = "true" }
"#;
    let e = repo(&format!(
        "if [ ! -f \"$FAKE_HOME/trail.made\" ]; then printf '%s' '{trail}' > trail.toml; git add trail.toml; git commit -qm plan; touch \"$FAKE_HOME/trail.made\"; fi"
    ));
    let recipe = e.path("plan.toml");
    std::fs::write(&recipe, "[[steps]]\nid = \"plan\"\noutput = \"trail\"\ncheck = { cmd = \"test -f trail.toml\" }\nloop = { max = 1 }\ngate = \"approved\"\n").unwrap();
    let repo = e.repo();
    let made = e.ok(&[
        "new",
        "Big",
        "--recipe",
        recipe.to_str().unwrap(),
        "--project",
        repo.to_str().unwrap(),
        "--start",
        "--json",
    ]);
    let made: Value = serde_json::from_str(made.lines().next().unwrap()).unwrap();
    let plan = made["tasks"][0]["short"].as_str().unwrap().to_string();
    assert_eq!(settle(&e, &plan), "verified", "{}", e.ok(&["show", &plan]));
    let said = e.ok(&["approve", &plan]);
    assert!(said.contains("its trail is going"), "{said}");
    let board = e.board();
    let next: Vec<String> = all_rows(&board)
        .into_iter()
        .filter(|r| r["title"].as_str().is_some_and(|t| t.starts_with("Big · ")))
        .map(|r| r["short"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(next.len(), 3, "{board}");
    let join = all_rows(&board).into_iter().find(|r| r["title"] == "Big · join").unwrap()["short"]
        .as_str()
        .unwrap()
        .to_string();
    poll(600, || e.show(&join)["flow"]["verified"].as_bool().unwrap_or(false));
    // The pieces started from the plan's branch, each in a worktree of its own.
    let left = all_rows(&e.board()).into_iter().find(|r| r["title"] == "Big · left").unwrap()["short"]
        .as_str()
        .unwrap()
        .to_string();
    let cwd = runs(&e, &left)[0]["cwd"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&cwd).join("trail.toml").exists(), "cut from the plan's branch");
    assert!(e.argv().iter().any(|a| a.contains("Merge the branches")), "{:?}", e.argv());
    // Approving again makes nothing twice.
    assert!(!e.ok(&["approve", &plan]).contains("its trail is going"));
}

#[test]
fn an_agent_verifies_the_work_as_asked_and_its_last_word_decides() {
    // A round writes the work; the verifier passes it once it is there.
    let e = repo("echo done > work.txt; git add work.txt; git commit -qm work");
    e.script(
        "claude",
        &format!(
            "#!/bin/sh\ncase \"$*\" in *'Verify this work'*) echo \"$*\" >> \"$FAKE_HOME/verify.log\"; if [ -f work.txt ]; then echo 'work.txt:1 is there'; echo PASS; else echo FAIL; fi; exit 0;; esac\n{}",
            &agent("echo done > work.txt; git add work.txt; git commit -qm work")["#!/bin/sh\n".len()..]
        ),
    );
    let t = e.new_task("Write the work");
    let out = e.ok(&["check", &t, "--verify", "run it and use /verify-app"]);
    assert!(out.contains("verified by an agent on opus"), "{out}");
    e.ok(&["loop", &t, "--max", "2"]);
    assert_eq!(settle(&e, &t), "verified");
    let last = flow(&e, &t)["last_check"].clone();
    assert_eq!(last["summary"], "verified by review", "{last}");
    // The verifier was asked what the check says, as a session of its own.
    let asked = std::fs::read_to_string(e.path("verify.log")).unwrap();
    assert!(asked.contains("use /verify-app") && asked.contains("--model opus"), "{asked}");
}

#[test]
fn a_check_that_keeps_changing_its_files_stops_rather_than_runs_for_ever() {
    let e = repo(":");
    let t = e.new_task("Restless");
    e.ok(&["loop", &t, "--check", "date +%s%N > stamp.txt", "--max", "3"]);
    assert_eq!(settle(&e, &t), "error");
    let checks = flow(&e, &t)["loop"]["checks"].as_array().unwrap().len();
    assert!(checks <= 3, "{checks} checks");
}

#[test]
fn inside_a_sandbox_a_check_cannot_be_given_a_verifier() {
    let e = repo(":");
    let t = e.new_task("Mine");
    let out =
        e.cmd(&["check", &t, "--verify", "/etc/hostname"]).env("VARDE_FENCED", "default").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not for a session in a sandbox"), "{out:?}");
    assert!(flow(&e, &t)["check"].is_null(), "nothing was set");
}

#[test]
fn a_plans_trail_reads_no_file_outside_its_checkout() {
    // The plan's trail names a file outside its checkout as a rubric.
    let secret = std::env::temp_dir().join(format!("varde-secret-{}", std::process::id()));
    std::fs::write(&secret, "TOPSECRET").unwrap();
    let trail = format!(
        "[[steps]]\nid = \"one\"\ncheck = {{ cmd = \"true\", review = true, rubric = \"{}\" }}\n",
        secret.display()
    );
    let e = repo(&format!(
        "if [ ! -f \"$FAKE_HOME/made\" ]; then printf '%s' '{trail}' > trail.toml; git add trail.toml; git commit -qm plan; touch \"$FAKE_HOME/made\"; fi"
    ));
    let recipe = e.path("plan.toml");
    std::fs::write(&recipe, "[[steps]]\nid = \"plan\"\noutput = \"trail\"\ncheck = { cmd = \"test -f trail.toml\" }\nloop = { max = 1 }\ngate = \"approved\"\n").unwrap();
    let repo_dir = e.repo();
    let made = e.ok(&[
        "new",
        "Big",
        "--recipe",
        recipe.to_str().unwrap(),
        "--project",
        repo_dir.to_str().unwrap(),
        "--start",
        "--json",
    ]);
    let made: Value = serde_json::from_str(made.lines().next().unwrap()).unwrap();
    let plan = made["tasks"][0]["short"].as_str().unwrap().to_string();
    assert_eq!(settle(&e, &plan), "verified");
    let out = e.cmd(&["approve", &plan]).output().unwrap();
    let _ = std::fs::remove_file(&secret);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a file in this checkout"), "{out:?}");
    let board = serde_json::to_string(&e.board()).unwrap();
    assert!(!board.contains("TOPSECRET"));
    let rubrics: Vec<Option<String>> = e
        .db()
        .prepare("SELECT rubric FROM check_def")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(rubrics.iter().all(|r| !r.as_deref().unwrap_or("").contains("TOPSECRET")), "{rubrics:?}");
}

#[test]
fn a_repository_a_sandboxed_session_leaves_is_held_though_it_has_no_check() {
    if !bwrap_works() {
        eprintln!("skipped: no bwrap here");
        return;
    }
    // A whole repository, its objects its own, not the quarantine's.
    let e = repo("(unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES; git init -q lib)");
    std::fs::write(e.path("config.toml"), format!("[sandbox]\nwrite = [\"{}\"]\n", e.path("").display()))
        .unwrap();
    let recipe = e.path("one.toml");
    std::fs::write(&recipe, "[[steps]]\nid = \"only\"\n").unwrap();
    let repo_dir = e.repo();
    let made = e
        .cmd(&[
            "new",
            "No check",
            "--recipe",
            recipe.to_str().unwrap(),
            "--project",
            repo_dir.to_str().unwrap(),
            "--start",
            "--json",
        ])
        .env("VARDE_SANDBOX", "on")
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let made: Value =
        serde_json::from_slice(&made.stdout[..made.stdout.iter().position(|b| *b == b'\n').unwrap()])
            .unwrap();
    let t = made["tasks"][0]["short"].as_str().unwrap().to_string();
    poll(300, || runs(&e, &t).first().is_some_and(|r| r["status"] == "exited"));
    let dir = runs(&e, &t)[0]["cwd"].as_str().unwrap().to_string();
    let lib = std::path::Path::new(&dir).join("lib");
    // Held once its agent has exited, before the run is recorded ended.
    poll(300, || lib.join(".git.varde-held").exists());
    assert!(lib.join(".git.varde-held").exists() && !lib.join(".git").exists(), "held");
}
