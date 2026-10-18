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
fn list_keeps_to_a_project_when_asked() {
    let e = env();
    std::fs::create_dir(e.path("other")).unwrap();
    e.ok(&["new", "Here", "--project", &e.path("work").to_string_lossy()]);
    e.ok(&["new", "There", "--project", &e.path("other").to_string_lossy()]);
    let all = e.ok(&["list"]);
    assert!(all.contains("Here") && all.contains("There"), "{all}");
    let here = e.ok(&["list", "--project", &e.path("work").to_string_lossy()]);
    assert!(here.contains("Here") && !here.contains("There"), "{here}");
    assert!(here.ends_with("1 task in other projects"), "{here}");
    let mut c = e.cmd(&["list", "--json", "--project"]);
    let board: Value =
        serde_json::from_slice(&c.current_dir(e.path("other")).output().unwrap().stdout).unwrap();
    let titles: Vec<&str> = all_rows(&board).iter().map(|r| r["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["There"]);
}

#[test]
fn the_board_opened_in_a_repository_keeps_to_it_until_dot() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    std::fs::create_dir(e.repo()).unwrap();
    git(&e.repo(), &["init", "-q", "-b", "main"]);
    e.ok(&["new", "In the repo", "--project", &e.repo().to_string_lossy()]);
    prepared(&e);
    let repo = e.repo().display().to_string();
    t.type_in(&pane, &format!("cd '{repo}' && {}", e.typed(env!("CARGO_BIN_EXE_varde"))));
    let shown = || t.tmux(&["capture-pane", "-p", "-t", &pane]).unwrap();
    wait("the board", || shown().contains("In the repo"));
    assert!(shown().contains("varde · repo · 1 task in other projects") && !shown().contains("Try it"));
    t.tmux(&["send-keys", "-t", &pane, "."]).unwrap();
    wait("every project", || shown().contains("Try it"));
    t.tmux(&["send-keys", "-t", &pane, "q"]).unwrap();
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
    // Without a cairn, `e` edits the task in the form, not the editor.
    t.tmux(&["send-keys", "-t", &pane, "e"]).unwrap();
    wait("the form", || shown().contains("Edit task"));
    t.tmux(&["send-keys", "-t", &pane, "Escape"]).unwrap();
    wait("the form to go", || !shown().contains("Edit task"));
    t.tmux(&["send-keys", "-t", &pane, "E"]).unwrap();
    wait("the message", || shown().contains("cairn unchanged"));
    assert!(!shown().contains("saved"), "{}", shown());
    t.tmux(&["send-keys", "-t", &pane, "q"]).unwrap();
}

#[test]
fn a_new_task_can_be_written_in_the_editor() {
    let e = env();
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    prepared(&e);
    // An editor that keeps the hint and the context typed, fills in the
    // empty Title, and writes more context.
    let editor = e.script(
        "editor",
        // Not `sed -i`, which BSD sed (macOS) takes differently.
        "#!/bin/sh\nsed 's/^Title: $/Title: Cache by locale/' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"\nprintf 'Start in cache.rs\\nthen the tests\\n' >> \"$1\"\n",
    );
    t.type_in(&pane, &e.typed(&format!("VISUAL={} {}", editor.display(), env!("CARGO_BIN_EXE_varde"))));
    let shown = || t.tmux(&["capture-pane", "-p", "-t", &pane]).unwrap();
    wait("the board", || shown().contains("Try it"));
    t.tmux(&["send-keys", "-t", &pane, "n"]).unwrap();
    // The form opens in the context.
    t.tmux(&["send-keys", "-t", &pane, "-l", "It misses"]).unwrap();
    t.tmux(&["send-keys", "-t", &pane, "C-x", "C-e"]).unwrap();
    wait("the form again", || shown().contains("Title:   Cache by locale"));
    t.tmux(&["send-keys", "-t", &pane, "C-s"]).unwrap();
    wait("the project", || shown().contains("Project for this task"));
    t.tmux(&["send-keys", "-t", &pane, "Enter"]).unwrap();
    wait("the task", || shown().contains("created "));
    t.tmux(&["send-keys", "-t", &pane, "q"]).unwrap();
    assert_eq!(
        e.query("SELECT body FROM task WHERE title = 'Cache by locale'"),
        "It misses\nStart in cache.rs\nthen the tests",
        "the context stays the context"
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
    wait("the keys", || shown().contains("cycle what needs you"));
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
#[cfg_attr(not(target_os = "linux"), ignore = "uses util-linux script")]
fn a_turn_that_ends_in_a_pane_on_screen_is_read_with_no_board_running() {
    let e = Env::with_repo(AGENT);
    let Some(t) = Tmux::start(e.dir.path()) else { return eprintln!("no tmux; skipped") };
    let pane = t.tmux(&["display", "-p", "-t", "t", "#{pane_id}"]).unwrap();
    let task = e.new_task("Watched");
    let plan: Value =
        serde_json::from_str(&e.ok(&["start", &task, "--worktree", "never", "--json"])).unwrap();
    let run = plan["run"].as_str().unwrap();
    let unread = || e.board()["groups"][0]["rows"][0]["unread"] == true;
    t.type_in(&pane, &e.typed(&format!("FAKE_SLEEP=30 '{}' run {run}", env!("CARGO_BIN_EXE_varde"))));
    // Its turn ends with no client attached: nobody is in it. The session
    // outlives the waits below: one that exits is not unread either.
    wait("the turn's end", unread);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(unread(), "read with nobody looking");
    let attach = format!("tmux -S {} attach -t t", t.sock.display());
    let mut client = Command::new("script")
        .args(["-qfc", &attach, "/dev/null"])
        .env("TERM", "xterm-256color")
        .env_remove("TMUX")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let read = poll(100, || !unread());
    let _ = client.kill();
    let _ = client.wait();
    assert!(read, "unread while its pane is on screen");
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

#[test]
fn next_outside_tmux_says_what_it_is_for_and_leaves_tmux_alone() {
    let e = env();
    let tmux = e.script("tmux", "#!/bin/sh\ntouch \"$FAKE_HOME/tmux-called\"\n");
    let out = e.cmd(&["next"]).env("VARDE_TMUX", &tmux).output().unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("`varde next` is for a key in tmux (see the guide)"), "{said}");
    assert!(!e.path("tmux-called").exists(), "the default tmux server was asked");
}
