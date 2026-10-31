//! The steward (§6.6): its line on the board, and a handed-over task's
//! asks waiting on its answer, driven as Claude drives the hook.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use common::*;

/// A task with a prepared run, and the run's session id.
fn task_with_run(e: &Env) -> (String, String, String) {
    std::fs::create_dir_all(e.path("work")).unwrap();
    let t = e.ok(&["new", "Do the thing", "--project", &e.path("work").to_string_lossy()]);
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--worktree", "never", "--json"])).unwrap();
    let run = plan["run"].as_str().unwrap().to_string();
    let native = e.query(&format!("SELECT native_id FROM run WHERE id = '{run}'"));
    (t, run, native)
}

/// The hook for `event`, started and left to run: a delegated one waits.
fn hook(e: &Env, run: &str, event: &str, payload: &Value) -> Child {
    let mut c = e.cmd(&["hook", event]);
    c.env("VARDE_RUN", run).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    child
}

fn reply(child: Child) -> Value {
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    serde_json::from_slice(&out.stdout).unwrap()
}

fn permission(e: &Env, native: &str, command: &str) -> Value {
    json!({
        "session_id": native,
        "cwd": e.path("work"),
        "hook_event_name": "PermissionRequest",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
}

fn question(e: &Env, native: &str) -> Value {
    json!({
        "session_id": native,
        "cwd": e.path("work"),
        "hook_event_name": "PreToolUse",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{
            "question": "JSON or TOML?", "header": "Format", "multiSelect": false,
            "options": [{"label": "JSON", "description": "j"}, {"label": "TOML", "description": "t"}],
        }]},
    })
}

/// `varde watch`, killed when dropped.
struct Watch(Child, BufReader<std::process::ChildStdout>);

impl Watch {
    fn start(e: &Env) -> Watch {
        Watch::start_with(e, &["watch"])
    }

    fn start_with(e: &Env, args: &[&str]) -> Watch {
        let mut child = e.cmd(args).stdout(Stdio::piped()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let w = Watch(child, out);
        // Until it has said it watches.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !e.home().join("steward.watch").exists() {
            assert!(Instant::now() < deadline, "watch never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        w
    }

    fn line(&mut self) -> Value {
        let mut l = String::new();
        self.1.read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Make `run` the session of the commands this test runs: its process,
/// as varde records it, becomes the test's own, which every `varde` it
/// starts descends from. No other run of the store keeps it.
fn be(e: &Env, run: &str) {
    let me = std::process::id();
    let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap();
    let start: i64 =
        stat[stat.rfind(')').unwrap() + 1..].split_whitespace().nth(19).unwrap().parse().unwrap();
    e.sql(&format!("UPDATE run SET pid = NULL WHERE pid = {me}"));
    e.sql(&format!("UPDATE run SET pid = {me}, pid_start = {start}, exit_code = NULL WHERE id = '{run}'"));
}

fn steward_on(e: &Env) -> String {
    let said = e.ok(&["steward", "on"]);
    said.split_whitespace().nth(5).unwrap().trim_end_matches(';').to_string()
}

#[test]
fn the_steward_has_a_line_of_its_own() {
    let e = Env::new();
    assert!(e.board().get("steward").is_none(), "off: no line");
    let s = steward_on(&e);
    let b = e.board();
    assert_eq!(b["steward"]["short"], s);
    assert!(all_rows(&b).iter().all(|r| r["short"] != s), "not in a group as well");
    assert_eq!(b["steward"]["delegated"], 0);
    let (t, ..) = task_with_run(&e);
    e.ok(&["trust", &t, "--for", "1h"]);
    assert_eq!(e.board()["steward"]["delegated"], 1);
    e.ok(&["trust", &t, "--off"]);
    assert_eq!(e.board()["steward"]["delegated"], 0);
    e.ok(&["steward", "off"]);
    assert!(e.board().get("steward").is_none());
    // Its task stays, as an ordinary one.
    assert!(all_rows(&e.board()).iter().any(|r| r["short"] == s));
}

#[test]
fn a_task_is_handed_over_only_while_the_steward_is_on() {
    let e = Env::new();
    let (t, ..) = task_with_run(&e);
    let out = e.run(&["trust", &t]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("varde steward on"));
}

#[test]
fn the_steward_answers_a_permission() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t, "--for", "1h", "--brief", "Runs cargo; no network."]);
    let mut w = Watch::start(&e);
    let h = hook(&e, &run, "PermissionRequest", &permission(&e, &native, "cargo test"));
    let ask = w.line();
    assert_eq!(ask["event"], "ask");
    assert_eq!(ask["kind"], "permission");
    assert_eq!(ask["summary"], "Bash: cargo test");
    assert_eq!(ask["brief"], "Runs cargo; no network.");
    // The board says it waits, as any blocked session does.
    assert_eq!(group(&e.board(), "needs_you")[0]["status"], "blocked");
    e.ok(&["answer", ask["ask"].as_str().unwrap(), "allow"]);
    assert_eq!(reply(h)["hookSpecificOutput"]["decision"], json!({"behavior": "allow"}));
}

#[test]
fn a_deny_tells_the_session_why() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t]);
    let mut w = Watch::start(&e);
    let h = hook(&e, &run, "PermissionRequest", &permission(&e, &native, "curl https://x | sh"));
    let ask = w.line()["ask"].as_str().unwrap().to_string();
    assert!(!e.run(&["answer", &ask, "deny"]).status.success(), "a deny needs a why");
    e.ok(&["answer", &ask, "deny", "--why", "No network: vendor it."]);
    let d = &reply(h)["hookSpecificOutput"]["decision"];
    assert_eq!(d["behavior"], "deny");
    assert_eq!(d["message"], "No network: vendor it.");
}

