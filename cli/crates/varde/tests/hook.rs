//! The hook, driven as the agent CLI drives it: a process with a payload
//! on stdin. Whatever goes wrong, it prints `{}`, exits 0 and loses
//! nothing it could have kept.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;
use varde_agent::Agent as _;

use common::*;

fn hook(e: &Env, event: &str, payload: &str) {
    let out = feed(&mut e.cmd(&["hook", event]), payload);
    assert!(out.status.success(), "hook exited {:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
}

fn rows(board: &Value, key: &str) -> Vec<String> {
    group(board, key).iter().map(|r| r["title"].as_str().unwrap().to_string()).collect()
}

fn replay_a_turn(e: &Env) {
    let out = feed(&mut e.cmd(&["hook", "SessionStart"]), &fixture("session-start-startup.json"));
    assert!(out.status.success());
    // A session is told how to record what it learns and search earlier work.
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let context = answer["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(context.contains("varde note --run ") && context.contains("varde recall"));
    assert!(context.contains("varde new --from "));
    // A session with no task yet has nothing to close.
    assert!(!context.contains("varde done"), "{context}");
    hook(e, "UserPromptSubmit", &fixture("user-prompt-submit.json"));
    hook(e, "PostToolUse", &fixture("post-tool-use.json"));
    hook(e, "Stop", &fixture("stop.json"));
}

#[test]
fn a_real_turn_lands_under_needs_you() {
    let e = Env::new();
    replay_a_turn(&e);
    let b = e.board();
    assert_eq!(rows(&b, "needs_you"), ["Run ls once with the Bash tool…"]);
    hook(&e, "SessionEnd", &fixture("session-end.json"));
    assert_eq!(rows(&e.board(), "inbox"), ["Run ls once with the Bash tool…"]);
}

#[test]
fn a_turn_that_ends_on_an_api_error_needs_you() {
    let e = Env::new();
    hook(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"));
    hook(&e, "StopFailure", &fixture("stop.json").replace("\"Stop\"", "\"StopFailure\""));
    assert_eq!(rows(&e.board(), "needs_you"), ["Run ls once with the Bash tool…"]);
    // Every event the plugin subscribes to is one varde reads.
    let plugin =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugin/hooks/hooks.json"));
    let plugin: Value = serde_json::from_str(&plugin.unwrap()).unwrap();
    for event in plugin["hooks"].as_object().unwrap().keys() {
        let p = serde_json::json!({"session_id": "s", "notification_type": "idle_prompt"});
        assert!(varde_agent::Claude.normalize(event, &p).is_some(), "{event}");
    }
}

#[test]
fn malformed_input_is_logged_not_fatal() {
    // Before anything made the store's directory: the first hook ever.
    let e = Env::new();
    assert!(!e.home().exists());
    hook(&e, "Stop", "not json {");
    hook(&e, "Stop", "");
    hook(&e, "Stop", "[1,2,3]");
    let log = std::fs::read_to_string(e.home().join("varde.log")).unwrap();
    assert!(log.contains("not JSON"));
}

#[test]
fn a_hook_answers_whatever_its_command_line() {
    let e = Env::new();
    for args in
        [&["hook"][..], &["hook", "--bogus", "Stop"], &["hook", "--agent"], &["hook", "--agent=pi", "x"]]
    {
        let out = feed(&mut e.cmd(args), &fixture("stop.json"));
        assert!(out.status.success(), "{args:?} exited {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n", "{args:?}");
    }
    // An unknown flag does not keep the event from being recorded.
    hook(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"));
    let out = feed(&mut e.cmd(&["hook", "--new-flag", "Stop"]), &fixture("stop.json"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
    assert_eq!(rows(&e.board(), "needs_you"), ["Run ls once with the Bash tool…"]);
}

#[test]
fn a_pi_turn_the_user_stopped_is_not_news() {
    // A session varde did not launch, so no transcript watcher: pi says so
    // itself (`agent_settled` with `aborted`).
    let e = Env::new();
    let p =
        |extra: &str| format!(r#"{{"session_id":"s1","session_file":"/p/s1.jsonl","cwd":"/repo"{extra}}}"#);
    for (event, extra) in [
        ("session_start", r#","reason":"startup""#),
        ("before_agent_start", r#","prompt":"Why is the page in English?""#),
        ("agent_settled", r#","aborted":true"#),
    ] {
        assert!(feed(&mut e.cmd(&["hook", "--agent", "pi", event]), &p(extra)).status.success());
    }
    assert_eq!(e.query("SELECT kind FROM event ORDER BY seq DESC LIMIT 1"), "interrupted");
    // At its prompt, but you were there.
    let board = e.board();
    let row = &group(&board, "needs_you")[0];
    assert_eq!((row["status"].as_str(), row["unread"].as_bool()), (Some("waiting"), Some(false)), "{row}");
}

#[test]
fn a_scripted_session_is_not_tracked() {
    // `claude -p` from a script, or a program on the Agent SDK.
    let e = Env::new();
    for event in ["SessionStart", "UserPromptSubmit"] {
        let file =
            if event == "SessionStart" { "session-start-startup.json" } else { "user-prompt-submit.json" };
        let out = feed(e.cmd(&["hook", event]).env("CLAUDE_CODE_ENTRYPOINT", "sdk-cli"), &fixture(file));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
    }
    assert!(!e.home().join("varde.db").exists());
}

/// Run `varde hook <event>` the way an agent varde did not launch does,
/// from a process of its own: `timeout`, under the name `claude`, stands
/// in for the agent. It runs the hook through a shell, behind `wrap`;
/// `then` is what it does after the hook returns.
fn hook_from(e: &Env, event: &str, payload: &str, wrap: &str, then: &str) -> std::process::Child {
    let file = e.path(&format!("{event}.json"));
    std::fs::write(&file, payload).unwrap();
    let agent = e.path("claude");
    if !agent.exists() {
        let timeout = Command::new("sh").args(["-c", "command -v timeout"]).output().unwrap();
        let timeout = String::from_utf8_lossy(&timeout.stdout).trim().to_string();
        std::os::unix::fs::symlink(timeout, &agent).unwrap();
    }
    e.env(&mut Command::new(agent))
        .args(["30", "sh", "-c"])
        .arg(format!("{wrap} \"$0\" hook {event} < \"$1\" > /dev/null; {then}"))
        .arg(env!("CARGO_BIN_EXE_varde"))
        .arg(&file)
        .spawn()
        .unwrap()
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "names GNU timeout claude, which macOS names by its file")]
fn a_session_varde_did_not_launch_ends_with_its_process() {
    let e = Env::new();
    for (event, file) in
        [("SessionStart", "session-start-startup.json"), ("UserPromptSubmit", "user-prompt-submit.json")]
    {
        assert!(hook_from(&e, event, &fixture(file), "", "true").wait().unwrap().success());
    }
    // It went without a SessionEnd: a closed terminal, a crash, a reboot.
    assert_eq!(rows(&e.board(), "working"), Vec::<String>::new());
    assert_eq!(rows(&e.board(), "inbox"), ["Run ls once with the Bash tool…"]);
    let task = e.board()["groups"][4]["rows"][0]["task"].as_str().unwrap().to_string();
    let show: Value = serde_json::from_slice(&e.run(&["show", &task, "--json", "--no-git"]).stdout).unwrap();
    assert_eq!(show["runs"][0]["status"], "exited");
    assert_eq!(show["runs"][0]["launch"], "adopted");
    assert_eq!(show["runs"][0]["why"]["alive"], false);
}

#[test]
fn done_leaves_a_session_varde_did_not_launch_running() {
    let e = Env::new();
    e.board(); // creates the store
    let mut agent = hook_from(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"), "", "sleep 30");
    let mut task = String::new();
    let working = poll(100, || {
        task = e.board()["groups"][1]["rows"][0]["task"].as_str().unwrap_or_default().to_string();
        !task.is_empty()
    });
    assert!(working, "the session never showed as working");
    let out = e.run(&["done", &task]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(agent.try_wait().unwrap().is_none(), "done stopped a session it did not start");
    let _ = agent.kill();
    let _ = agent.wait();
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "uses GNU timeout")]
fn a_wrapper_around_the_hook_is_not_taken_for_the_agent() {
    let e = Env::new();
    let mut agent =
        hook_from(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"), "timeout 5", "sleep 30");
    let mut working = vec![];
    poll(100, || {
        working = rows(&e.board(), "working");
        !working.is_empty()
    });
    // The wrapper has long exited; the agent is still there.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(rows(&e.board(), "working"), working);
    assert_eq!(working.len(), 1);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(rows(&e.board(), "working"), Vec::<String>::new());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "uses GNU timeout")]
fn a_session_whose_first_prompt_waited_in_pending_still_ends_with_its_process() {
    let e = Env::new();
    e.board();
    let db = lock_store(e.home().join("varde.db"));
    let mut agent = hook_from(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"), "", "sleep 30");
    poll(100, || e.home().join("pending.jsonl").exists());
    drop(db);
    poll(100, || !rows(&e.board(), "working").is_empty());
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(rows(&e.board(), "working"), Vec::<String>::new());
}

#[test]
fn internal_processes_are_not_tracked() {
    let e = Env::new();
    let out = feed(
        e.cmd(&["hook", "UserPromptSubmit"]).env("VARDE_INTERNAL", "1"),
        &fixture("user-prompt-submit.json"),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
    assert!(!e.home().join("varde.db").exists());
}

#[test]
fn a_locked_store_defers_to_pending_and_loses_nothing() {
    let e = Env::new();
    assert!(feed(&mut e.cmd(&["hook", "SessionStart"]), &fixture("session-start-startup.json"))
        .status
        .success());

    // Hold the write lock the way a stuck process would.
    let db = lock_store(e.home().join("varde.db"));
    hook(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"));
    hook(&e, "Stop", &fixture("stop.json"));
    assert!(e.home().join("pending.jsonl").exists());
    drop(db);

    // The next command applies them, in order.
    assert_eq!(rows(&e.board(), "needs_you"), ["Run ls once with the Bash tool…"]);
    assert!(!e.home().join("pending.jsonl").exists());
}

#[test]
fn an_unwritable_home_still_answers() {
    let e = Env::new();
    let file = e.path("not-a-dir");
    std::fs::write(&file, "").unwrap();
    let out = feed(
        e.cmd(&["hook", "UserPromptSubmit"]).env("VARDE_HOME", &file),
        &fixture("user-prompt-submit.json"),
    );
    assert!(out.status.success(), "hook exited {:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
}

/// Open the store in another process and hold an exclusive transaction
/// until the returned guard is dropped.
fn lock_store(db: PathBuf) -> LockGuard {
    let (held, locked) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        held.send(()).unwrap();
        let _ = wait.recv();
        conn.execute_batch("ROLLBACK").unwrap();
    });
    locked.recv().unwrap();
    LockGuard { release: Some(release), thread: Some(thread) }
}

struct LockGuard {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What `varde doctor` says of its check `name`.
fn doctor(e: &Env, name: &str) -> Value {
    let json: Value = serde_json::from_slice(&e.run(&["doctor", "--json"]).stdout).unwrap();
    json["checks"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap().clone()
}

fn pending_line(session: &str, event: &str, extra: &str) -> String {
    format!(
        "{{\"agent\":\"claude\",\"event\":\"{event}\",\"ts\":1,\"claim\":null,\"payload\":{{\"session_id\":\"{session}\",\"cwd\":\"/tmp/p\",\"prompt\":\"work {extra}\"}}}}\n"
    )
}

#[test]
fn an_event_that_cannot_be_applied_is_set_aside_not_blocking() {
    let e = Env::new();
    e.board(); // creates the store
    e.sql(
        "CREATE TRIGGER poison BEFORE INSERT ON event WHEN NEW.raw LIKE '%poison%'
                      BEGIN SELECT RAISE(ABORT, 'poisoned'); END;",
    );
    let mut lines = pending_line("s1", "UserPromptSubmit", "poison");
    lines += &pending_line("s2", "UserPromptSubmit", "fine");
    std::fs::write(e.home().join("pending.jsonl"), lines).unwrap();
    // The good event lands; the bad one is kept aside; commands keep working.
    assert_eq!(rows(&e.board(), "working"), ["work fine"]);
    let failed = std::fs::read_to_string(e.home().join("pending.failed.jsonl")).unwrap();
    assert!(failed.contains("poisoned") && failed.contains("s1"));
    let doctor = e.run(&["doctor"]);
    assert!(!doctor.status.success());
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("1 could not be applied"));
}

#[test]
fn a_torn_pending_write_costs_its_line_not_the_file() {
    let e = Env::new();
    e.board();
    // Bytes that are not UTF-8, as a write torn by a crash leaves them,
    // between two good events.
    let mut bytes = pending_line("s1", "UserPromptSubmit", "one").into_bytes();
    bytes.extend_from_slice(b"{\"agent\":\"cla\xff\xfe\n");
    bytes.extend(pending_line("s2", "UserPromptSubmit", "two").into_bytes());
    std::fs::write(e.home().join("pending.jsonl"), bytes).unwrap();
    let mut working = rows(&e.board(), "working");
    working.sort();
    assert_eq!(working, ["work one", "work two"]);
    assert!(!e.home().join("pending.jsonl").exists());
    assert!(
        doctor(&e, "pending")["detail"].as_str().unwrap_or_default().contains("1 could not be applied"),
        "{}",
        doctor(&e, "pending")
    );
}

#[test]
fn concurrent_drains_apply_each_event_once() {
    let e = Env::new();
    e.board();
    let lines: String = (0..400).map(|i| pending_line(&format!("s{i}"), "UserPromptSubmit", "x")).collect();
    std::fs::write(e.home().join("pending.jsonl"), lines).unwrap();
    let children: Vec<_> =
        (0..4).map(|_| e.cmd(&["list", "--json"]).stdout(Stdio::null()).spawn().unwrap()).collect();
    for mut c in children {
        c.wait().unwrap();
    }
    e.board();
    assert_eq!(e.query("SELECT count(*) FROM event"), "400");
}

#[test]
fn doctor_names_what_failed_and_how_to_fix_it() {
    let e = Env::new();
    hook(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"));
    e.sql(
        "INSERT INTO worktree (path, task, repo, branch, base, state, created)
         SELECT '/nonexistent/wt', id, '/nonexistent', 'varde/x', 'main', 'active', 1 FROM task",
    );
    let out = e.run(&["doctor"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("!!  worktrees") && text.contains("/nonexistent/wt"), "{text}");
    assert!(text.contains("fix: varde start <task>"), "{text}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("worktrees"));
    let wt = doctor(&e, "worktrees");
    assert_eq!(wt["ok"], false);
    assert!(wt["fix"].is_string());
}

#[test]
fn doctor_lets_a_cairn_update_given_up_on_long_ago_rest() {
    let e = Env::new();
    hook(&e, "UserPromptSubmit", &fixture("user-prompt-submit.json"));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let memory = || doctor(&e, "memory");
    e.sql(&format!(
        "INSERT INTO job (id, run, upto, state, error, created, finished, attempts)
             SELECT 'J1', id, 10, 'failed', 'the model call failed', 1, {}, 3 FROM run",
        now - 8 * 24 * 3600 * 1000
    ));
    assert_eq!(memory()["ok"], true, "{}", memory());
    e.sql(&format!("UPDATE job SET finished = {now}"));
    let m = memory();
    assert_eq!(m["ok"], false);
    // Given up on: nothing retries it, and the advice does not say so.
    let fix = m["fix"].as_str().unwrap();
    assert!(fix.contains("claude -p") && !fix.contains("retries"), "{fix}");
    // The advice names the agent that made the update.
    e.sql("UPDATE run SET agent = 'pi'");
    assert!(memory()["fix"].as_str().unwrap().contains("try `pi -p hello`"), "{}", memory());
}

#[test]
fn doctor_finds_hooks_only_where_the_agents_load_them() {
    let e = Env::new();
    let bin = e.path("bin");
    std::fs::create_dir(&bin).unwrap();
    e.script("bin/claude", "#!/bin/sh\n");
    e.script("bin/pi", "#!/bin/sh\n");
    let (cc, pi) = (e.path("cc"), e.path("pi-agent"));
    let check = |name: &str| -> Value {
        let out = e
            .cmd(&["doctor", "--json"])
            .env("PATH", &bin)
            .env("HOME", e.path("home"))
            .env("CLAUDE_CONFIG_DIR", &cc)
            .env("PI_CODING_AGENT_DIR", &pi)
            .env_remove("VARDE_CLAUDE")
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&out.stdout).unwrap();
        json["checks"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap().clone()
    };
    let write = |p: PathBuf, text: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };

    // A plugin copy left in the cache is not an installed plugin.
    let copy = cc.join("plugins/cache/m/varde/0.1.0");
    write(copy.join("hooks/hooks.json"), r#"{"hooks": {"Stop": [{"command": "varde hook Stop"}]}}"#);
    assert_eq!(check("claude")["ok"], false);
    let installed =
        format!(r#"{{"version": 2, "plugins": {{"varde@m": [{{"installPath": "{}"}}]}}}}"#, copy.display());
    write(cc.join("plugins/installed_plugins.json"), &installed);
    // A copy an older varde installed, without the events added since.
    let older = check("claude");
    assert_eq!(older["ok"], false);
    let detail = older["detail"].as_str().unwrap();
    assert!(
        detail.contains("has no hook for") && detail.contains("SessionStart") && !detail.contains("Stop,"),
        "{older}"
    );
    // Named as it was installed: from marketplace `m`.
    assert!(
        older["fix"].as_str().unwrap().contains("marketplace update m && claude plugin update varde@m"),
        "{older}"
    );
    let shipped = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugin/hooks/hooks.json");
    write(copy.join("hooks/hooks.json"), &std::fs::read_to_string(shipped).unwrap());
    assert_eq!(check("claude")["ok"], true, "{}", check("claude"));
    write(cc.join("settings.json"), r#"{"enabledPlugins": {"varde@m": false}}"#);
    assert_eq!(check("claude")["ok"], false);

    // pi's directory is `$PI_CODING_AGENT_DIR`, which may not have an
    // extensions directory yet.
    write(e.path("home/.pi/agent/extensions/varde.ts"), r#"spawnSync("varde", ["hook"])"#);
    let missing = check("pi");
    assert_eq!(missing["ok"], false);
    let fix = missing["fix"].as_str().unwrap();
    assert!(fix.starts_with(&format!("mkdir -p {}/extensions && ln -s ", pi.display())), "{fix}");
    // An extension its settings list counts.
    write(pi.join("settings.json"), r#"{"extensions": ["-builtin:mcp", "~/.pi/agent/extensions/varde.ts"]}"#);
    assert_eq!(check("pi")["ok"], true, "{}", check("pi"));

    // No config file: the defaults, not a path that is not there.
    assert_eq!(check("config")["detail"], "none; the defaults");
    std::fs::write(
        e.path("config.toml"),
        "[agents.claude]\ncommand = [\"claude\", \"--model\", \"haiku\"]\n",
    )
    .unwrap();
    assert_eq!(check("agent")["detail"], format!("claude runs {}/claude --model haiku", bin.display()));
}

#[test]
fn doctor_checks_the_agent_a_new_conversation_uses() {
    let e = Env::new();
    let fake = e.script("claude", "#!/bin/sh\n");
    let check = |agent: &str| -> Value {
        std::fs::write(e.path("config.toml"), format!("agent = \"{agent}\"\n")).unwrap();
        doctor(&e, "agent")
    };
    assert_eq!(check("claude")["ok"], true);
    let typo = check("claud");
    assert!(typo["ok"] == false && typo["detail"].as_str().unwrap().contains("unknown agent"), "{typo}");
    let missing = check("pi");
    assert!(
        missing["ok"] == false && missing["detail"].as_str().unwrap().contains("not on PATH"),
        "{missing}"
    );
    // A file that cannot run is not an agent: PATH would find another.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o644)).unwrap();
    let noexec = check("claude");
    assert!(
        noexec["ok"] == false && noexec["detail"].as_str().unwrap().contains("not executable"),
        "{noexec}"
    );
}
