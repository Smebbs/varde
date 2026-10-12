//! `varde run` in a terminal of its own, and in tmux on a server of the
//! test's own, with a fake agent that reports what it was given.

mod common;

use std::process::Command;

use serde_json::Value;

use common::*;

/// Records the signals it starts with ignored, then waits for `$FAKE_GO`
/// to appear if asked to.
const FAKE: &str = r#"#!/bin/sh
grep SigIgn /proc/self/status | cut -f2 > "$FAKE_HOME/sigign"
if [ -n "$FAKE_GO" ]; then
  while [ ! -e "$FAKE_GO" ]; do sleep 0.05; done
fi
exit 0
"#;

fn env() -> Env {
    let e = Env::with_agent(FAKE);
    std::fs::create_dir(e.path("work")).unwrap();
    e
}

/// A run of a new task, prepared and not yet launched.
fn prepared(e: &Env) -> (String, String) {
    let t = e.ok(&["new", "Try it", "--project", &e.path("work").to_string_lossy()]);
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--worktree", "never", "--json"])).unwrap();
    (t, plan["run"].as_str().unwrap().to_string())
}

fn sigign(e: &Env) -> u64 {
    let s = std::fs::read_to_string(e.path("sigign")).unwrap();
    u64::from_str_radix(s.trim(), 16).unwrap()
}

const HUP_INT_QUIT: u64 = 0b111;

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "reads /proc/self/status")]
fn the_agent_gets_the_terminals_signals_even_when_varde_ignored_them() {
    let e = env();
    let (_, run) = prepared(&e);
    // As in the board, which ran a session before this one.
    let status = e
        .env(&mut Command::new("sh"))
        .arg("-c")
        .arg(format!("trap '' INT QUIT HUP; exec '{}' run {run}", env!("CARGO_BIN_EXE_varde")))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(sigign(&e) & HUP_INT_QUIT, 0, "SigIgn {:x}", sigign(&e));
}

#[test]
fn the_board_without_a_terminal_says_so_and_fails() {
    let e = env();
    let out = e.cmd(&[]).stdin(std::process::Stdio::null()).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("needs a terminal") && !err.contains("panicked"), "{err}");
}

/// `varde run` as typed into a shell, waiting for `go` to end.
fn typed(e: &Env, run: &str, extra: &str) -> String {
    e.typed(&format!(
        "FAKE_GO='{}' {extra} '{}' run {run}",
        e.path("go").display(),
        env!("CARGO_BIN_EXE_varde")
    ))
}

fn wait(what: &str, done: impl FnMut() -> bool) {
    assert!(poll(100, done), "{what} never happened");
}

#[test]
fn a_pane_of_your_own_is_the_tasks_only_while_its_session_runs() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    let (task, run) = prepared(&e);
    t.type_in(&pane, &typed(&e, &run, ""));
    wait("the tag", || t.tag(&pane).to_ascii_lowercase().ends_with(&task));
    std::fs::write(e.path("go"), "").unwrap();
    wait("the untag", || t.tag(&pane).is_empty());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "finds the board in /proc")]
fn sigterm_ends_the_board_with_the_terminal_given_back() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    t.type_in(&pane, &format!("{}; echo board-ended-$?", e.typed(env!("CARGO_BIN_EXE_varde"))));
    let alternate = || t.tmux(&["display", "-p", "-t", &pane, "#{alternate_on}"]).unwrap();
    wait("the board", || alternate() == "1");
    // The board is below the pane's shell: tmux before 3.4 runs that
    // shell under another (`sh -c sh`).
    let mut board = t.tmux(&["display", "-p", "-t", &pane, "#{pane_pid}"]).unwrap();
    while std::fs::read_to_string(format!("/proc/{board}/comm")).unwrap().trim() != "varde" {
        let children = std::fs::read_to_string(format!("/proc/{board}/task/{board}/children")).unwrap();
        board = children.split_whitespace().next().expect("the board's process").to_string();
    }
    assert!(Command::new("kill").args(["-TERM", &board]).status().unwrap().success());
    wait("the board's end", || {
        t.tmux(&["capture-pane", "-p", "-t", &pane]).unwrap().contains("board-ended-0")
    });
    assert_eq!(alternate(), "0");
}