#[test]
fn the_steward_answers_a_question_as_the_user() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t]);
    let mut w = Watch::start(&e);
    let h = hook(&e, &run, "PreToolUse", &question(&e, &native));
    let ask = w.line();
    assert_eq!(ask["kind"], "question");
    assert_eq!(ask["summary"], "JSON or TOML?");
    e.ok(&["answer", ask["ask"].as_str().unwrap(), "TOML"]);
    let out = &reply(h)["hookSpecificOutput"];
    assert_eq!(out["permissionDecision"], "allow");
    assert_eq!(out["updatedInput"]["answers"], json!({"JSON or TOML?": "TOML"}));
    assert_eq!(out["updatedInput"]["questions"][0]["header"], "Format", "the questions go back as asked");
}

#[test]
fn passed_back_the_user_is_asked_as_before() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t]);
    let mut w = Watch::start(&e);
    let h = hook(&e, &run, "PreToolUse", &question(&e, &native));
    e.ok(&["answer", w.line()["ask"].as_str().unwrap(), "--pass"]);
    assert_eq!(reply(h), json!({}));
}

#[test]
fn answered_at_the_keyboard_the_hook_stops_waiting() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t]);
    let mut w = Watch::start(&e);
    let h = hook(&e, &run, "PermissionRequest", &permission(&e, &native, "cargo test"));
    w.line();
    // The user said yes in the dialog: the tool ran.
    let ran = json!({"session_id": native, "cwd": e.path("work"), "hook_event_name": "PostToolUse",
                     "tool_name": "Bash"});
    reply(hook(&e, &run, "PostToolUse", &ran));
    assert_eq!(reply(h), json!({}));
    assert_eq!(e.ok(&["asks"]), "nothing waits on the steward");
}

#[test]
fn nothing_waits_where_the_steward_may_not_answer() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    let quick = |command: &str| {
        let started = Instant::now();
        assert_eq!(reply(hook(&e, &run, "PermissionRequest", &permission(&e, &native, command))), json!({}));
        assert!(started.elapsed() < Duration::from_secs(2), "{command} waited");
    };
    // Not handed over.
    quick("cargo test");
    e.ok(&["trust", &t]);
    // Handed over, but no steward watching.
    quick("cargo test");
    let _w = Watch::start(&e);
    // What you keep, whoever watches.
    quick("git push origin main");
    quick("gh pr merge 3");
    assert_eq!(e.ok(&["asks"]), "nothing waits on the steward");
}

#[test]
fn a_handed_over_session_is_told_once() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    let ran = json!({"session_id": native, "cwd": e.path("work"), "hook_event_name": "PostToolUse",
                     "tool_name": "Bash"});
    assert_eq!(reply(hook(&e, &run, "PostToolUse", &ran)), json!({}));
    e.ok(&["trust", &t]);
    let told = reply(hook(&e, &run, "PostToolUse", &ran));
    let context = told["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(context.contains("AskUserQuestion"), "{context}");
    assert_eq!(reply(hook(&e, &run, "PostToolUse", &ran)), json!({}), "once");
}

#[test]
fn the_manual_comes_from_the_binary() {
    let e = Env::new();
    let index = e.ok(&["steward", "guide"]);
    for topic in ["file", "review", "report", "find", "check", "route", "answer", "control"] {
        assert!(index.contains(&format!("  {topic} ")), "{topic} in the index");
        assert!(!e.ok(&["steward", "guide", topic]).is_empty());
    }
    assert!(!e.run(&["steward", "guide", "nope"]).status.success());
}

/// Wait until `task` has `n` runs and the newest has ended.
fn ended(e: &Env, task: &str, n: usize) {
    poll(200, || {
        let runs = runs(e, task);
        runs.len() == n && runs.last().is_some_and(|r| r["status"] == "exited")
    });
}

#[test]
fn say_carries_a_task_on_in_the_background() {
    let e = Env::with_agent(AGENT);
    std::fs::create_dir(e.path("work")).unwrap();
    let t = e.ok(&["new", "Fix the cache", "--project", &e.path("work").to_string_lossy()]);
    // Never run: its first session starts, told the text with its title.
    let said = e.ok(&["say", &t, "--worktree", "never", "Slack: the locale bug is back"]);
    assert_eq!(said, format!("{t} starts in the background; varde attach {t} takes it over"));
    ended(&e, &t, 1);
    let first = e.argv().pop().unwrap();
    assert!(first.starts_with("-p --session-id "), "{first}");
    assert!(first.contains("Fix the cache~~Slack: the locale bug is back"), "{first}");
    // Ended: its conversation resumes with the text as its turn.
    e.ok(&["say", &t, "Also check the de-CH locale."]);
    ended(&e, &t, 2);
    let native = runs(&e, &t)[0]["native_id"].as_str().unwrap().to_string();
    assert_eq!(e.argv().pop().unwrap(), format!("-p --resume {native} -- Also check the de-CH locale."));
    assert!(runs(&e, &t).iter().all(|r| r["background"] == true), "{:?}", runs(&e, &t));
}

/// A task whose session is open in the background, sleeping after its
/// turn; its run, the agent's pid and its session id.
fn open_session(e: &Env) -> (String, String, i64, String) {
    std::fs::create_dir_all(e.path("work")).unwrap();
    let t = e.ok(&["new", "Fix the cache", "--project", &e.path("work").to_string_lossy()]);
    let out = e.cmd(&["say", &t, "--worktree", "never", "Go"]).env("FAKE_SLEEP", "30").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    poll(100, || runs(e, &t).first().is_some_and(|r| r["status"] == "waiting"));
    let run = runs(e, &t)[0]["id"].as_str().unwrap().to_string();
    let pid: i64 = e.query(&format!("SELECT pid FROM run WHERE id = '{run}'")).parse().unwrap();
    let native = e.query(&format!("SELECT native_id FROM run WHERE id = '{run}'"));
    (t, run, pid, native)
}