#[test]
fn an_edit_that_changes_nothing_says_so_on_the_board() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    prepared(&e);
    t.type_in(&pane, &e.typed(&format!("VISUAL=true EDITOR=true {}", env!("CARGO_BIN_EXE_varde"))));
    let shown = || t.tmux(&["capture-pane", "-p", "-t", &pane]).unwrap();
    wait("the board", || shown().contains("Try it"));
    for key in ["e", "E"] {
        t.tmux(&["send-keys", "-t", &pane, key]).unwrap();
        wait("the message", || shown().contains("cairn unchanged"));
        assert!(!shown().contains("saved"), "{}", shown());
        t.tmux(&["send-keys", "-t", &pane, "g"]).unwrap();
        wait("the message to go", || !shown().contains("cairn unchanged"));
    }
    t.tmux(&["send-keys", "-t", &pane, "q"]).unwrap();
}

#[test]
fn a_new_task_can_be_written_in_the_editor() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    prepared(&e);
    // An editor that keeps the hint and the title typed, and writes the rest.
    let editor = e.path("editor");
    std::fs::write(
        &editor,
        "#!/bin/sh\nsed -i 's/^Cache$/Cache by locale/' \"$1\"\nprintf 'Start in cache.rs\\nthen the tests\\n' >> \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    t.type_in(&pane, &e.typed(&format!("VISUAL={} {}", editor.display(), env!("CARGO_BIN_EXE_varde"))));
    let shown = || t.tmux(&["capture-pane", "-p", "-t", &pane]).unwrap();
    wait("the board", || shown().contains("Try it"));
    t.tmux(&["send-keys", "-t", &pane, "n"]).unwrap();
    t.tmux(&["send-keys", "-t", &pane, "-l", "Cache"]).unwrap();
    t.tmux(&["send-keys", "-t", &pane, "C-g"]).unwrap();
    wait("the form again", || shown().contains("Title: Cache by locale"));
    t.tmux(&["send-keys", "-t", &pane, "C-s"]).unwrap();
    wait("the project", || shown().contains("Project for this task"));
    t.tmux(&["send-keys", "-t", &pane, "Enter"]).unwrap();
    wait("the task", || shown().contains("created "));
    t.tmux(&["send-keys", "-t", &pane, "q"]).unwrap();
    assert_eq!(
        e.query("SELECT body FROM task WHERE title = 'Cache by locale'"),
        "Start in cache.rs\nthen the tests"
    );
}

#[test]
fn a_flood_of_keys_is_not_a_refresh_a_key() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    e.ok(&["list"]);
    let tasks: String = (0..1000)
        .map(|i| format!("INSERT INTO task (id, title, created) VALUES ('01FLOOD{i:019}', 'Task {i}', {i});"))
        .collect();
    e.sql(&tasks);
    t.type_in(&pane, &e.typed(env!("CARGO_BIN_EXE_varde")));
    let shown = || t.tmux(&["capture-pane", "-p", "-t", &pane]).unwrap();
    wait("the board", || shown().contains("Task 999"));
    // Read again for each key, the board took seconds over these.
    let js = "j".repeat(200);
    for _ in 0..10 {
        t.tmux(&["send-keys", "-t", &pane, "-l", &js]).unwrap();
    }
    t.tmux(&["send-keys", "-t", &pane, "?"]).unwrap();
    wait("the keys", || shown().contains("varde keys"));
    t.tmux(&["send-keys", "-t", &pane, "Escape", "q"]).unwrap();
}

#[test]
fn the_pane_is_found_by_its_terminal_not_an_inherited_tmux_pane() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let first = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    let other = t.tmux(&["new-window", "-d", "-P", "-F", "#{pane_id}", "sh"]).unwrap();
    let (_, run) = prepared(&e);
    // As in a terminal of an Emacs that was started in `first`.
    t.type_in(&other, &typed(&e, &run, &format!("TMUX_PANE={first}")));
    wait("the tag", || !t.tag(&other).is_empty());
    assert_eq!(t.tag(&first), "");
    std::fs::write(e.path("go"), "").unwrap();
    wait("the untag", || t.tag(&other).is_empty());
}

#[test]
fn attach_on_a_live_task_names_its_tmux_window() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    let (task, run) = prepared(&e);
    t.type_in(&pane, &typed(&e, &run, ""));
    wait("the tag", || t.tag(&pane).to_ascii_lowercase().ends_with(&task));
    // As from another pane of that tmux; with no client to switch, it says where.
    let out = e
        .cmd(&["attach", &task])
        .env("TMUX", format!("{},1,0", t.sock.display()))
        .env("EDITOR", "false")
        .env("VISUAL", "false")
        .env_remove("VARDE_TMUX")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(said.contains("already running in tmux window t:0"), "{said}");
    std::fs::write(e.path("go"), "").unwrap();
    wait("the untag", || t.tag(&pane).is_empty());
}