#[test]
fn say_reaches_an_open_session_with_its_next_hook() {
    let e = Env::with_agent(AGENT);
    let (t, run, pid, native) = open_session(&e);
    // No way in from outside: it waits for the session's next hook.
    let said = e.ok(&["say", &t, "Also check the de-CH locale."]);
    assert_eq!(said, format!("{t}: its open session gets it with its next tool call or prompt"));
    assert_eq!(runs(&e, &t).len(), 1, "no second session");
    let ran = json!({"session_id": native, "cwd": e.path("work"), "hook_event_name": "PostToolUse",
                     "tool_name": "Bash"});
    let got = reply(hook(&e, &run, "PostToolUse", &ran));
    let context = got["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(
        context.contains("from the user, sent through varde:\n\n│ Also check the de-CH locale."),
        "{context}"
    );
    assert_eq!(reply(hook(&e, &run, "PostToolUse", &ran)), json!({}), "once");
    assert_eq!(e.query("SELECT via FROM message WHERE text LIKE 'Also%'"), "hook");
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

#[test]
fn say_pushes_into_an_open_claude_session_and_waits_for_its_answer() {
    use std::io::Read;
    let e = Env::with_agent(AGENT);
    let (t, run, pid, native) = open_session(&e);
    // Claude's registry entry for the session, and its inbox socket.
    let sessions = e.path("claude-config/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let socket = e.path("inbox.sock");
    let inbox = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::fs::write(
        sessions.join(format!("{pid}.json")),
        json!({"pid": pid, "sessionId": native, "messagingSocketPath": socket, "peerProtocol": 1})
            .to_string(),
    )
    .unwrap();
    std::fs::write(sessions.join(format!("{pid}.abc.key")), "sekrit\n").unwrap();
    let wait = e
        .cmd(&["say", &t, "--wait", "Run the tests and say what failed."])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut conn, _) = inbox.accept().unwrap();
    let mut got = String::new();
    conn.read_to_string(&mut got).unwrap();
    let lines: Vec<Value> = got.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0], json!({"type": "auth", "token": "sekrit"}));
    assert_eq!(lines[1]["type"], "user");
    assert_eq!(lines[1]["session_id"], native.as_str());
    assert_eq!(lines[1]["priority"], "next", "after the turn under way; --now cuts in");
    assert!(lines[1]["message"]["content"].as_str().unwrap().ends_with("Run the tests and say what failed."));
    // The session answers, and ends its turn.
    let transcript = e.query(&format!("SELECT transcript FROM run WHERE id = '{run}'"));
    let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
    use std::io::Write;
    writeln!(
        f,
        "{}",
        json!({"type": "user", "message": {"role": "user", "content": "Run the tests and say what failed."}})
    )
    .unwrap();
    writeln!(f, "{}", json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "All 12 pass."}]}})).unwrap();
    let stop = json!({"session_id": native, "cwd": e.path("work"), "transcript_path": transcript,
                      "hook_event_name": "Stop"});
    reply(hook(&e, &run, "Stop", &stop));
    let out = wait.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.starts_with(&format!("{t}: in its open session now")), "{out}");
    assert!(out.contains("All 12 pass."), "{out}");
    assert_eq!(e.query("SELECT via FROM message WHERE text LIKE 'Run%'"), "push");
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

#[test]
fn say_to_a_closed_task_with_no_session_is_a_follow_up() {
    let e = Env::with_agent(AGENT);
    std::fs::create_dir(e.path("work")).unwrap();
    let t = e.ok(&["new", "Fix the cache", "--project", &e.path("work").to_string_lossy()]);
    e.ok(&["say", &t, "--worktree", "never", "Go"]);
    ended(&e, &t, 1);
    e.ok(&["done", &t]);
    let out = e.run(&["say", &t, "More"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("varde new --from"), "{out:?}");
    assert_eq!(runs(&e, &t).len(), 1);
}

#[test]
fn a_rule_answers_without_the_steward() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t, "--allow", "Bash(cargo test*)", "--allow", "Bash(git push*)"]);
    // No steward watching: the rule answers by itself, at once.
    let ask = |command: &str| reply(hook(&e, &run, "PermissionRequest", &permission(&e, &native, command)));
    assert_eq!(ask("cargo test -p varde")["hookSpecificOutput"]["decision"], json!({"behavior": "allow"}));
    assert_eq!(ask("cargo build"), json!({}));
    assert_eq!(ask("cargo test && cargo build"), json!({}), "all of it must be allowed");
    assert_eq!(ask("git push"), json!({}), "what you keep, whatever the rule");
    // A rule alone adds to the hand-over, keeping its time and brief.
    let until = e.query("SELECT until FROM trust");
    assert_eq!(
        e.ok(&["trust", &t, "--allow", "Bash(cargo build*)"]),
        format!("the hook now allows Bash(cargo build*) on {t} by itself")
    );
    assert_eq!(e.query("SELECT until FROM trust"), until);
    assert_eq!(ask("cargo test && cargo build")["hookSpecificOutput"]["decision"]["behavior"], "allow");
    // Kept, as asks the rule settled.
    assert_eq!(e.query("SELECT count(*) FROM ask WHERE by = 'rule'"), "2");
    assert!(!e.run(&["trust", &t, "--allow", "Bash(oops"]).status.success());
}

#[test]
fn the_steward_starts_told_what_it_holds() {
    let e = Env::new();
    let s = steward_on(&e);
    let (t, ..) = task_with_run(&e);
    e.ok(&["trust", &t]);
    let plan: Value = serde_json::from_str(&e.ok(&["start", &s, "--worktree", "never", "--json"])).unwrap();
    let run = plan["run"].as_str().unwrap();
    let native = e.query(&format!("SELECT native_id FROM run WHERE id = '{run}'"));
    let started = json!({"session_id": native, "cwd": e.path("state/steward"), "source": "startup",
                         "hook_event_name": "SessionStart"});
    let told = reply(hook(&e, run, "SessionStart", &started));
    let context = told["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(context.contains("You are the user's varde steward"), "{context}");
    assert!(context.contains(&format!("You hold a task handed over by the user ({t})")), "{context}");
    assert!(context.contains("start varde watch"), "{context}");
}

#[test]
fn what_the_steward_files_needs_a_project_and_waits_in_the_inbox() {
    let e = Env::new();
    let s = steward_on(&e);
    let plan: Value = serde_json::from_str(&e.ok(&["start", &s, "--worktree", "never", "--json"])).unwrap();
    let run = plan["run"].as_str().unwrap();
    std::fs::create_dir_all(e.path("work")).unwrap();
    be(&e, run);
    let new = |args: &[&str]| e.cmd(&[&["new", "Set up CI"], args].concat()).output().unwrap();
    let out = new(&[]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("give --project"), "{out:?}");
    let out = new(&["--project", &e.path("work").to_string_lossy()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(group(&e.board(), "inbox")[0]["title"], "Set up CI");
    // Yours, typed in a shell, is not.
    e.sql(&format!("UPDATE run SET pid = NULL WHERE id = '{run}'"));
    e.ok(&["new", "Mine", "--project", &e.path("work").to_string_lossy()]);
    assert_eq!(group(&e.board(), "open")[0]["title"], "Mine");
}

#[test]
fn say_reopens_a_closed_task_only_when_told() {
    let e = Env::with_agent(AGENT);
    std::fs::create_dir(e.path("work")).unwrap();
    let t = e.ok(&["new", "Fix the cache", "--project", &e.path("work").to_string_lossy()]);
    e.ok(&["say", &t, "--worktree", "never", "Go"]);
    ended(&e, &t, 1);
    e.ok(&["done", &t]);
    assert!(!e.run(&["say", &t, "One more thing."]).status.success());
    e.ok(&["say", &t, "--reopen", "One more thing."]);
    ended(&e, &t, 2);
    assert_eq!(e.show(&t)["state"], "open");
    assert!(e.argv().pop().unwrap().ends_with("-- One more thing."));
}

#[test]
fn said_reads_back_the_latest_turns_and_stop_ends_a_session() {
    let e = Env::with_agent(AGENT);
    let (t, run, pid, _) = open_session(&e);
    let transcript = e.query(&format!("SELECT transcript FROM run WHERE id = '{run}'"));
    let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
    use std::io::Write;
    writeln!(f, "{}", json!({"type": "user", "message": {"role": "user", "content": "Which locale?"}}))
        .unwrap();
    writeln!(f, "{}", json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "de-CH or de-DE?"}]}})).unwrap();
    let said = e.ok(&["said", &t]);
    assert!(said.starts_with(&format!("{t} (waiting):")) && said.contains("de-CH or de-DE?"), "{said}");
    // Stopped, the task stays as it was.
    assert_eq!(e.ok(&["stop", &t]), format!("{t}: stopped 1 session"));
    poll(100, || !std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(e.show(&t)["state"], "open");
}

#[test]
fn the_steward_closes_what_you_could_and_another_agent_spares_your_session() {
    let e = Env::with_agent(AGENT);
    let s = steward_on(&e);
    let plan: Value = serde_json::from_str(&e.ok(&["start", &s, "--worktree", "never", "--json"])).unwrap();
    let steward_run = plan["run"].as_str().unwrap().to_string();
    let (t, run, pid, _) = open_session(&e);
    // A session of yours, not one a schedule or `say` started.
    e.sql(&format!("UPDATE run SET background = 0 WHERE id = '{run}'"));
    // Another agent's shell: your session is spared.
    let out = e.cmd(&["park", &t]).env("CLAUDECODE", "1").env("VARDE_RUN", "nope").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists(), "spared");
    e.ok(&["reopen", &t]);
    // The steward's: it acts for you.
    be(&e, &steward_run);
    let out = e.cmd(&["park", &t]).env("CLAUDECODE", "1").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    poll(100, || !std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn watch_all_says_every_task_that_needs_you() {
    let e = Env::with_agent(AGENT);
    steward_on(&e);
    let mut w = Watch::start_with(&e, &["watch", "--all"]);
    let (t, ..) = open_session(&e);
    let line = w.line();
    assert_eq!((line["event"].as_str(), line["task"].as_str()), (Some("needs-you"), Some(t.as_str())));
    assert_eq!(line["held"], false);
}

#[test]
fn another_tasks_session_may_say_but_not_answer_or_hand_over() {
    let e = Env::with_agent(AGENT);
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    // A second task, whose session runs these commands.
    let (other, other_run, _) = task_with_run(&e);
    let as_other = |args: &[&str]| {
        be(&e, &other_run);
        let out = e.cmd(args).output().unwrap();
        e.sql(&format!("UPDATE run SET pid = NULL WHERE id = '{other_run}'"));
        out
    };
    let out = as_other(&["trust", &t, "--allow", "Bash(*)"]);
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not a task's session"),
        "{out:?}"
    );
    e.ok(&["trust", &t]);
    let mut w = Watch::start(&e);
    let h = hook(&e, &run, "PermissionRequest", &permission(&e, &native, "cargo test"));
    let ask = w.line()["ask"].as_str().unwrap().to_string();
    assert!(!as_other(&["answer", &ask, "allow"]).status.success(), "not in the user's name");
    // Nor by claiming to be the user, unsetting what marks an agent.
    be(&e, &other_run);
    let out =
        e.cmd(&["answer", &ask, "allow"]).env_remove("VARDE_RUN").env_remove("CLAUDECODE").output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    e.sql(&format!("UPDATE run SET pid = NULL WHERE id = '{other_run}'"));
    e.ok(&["answer", &ask, "deny", "--why", "Not now."]);
    reply(h);
    // Saying is fine, and says who it is from.
    let out = as_other(&["say", &t, "Which port do you use?"]);
    assert!(out.status.success(), "{out:?}");
    let sender = e.query("SELECT sender FROM message ORDER BY created DESC LIMIT 1");
    assert_eq!(sender, format!("task {other} (Do the thing), another session varde runs"));
}

#[test]
fn a_task_asked_from_a_session_answers_back_by_itself() {
    let e = Env::with_agent(AGENT);
    // A: the session asking (here the steward's would do as well).
    let (a, a_run, a_native) = task_with_run(&e);
    // B: asked, its session open.
    let (b, b_run, pid, b_native) = open_session(&e);
    be(&e, &a_run);
    let out = e.cmd(&["say", &b, "Run the tests and say what failed."]).output().unwrap();
    e.sql(&format!("UPDATE run SET pid = NULL WHERE id = '{a_run}'"));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.ends_with("its answer comes back here as a message\n"), "{said}");
    // B takes it with its next tool call, answers, and its turn ends.
    let tool = |native: &str, run: &str| {
        let p = json!({"session_id": native, "cwd": e.path("work"), "hook_event_name": "PostToolUse",
                       "tool_name": "Bash"});
        reply(hook(&e, run, "PostToolUse", &p))
    };
    assert!(tool(&b_native, &b_run)["hookSpecificOutput"]["additionalContext"].as_str().is_some());
    let transcript = e.query(&format!("SELECT transcript FROM run WHERE id = '{b_run}'"));
    let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
    use std::io::Write;
    writeln!(f, "{}", json!({"type": "user", "message": {"role": "user", "content": "Run the tests."}}))
        .unwrap();
    writeln!(f, "{}", json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "All 12 pass."}]}})).unwrap();
    let stop = json!({"session_id": b_native, "cwd": e.path("work"), "transcript_path": transcript,
                      "hook_event_name": "Stop"});
    reply(hook(&e, &b_run, "Stop", &stop));
    // Its answer goes back to A, held while A has no session open.
    poll(100, || e.query("SELECT count(*) FROM message WHERE sender LIKE 'task %answering%'") == "1");
    let held = e.query("SELECT task || ' ' || coalesce(delivered, 'held') || ' ' || coalesce(reply_to, 'none') FROM message WHERE sender LIKE 'task %answering%'");
    let a_id = e.query(&format!("SELECT task FROM run WHERE id = '{a_run}'"));
    assert_eq!(held, format!("{a_id} held none"), "held for A, and asks for no reply");
    // A's session gets it with its next hook.
    let got = tool(&a_native, &a_run);
    let context = got["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(
        context.contains(&format!("from task {b} (Fix the cache), answering your varde say")),
        "{context}"
    );
    assert!(context.contains("All 12 pass."), "{context}");
    // Answered once: a second turn end sends nothing more.
    reply(hook(&e, &b_run, "Stop", &stop));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(e.query("SELECT count(*) FROM message WHERE sender LIKE 'task %answering%'"), "1");
    let _ = a;
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

#[test]
fn a_tasks_session_starts_no_session_and_an_answer_asks_for_none() {
    let e = Env::with_agent(AGENT);
    let (a, a_run, _) = task_with_run(&e);
    let (b, b_run, pid, b_native) = open_session(&e);
    // B's session says something to A, which has no session open: held
    // for A's next one, never a session started behind your back.
    let b_pid = e.query(&format!("SELECT pid FROM run WHERE id = '{b_run}'"));
    be(&e, &b_run);
    let out = e.cmd(&["say", &a, "FYI: the cache is fixed."]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{a} has no session open: it gets this when it next runs\n")
    );
    assert_eq!(runs(&e, &a).len(), 1, "no session started");
    // A asks B; B, answering, says it back: that answer asks for none.
    be(&e, &a_run);
    e.cmd(&["say", &b, "Which locale?"]).output().unwrap();
    e.sql(&format!("UPDATE run SET pid = {b_pid} WHERE id = '{b_run}'"));
    let ran = json!({"session_id": b_native, "cwd": e.path("work"), "hook_event_name": "PostToolUse", "tool_name": "Bash"});
    let got = reply(hook(&e, &b_run, "PostToolUse", &ran));
    let context = got["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(context.contains("answer here, and do not varde say it back"), "{context}");
    be(&e, &b_run);
    e.cmd(&["say", &a, "de-CH."]).output().unwrap();
    assert_eq!(e.query("SELECT coalesce(reply_to, 'none') FROM message WHERE text = 'de-CH.'"), "none");
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

#[test]
fn taken_back_a_waiting_hook_lets_go_and_a_gone_ask_is_settled() {
    let e = Env::new();
    steward_on(&e);
    let (t, run, native) = task_with_run(&e);
    e.ok(&["trust", &t]);
    let mut w = Watch::start(&e);
    let started = Instant::now();
    let h = hook(&e, &run, "PreToolUse", &question(&e, &native));
    w.line();
    // You are back: T on the board, or this.
    e.ok(&["trust", &t, "--off"]);
    assert_eq!(reply(h), json!({}), "the question is the user's again");
    assert!(started.elapsed() < Duration::from_secs(5));
    // An ask whose session went on is settled, whoever made it.
    e.ok(&["trust", &t]);
    let h = hook(&e, &run, "PermissionRequest", &permission(&e, &native, "cargo test"));
    w.line();
    let ran = json!({"session_id": native, "cwd": e.path("work"), "hook_event_name": "PostToolUse", "tool_name": "Bash"});
    reply(hook(&e, &run, "PostToolUse", &ran));
    reply(h);
    assert_eq!(e.ok(&["asks"]), "nothing waits on the steward");
}

#[test]
fn turned_on_again_the_steward_is_the_one_it_was() {
    let e = Env::new();
    let first = steward_on(&e);
    e.ok(&["steward", "off"]);
    assert_eq!(steward_on(&e), first, "with its memory of you");
}

#[test]
fn a_push_queued_behind_a_turn_is_answered_by_the_turn_that_takes_it() {
    let e = Env::with_agent(AGENT);
    let (_, a_run, _) = task_with_run(&e);
    let (b, b_run, pid, b_native) = open_session(&e);
    let sessions = e.path("claude-config/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let socket = e.path("inbox.sock");
    let inbox = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::fs::write(
        sessions.join(format!("{pid}.json")),
        json!({"pid": pid, "sessionId": native_of(&e, &b_run), "messagingSocketPath": socket, "peerProtocol": 1})
            .to_string(),
    )
    .unwrap();
    let drain = std::thread::spawn(move || {
        let (mut c, _) = inbox.accept().unwrap();
        std::io::Read::read_to_string(&mut c, &mut String::new()).unwrap();
    });
    be(&e, &a_run);
    e.ok(&["say", &b, "Which locale fails?"]);
    e.sql(&format!("UPDATE run SET pid = NULL WHERE id = '{a_run}'"));
    drain.join().unwrap();
    let transcript = e.query(&format!("SELECT transcript FROM run WHERE id = '{b_run}'"));
    let stop = json!({"session_id": b_native, "cwd": e.path("work"), "transcript_path": transcript, "hook_event_name": "Stop"});
    use std::io::Write;
    let append = |lines: &[Value]| {
        let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    };
    // The turn it was queued behind ends: no answer from that one.
    append(&[
        json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Earlier work done."}]}}),
    ]);
    reply(hook(&e, &b_run, "Stop", &stop));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(e.query("SELECT count(*) FROM message WHERE sender LIKE '%answering%'"), "0");
    // The turn that takes it.
    append(&[
        json!({"type": "user", "isMeta": true, "origin": {"kind": "peer"}, "message": {"role": "user", "content": "│ Which locale fails?"}}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "de-CH."}]}}),
    ]);
    reply(hook(&e, &b_run, "Stop", &stop));
    poll(100, || e.query("SELECT count(*) FROM message WHERE sender LIKE '%answering%'") == "1");
    let text = e.query("SELECT text FROM message WHERE sender LIKE '%answering%'");
    assert!(text.contains("de-CH.") && !text.contains("Earlier work done."), "{text}");
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

fn native_of(e: &Env, run: &str) -> String {
    e.query(&format!("SELECT native_id FROM run WHERE id = '{run}'"))
}
