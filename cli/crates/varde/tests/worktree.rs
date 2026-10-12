//! A task's worktree over its whole life: made, closed, dropped, brought
//! back, and started where there is no repository.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use common::*;

/// Commands run in the repository.
fn env() -> Env {
    let mut e = Env::with_repo(AGENT);
    e.cwd = e.repo();
    e
}

/// The task's worktree path and branch.
fn worktree(e: &Env, task: &str) -> (PathBuf, String) {
    let w = &e.show(task)["worktree"];
    (PathBuf::from(w["path"].as_str().unwrap()), w["branch"].as_str().unwrap().to_string())
}

/// Where the fake agent last ran.
fn last_cwd(e: &Env) -> String {
    let log = std::fs::read_to_string(e.path("cwd.log")).unwrap();
    log.lines().last().unwrap().to_string()
}

#[test]
fn a_task_whose_worktree_landed_gets_a_new_one() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Landed");
    let (_, branch) = worktree(&e, &t);
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    e.ok(&["done", &t]);
    assert!(!path.exists());
    e.ok(&["attach", &t, "--fresh", "--worktree", "always"]);
    assert!(path.join("work.txt").exists());
    let d = e.show(&t);
    assert_eq!((d["state"].as_str(), d["worktree"]["state"].as_str()), (Some("open"), Some("active")));
}

#[test]
fn a_start_that_failed_after_git_made_the_worktree_can_be_run_again() {
    let e = env();
    let t = e.new_task("Half made");
    // An agent that does not exist fails before anything is made.
    assert!(!e.run(&["start", &t, "--worktree", "always", "--agent", "nope"]).status.success());
    assert!(!e.repo().join(".worktrees").exists());
    // Git debris from a start that failed after making the worktree.
    let path = e.repo().join(".worktrees").join(format!("half-made-{t}"));
    git(
        &e.repo(),
        &["worktree", "add", "-q", "-b", &format!("varde/half-made-{t}"), &path.to_string_lossy()],
    );
    e.ok(&["attach", &t, "--worktree", "always"]);
    assert_eq!(worktree(&e, &t).0, path);
}

#[test]
fn a_dropped_task_started_again_has_its_work_back() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Changed my mind");
    std::fs::write(path.join("work.txt"), "edited").unwrap();
    std::fs::write(path.join("new.txt"), "untracked").unwrap();
    e.ok(&["drop", &t]);
    assert!(!path.exists());
    assert!(e.ok(&["reopen", &t]).starts_with("reopened "));
    assert!(e.ok(&["reopen", &t]).ends_with(" is already open"));
    let out = e.run(&["start", &t]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("restored worktree") && said.contains("brought back"), "{said}");
    assert_eq!(git(&path, &["log", "-1", "--format=%s"]), "work.txt");
    assert_eq!(std::fs::read_to_string(path.join("work.txt")).unwrap(), "edited");
    assert_eq!(std::fs::read_to_string(path.join("new.txt")).unwrap(), "untracked");
}

#[test]
fn a_task_dropped_twice_keeps_both_drops() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Twice");
    let first = git(&path, &["rev-parse", "HEAD"]);
    e.ok(&["drop", &t]);
    e.ok(&["start", &t]);
    commit(&path, "more.txt", "more");
    let second = git(&path, &["rev-parse", "HEAD"]);
    e.ok(&["drop", &t]);
    for c in [first, second] {
        assert!(!git(&e.repo(), &["for-each-ref", "--contains", &c, "refs/varde/"]).is_empty());
    }
}

#[test]
fn a_drop_survives_a_worktree_branch_or_repo_that_is_gone() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Gone by hand");
    let (_, branch) = worktree(&e, &t);
    git(&e.repo(), &["worktree", "remove", "--force", &path.to_string_lossy()]);
    git(&e.repo(), &["branch", "-D", &branch]);
    let out: Value = serde_json::from_str(&e.ok(&["drop", &t, "--json"])).unwrap();
    assert!(out["worktree"]["saved"].is_null());
    assert_eq!(e.show(&t)["state"], "dropped");

    let (t, _) = worktree_with_a_commit(&e, "Repo gone");
    let (u, _) = worktree_with_a_commit(&e, "Repo gone too");
    e.ok(&["done", &u]);
    std::fs::rename(e.repo(), e.path("moved")).unwrap();
    std::fs::create_dir(e.repo()).unwrap();
    let out: Value = serde_json::from_str(&e.ok(&["drop", &t, "--json"])).unwrap();
    assert_eq!(out["worktree"]["action"], "discarded");
    assert_eq!(e.show(&t)["worktree"]["state"], "removed");
    // A done task whose repo is gone does not wait in Done for ever.
    e.ok(&["git-refresh"]);
    assert_eq!(e.show(&u)["worktree"]["state"], "removed");
}

#[test]
fn done_leaves_a_landed_worktree_while_its_session_runs() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Still running");
    let (_, branch) = worktree(&e, &t);
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    let mut child = e.cmd(&["attach", &t]).env("FAKE_SLEEP", "30").spawn().unwrap();
    // The first run, which made the commit, has exited; this is the second.
    poll(100, || e.show(&t)["runs"].as_array().unwrap().iter().any(|r| r["status"] == "waiting"));
    let out: Value = serde_json::from_str(&e.ok(&["done", &t, "--json"])).unwrap();
    assert_eq!(out["worktree"]["action"], "kept");
    assert_eq!(out["stopped"].as_array().unwrap().len(), 1);
    assert!(path.exists());
    child.wait().unwrap();
    // Once the session is gone, the refresh removes it.
    e.ok(&["git-refresh"]);
    assert!(!path.exists());
}

#[test]
fn done_remove_waits_for_a_session_to_stop_before_looking_at_its_worktree() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Last word");
    let (_, branch) = worktree(&e, &t);
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    let mut child =
        e.cmd(&["attach", &t]).env("FAKE_SLEEP", "30").env("FAKE_LAST_WRITE", "late.txt").spawn().unwrap();
    poll(100, || e.show(&t)["runs"].as_array().unwrap().iter().any(|r| r["status"] == "waiting"));
    // Clean and landed when done looks, but the session writes as it stops:
    // removing the worktree then would destroy that.
    let out = e.run(&["done", &t, "--remove"]);
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("uncommitted changes") && said.contains("its sessions were stopped"), "{said}");
    assert_eq!(std::fs::read_to_string(path.join("late.txt")).unwrap(), "late\n");
    child.wait().unwrap();
    assert_eq!(e.show(&t)["state"], "open");
}

#[test]
fn drop_keeps_what_a_session_writes_as_it_stops() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Dropped last word");
    let mut child =
        e.cmd(&["attach", &t]).env("FAKE_SLEEP", "30").env("FAKE_LAST_WRITE", "late.txt").spawn().unwrap();
    poll(100, || e.show(&t)["runs"].as_array().unwrap().iter().any(|r| r["status"] == "waiting"));
    e.ok(&["drop", &t]);
    child.wait().unwrap();
    assert!(!path.exists());
    // Starting it again brings the work back, the late write with it.
    e.ok(&["attach", &t]);
    assert_eq!(std::fs::read_to_string(path.join("late.txt")).unwrap(), "late\n");
}

#[test]
fn a_run_restored_below_the_worktree_root_resumes_there() {
    let e = env();
    let t = e.new_task("Deep");
    e.ok(&["attach", &t, "--worktree", "always"]);
    let (path, _) = worktree(&e, &t);
    commit(&path, "keep.txt", "k");
    std::fs::create_dir(path.join("src")).unwrap();
    commit(&path, "src/lib.txt", "l");
    let src = path.join("src");
    // The conversation resumed by hand from the subdirectory.
    let native = e.show(&t)["runs"][0]["native_id"].as_str().unwrap().to_string();
    let mut agent = Command::new(e.path("claude"));
    agent.args(["--resume", &native]);
    let out = e.env(&mut agent).current_dir(&src).env("PWD", &src).output().unwrap();
    assert!(out.status.success());
    let runs = e.show(&t)["runs"].as_array().unwrap().clone();
    assert_eq!(runs.last().unwrap()["cwd"].as_str().unwrap(), src.to_string_lossy());
    e.ok(&["done", &t, "--remove", "--force"]);
    assert!(!path.exists());
    e.ok(&["attach", &t]);
    assert_eq!(last_cwd(&e), src.to_string_lossy());
    assert!(path.join("keep.txt").exists());
}

#[test]
fn outside_a_repository_no_worktree_is_asked_for_or_made() {
    let e = env();
    let plain = e.path("plain");
    std::fs::create_dir(&plain).unwrap();
    let t = e.ok(&["new", "Notes", "--project", &plain.to_string_lossy()]);
    // No terminal, no --worktree: nothing to ask.
    e.ok(&["attach", &t]);
    let u = e.ok(&["new", "More notes", "--project", &plain.to_string_lossy()]);
    let out = e.run(&["attach", &u, "--worktree", "always"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a git repository"));
    assert!(e.show(&u)["worktree"].is_null());
    // Recall there says what it searched.
    let out = e.cmd(&["recall", "zebra"]).current_dir(&plain).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("nothing found in the tasks made in this directory or any other project"),
        "{said}"
    );
    let said = e.ok(&["recall", "zebra"]);
    assert!(said.contains("nothing found in this project or any other project"), "{said}");
}

#[test]
fn a_repository_with_no_commits_says_so() {
    let e = env();
    let empty = e.path("empty");
    std::fs::create_dir(&empty).unwrap();
    git(&empty, &["init", "-q"]);
    let t = e.ok(&["new", "First", "--project", &empty.to_string_lossy()]);
    let out = e.run(&["attach", &t, "--worktree", "always"]);
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("no commits yet") && !said.contains("fatal"), "{said}");
    // Nothing to ask about: it runs without one, and says why.
    let out = e.run(&["start", &t, "--json"]);
    let plan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["action"], "run");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no commits yet; running without a worktree"));
    assert!(e.show(&t)["worktree"].is_null());
}

#[test]
fn a_default_of_always_falls_back_in_a_repository_with_no_commits() {
    let e = env();
    std::fs::write(e.path("config.toml"), "[worktree]\ndefault = \"always\"\n").unwrap();
    let empty = e.path("empty");
    std::fs::create_dir(&empty).unwrap();
    git(&empty, &["init", "-q"]);
    let t = e.ok(&["new", "First", "--project", &empty.to_string_lossy()]);
    let out = e.run(&["start", &t, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no commits yet; running without a worktree"));
}

#[test]
fn a_task_made_in_a_subdirectory_belongs_to_the_repository() {
    let e = env();
    let sub = e.repo().join("src/deep");
    std::fs::create_dir_all(&sub).unwrap();
    let t = e.ok(&["new", "From below"]);
    let u =
        String::from_utf8(e.cmd(&["new", "From deeper"]).current_dir(&sub).output().unwrap().stdout).unwrap();
    for task in [t.as_str(), u.trim()] {
        assert_eq!(
            e.show(task)["project"].as_str().unwrap(),
            e.repo().canonicalize().unwrap().to_string_lossy()
        );
    }
    e.cmd(&["attach", u.trim(), "--worktree", "always"]).current_dir(&sub).output().unwrap();
    assert!(worktree(&e, u.trim()).0.starts_with(e.repo().canonicalize().unwrap().join(".worktrees")));
}

#[test]
fn a_project_that_does_not_exist_is_refused() {
    let e = env();
    let out = e.run(&["new", "Nowhere", "--project", "/no/such/dir"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no directory /no/such/dir"));
}

#[test]
fn an_empty_varde_home_is_unset() {
    let e = env();
    let home = e.path("home");
    let out =
        e.cmd(&["new", "Homeless"]).env("VARDE_HOME", "").env("XDG_STATE_HOME", &home).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(home.join("varde/varde.db").exists());
    assert!(!e.repo().join("varde.db").exists());
}

#[test]
fn a_worktree_with_changed_included_files_is_not_removed_when_its_work_lands() {
    let e = env();
    std::fs::write(e.repo().join(".gitignore"), ".env\n").unwrap();
    std::fs::write(e.repo().join(".worktreeinclude"), ".env\n").unwrap();
    git(&e.repo(), &["add", "."]);
    git(&e.repo(), &["commit", "-q", "-m", "setup"]);
    std::fs::write(e.repo().join(".env"), "A=1").unwrap();
    let (t, path) = worktree_with_a_commit(&e, "Secrets");
    assert_eq!(std::fs::read_to_string(path.join(".env")).unwrap(), "A=1");
    std::fs::write(path.join(".env"), "A=2").unwrap();
    let (_, branch) = worktree(&e, &t);
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    let said = e.ok(&["done", &t]);
    assert!(said.contains("its .env differs from the main checkout's"), "{said}");
    e.ok(&["git-refresh"]);
    assert!(path.exists());
    // Removing it by hand asks for --force, and then says what went.
    let out = e.run(&["done", &t, "--remove"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("changed .env"), "{err}");
    assert!(path.exists());
    // As the main checkout has it, nothing would be lost.
    std::fs::write(path.join(".env"), "A=1").unwrap();
    e.ok(&["git-refresh"]);
    assert!(!path.exists());
}

#[test]
fn dropping_a_worktree_with_changed_included_files_keeps_it() {
    let e = env();
    std::fs::write(e.repo().join(".gitignore"), ".env\n").unwrap();
    std::fs::write(e.repo().join(".worktreeinclude"), ".env\n").unwrap();
    git(&e.repo(), &["add", "."]);
    git(&e.repo(), &["commit", "-q", "-m", "setup"]);
    std::fs::write(e.repo().join(".env"), "A=1").unwrap();
    let (t, path) = worktree_with_a_commit(&e, "Secrets");
    std::fs::write(path.join(".env"), "A=2").unwrap();
    // The discarded ref keeps no ignored file: the change would be lost.
    let out = e.run(&["drop", &t]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("changed .env"), "{err}");
    assert_eq!(std::fs::read_to_string(path.join(".env")).unwrap(), "A=2");
}

#[test]
fn settings_choose_the_worktree_policy_place_and_branch() {
    let e = env();
    std::fs::write(
        e.path("config.toml"),
        "[worktree]\ndefault = \"always\"\ndir = \"../{repo}-trees\"\nbranch_prefix = \"me/\"\n",
    )
    .unwrap();
    let t = e.new_task("Placed elsewhere");
    e.ok(&["attach", &t]);
    let (path, branch) = worktree(&e, &t);
    // Stored resolved, as hooks report a session's directory.
    assert_eq!(path, e.path("repo-trees").join(branch.strip_prefix("me/").unwrap()));
    assert!(branch.starts_with("me/placed-elsewhere-"));
    // Outside the repository, nothing is added to its excludes.
    let exclude = std::fs::read_to_string(e.repo().join(".git/info/exclude")).unwrap_or_default();
    assert!(!exclude.contains("trees"));
    // The repository's own setting wins over the user's.
    git(&e.repo(), &["config", "varde.worktree", "never"]);
    let u = e.new_task("Stays home");
    e.ok(&["attach", &u]);
    assert!(e.show(&u)["worktree"].is_null());
    assert_eq!(Path::new(&last_cwd(&e)).canonicalize().unwrap(), e.repo().canonicalize().unwrap());
}

#[test]
fn a_broken_config_never_stops_a_hook() {
    let e = env();
    std::fs::write(e.path("config.toml"), "this is = not [toml").unwrap();
    let out = feed(&mut e.cmd(&["hook", "Stop"]), "{}");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn start_asks_in_json_when_it_has_no_terminal() {
    let e = env();
    let t = e.new_task("Undecided");
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json"])).unwrap();
    assert_eq!((&plan["action"], &plan["ask"]), (&"ask".into(), &"worktree".into()));
    // Nothing was started, and the answer carries on.
    assert!(e.show(&t)["runs"].as_array().unwrap().is_empty());
    let plan: Value = serde_json::from_str(&e.ok(&["start", &t, "--json", "--worktree", "never"])).unwrap();
    assert_eq!(plan["action"], "run");
    // Without --json it is an error naming the flag.
    let u = e.new_task("Also undecided");
    let out = e.run(&["start", &u]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("pass --worktree"));
}

#[test]
fn a_repository_setup_runs_once_in_a_new_worktree_before_the_agent() {
    let e = env();
    git(&e.repo(), &["config", "varde.setup", "echo ran >> setup.log; pwd > where"]);
    let t = e.new_task("Needs deps");
    let out = e.run(&["attach", &t, "--worktree", "always"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("setting up the worktree"));
    let (path, _) = worktree(&e, &t);
    assert_eq!(std::fs::read_to_string(path.join("setup.log")).unwrap(), "ran\n");
    // A resume in the same worktree does not set it up again.
    e.ok(&["attach", &t]);
    assert_eq!(std::fs::read_to_string(path.join("setup.log")).unwrap(), "ran\n");
    // A failing setup is reported, and the session still starts.
    git(&e.repo(), &["config", "varde.setup", "exit 3"]);
    let u = e.new_task("Broken deps");
    let out = e.run(&["attach", &u, "--worktree", "always"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("the setup failed"));
    assert_eq!(e.show(&u)["runs"].as_array().unwrap().len(), 1);
}

#[test]
fn a_restored_worktree_is_set_up_at_its_root_and_only_when_remade() {
    let e = env();
    let t = e.new_task("Deep setup");
    e.ok(&["attach", &t, "--worktree", "always"]);
    let (path, _) = worktree(&e, &t);
    std::fs::create_dir(path.join("src")).unwrap();
    commit(&path, "src/lib.txt", "l");
    let src = path.join("src");
    let native = e.show(&t)["runs"][0]["native_id"].as_str().unwrap().to_string();
    let mut agent = Command::new(e.path("claude"));
    agent.args(["--resume", &native]);
    assert!(e.env(&mut agent).current_dir(&src).env("PWD", &src).output().unwrap().status.success());
    git(
        &e.repo(),
        &["config", "varde.setup", "echo ran >> \"$(git rev-parse --show-toplevel)/setup.log\"; pwd > where"],
    );
    e.ok(&["done", &t, "--remove", "--force"]);
    let out = e.run(&["attach", &t]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(last_cwd(&e), src.to_string_lossy());
    let root = path.canonicalize().unwrap();
    assert_eq!(std::fs::read_to_string(path.join("where")).unwrap().trim(), root.to_string_lossy());
    // Only the subdirectory is gone: nothing is remade, nor set up again.
    std::fs::remove_dir_all(&src).unwrap();
    let out = e.run(&["attach", &t]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("restored worktree"));
    assert_eq!(std::fs::read_to_string(path.join("setup.log")).unwrap(), "ran\n");
}

#[test]
fn a_bad_repository_setting_is_ignored_with_a_notice() {
    let e = env();
    git(&e.repo(), &["config", "varde.worktree", "sometimes"]);
    let t = e.new_task("Typo in git config");
    let out = e.run(&["start", &t, "--worktree", "always", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("varde.worktree"));
    // Without the flag it is as if unset: the question is asked.
    let u = e.new_task("Also a typo");
    let plan: Value = serde_json::from_str(&e.ok(&["start", &u, "--json"])).unwrap();
    assert_eq!(plan["ask"], "worktree");
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "the fake claude is a script, which macOS names by its interpreter"
)]
fn a_task_belongs_to_the_checkout_it_was_made_in() {
    let e = env();
    let repo = e.repo().canonicalize().unwrap();
    // A worktree of your own is a project of its own: the agent edits it,
    // not the main checkout.
    let mine = e.dir.path().canonicalize().unwrap().join("repo-feature");
    git(&repo, &["worktree", "add", "-q", "-b", "feature", &mine.to_string_lossy()]);
    let t = String::from_utf8(e.cmd(&["new", "Mine"]).current_dir(&mine).output().unwrap().stdout).unwrap();
    assert_eq!(e.show(t.trim())["project"].as_str().unwrap(), mine.to_string_lossy());
    e.ok(&["attach", t.trim(), "--worktree", "never"]);
    assert_eq!(last_cwd(&e), mine.to_string_lossy());
    // A session typed into a task's worktree varde made is in the repository.
    let u = e.new_task("Made by varde");
    e.ok(&["attach", &u, "--worktree", "always"]);
    let (made, _) = worktree(&e, &u);
    let mut agent = Command::new(e.path("claude"));
    agent.args(["--session-id", "typed-in-the-worktree"]);
    assert!(e.env(&mut agent).current_dir(&made).env("PWD", &made).output().unwrap().status.success());
    let board: Value = serde_json::from_str(&e.ok(&["list", "--json"])).unwrap();
    let inbox = board["groups"].as_array().unwrap().iter().find(|g| g["key"] == "inbox").expect("an inbox");
    let task = inbox["rows"][0]["task"].as_str().unwrap();
    assert_eq!(e.show(task)["project"].as_str().unwrap(), repo.to_string_lossy());
    assert!(e.ok(&["accept", task]).starts_with("accepted "));
    assert!(e.ok(&["accept", task]).ends_with(" was not in the Inbox"));
}

#[test]
fn a_worktree_of_your_own_gives_its_tasks_its_branch() {
    let e = env();
    let repo = e.repo().canonicalize().unwrap();
    let mine = e.dir.path().canonicalize().unwrap().join("repo-feature");
    git(&repo, &["worktree", "add", "-q", "-b", "feature", &mine.to_string_lossy()]);
    commit(&mine, "feature.txt", "feature");
    let t = String::from_utf8(e.cmd(&["new", "Mine"]).current_dir(&mine).output().unwrap().stdout).unwrap();
    let t = t.trim();
    e.ok(&["attach", t, "--worktree", "always"]);
    let w = &e.show(t)["worktree"];
    let path = PathBuf::from(w["path"].as_str().unwrap());
    let branch = w["branch"].as_str().unwrap().to_string();
    // Cut from your branch, with your work in it, and kept beside the
    // repository's other task worktrees rather than inside yours.
    assert_eq!(w["base"], "feature");
    assert!(path.join("feature.txt").exists());
    assert!(path.starts_with(repo.join(".worktrees")), "{}", path.display());
    assert_eq!(git(&mine, &["status", "--porcelain"]), "");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    // And it has landed once merged into your branch, not main.
    commit(&path, "work.txt", "work");
    git(&mine, &["merge", "-q", "--no-edit", &branch]);
    e.ok(&["done", t]);
    assert!(!path.exists());
    assert_eq!(git(&repo, &["rev-list", "--count", "main"]), "1");
}

#[test]
fn work_your_git_config_hides_from_status_is_still_kept() {
    let e = env();
    git(&e.repo(), &["config", "status.showUntrackedFiles", "no"]);
    let t = e.new_task("Hidden");
    e.ok(&["attach", &t, "--worktree", "always"]);
    let (path, _) = worktree(&e, &t);
    std::fs::write(path.join("new.rs"), "fn main() {}").unwrap();
    e.ok(&["done", &t]);
    e.ok(&["git-refresh"]);
    assert!(path.join("new.rs").exists());
    e.ok(&["start", &t]);
    e.ok(&["drop", &t]);
    e.ok(&["start", &t]);
    assert_eq!(std::fs::read_to_string(path.join("new.rs")).unwrap(), "fn main() {}");
}

#[test]
fn commits_on_a_detached_head_stop_a_removal() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Detached");
    let (_, branch) = worktree(&e, &t);
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    git(&path, &["checkout", "-q", "--detach"]);
    commit(&path, "precious.txt", "precious");
    let out = e.run(&["done", &t, "--remove"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("detached HEAD"), "{err}");
    assert!(path.join("precious.txt").exists());
    e.ok(&["done", &t]);
    e.ok(&["git-refresh"]);
    assert!(path.join("precious.txt").exists());
}

#[test]
fn a_drop_refuses_a_repository_no_ref_can_keep() {
    let e = env();
    let (t, path) = worktree_with_a_commit(&e, "Vendored");
    let lib = path.join("vendor/lib");
    std::fs::create_dir_all(&lib).unwrap();
    git(&lib, &["init", "-q"]);
    commit(&lib, "lib.rs", "lib");
    let out = e.run(&["drop", &t]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("vendor/lib/"), "{err}");
    assert!(lib.join("lib.rs").exists());
    assert_eq!(e.show(&t)["state"], "open");
}

#[test]
fn a_task_outlives_the_worktree_of_your_own_it_was_made_in() {
    let e = env();
    let repo = e.repo().canonicalize().unwrap();
    let mine = e.dir.path().canonicalize().unwrap().join("repo-feature");
    git(&repo, &["worktree", "add", "-q", "-b", "feature", &mine.to_string_lossy()]);
    let t = String::from_utf8(e.cmd(&["new", "Mine"]).current_dir(&mine).output().unwrap().stdout).unwrap();
    let t = t.trim();
    e.ok(&["attach", t, "--worktree", "always"]);
    let (path, _) = worktree(&e, t);
    commit(&path, "work.txt", "work");
    git(&repo, &["worktree", "remove", &mine.to_string_lossy()]);
    e.ok(&["done", t]);
    e.ok(&["git-refresh"]);
    assert_eq!(e.show(t)["worktree"]["state"], "active");
    let out: Value = serde_json::from_str(&e.ok(&["drop", t, "--json"])).unwrap();
    assert!(out["worktree"]["saved"].is_string(), "{out}");
    assert!(!path.exists());
}

/// git that may clone from a path, which the tests' git config forbids.
fn git_file(dir: &Path, args: &[&str]) -> String {
    git(dir, &[&["-c", "protocol.file.allow=always"], args].concat())
}

/// A repository `lib` with `a.txt` and then `b.txt` on main, after a
/// first commit (a shallow clone cuts below a commit with parents), and the
/// commit `pin` chooses after `a.txt`; the test's repository has it as
/// submodule `lib` at that commit, and so has a task's worktree. With
/// `cloned`, the main checkout has no repository of its own for it, as
/// in a fresh clone.
fn a_task_with_a_submodule(
    e: &Env,
    shallow: bool,
    cloned: bool,
    pin: impl FnOnce(&Path) -> String,
) -> (String, PathBuf) {
    let lib = e.path("lib");
    std::fs::create_dir(&lib).unwrap();
    git(&lib, &["init", "-q", "-b", "main"]);
    git(&lib, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
    git(&lib, &["commit", "-q", "--allow-empty", "-m", "init"]);
    commit(&lib, "a.txt", "a");
    let pinned = pin(&lib);
    git(&lib, &["checkout", "-q", "main"]);
    commit(&lib, "b.txt", "b");
    let repo = e.repo();
    git_file(&repo, &["submodule", "add", "-q", &format!("file://{}", lib.display()), "lib"]);
    git(&repo.join("lib"), &["checkout", "-q", &pinned]);
    if shallow {
        git(&repo, &["config", "-f", ".gitmodules", "submodule.lib.shallow", "true"]);
    }
    git(&repo, &["add", ".gitmodules", "lib"]);
    git(&repo, &["commit", "-q", "-m", "lib"]);
    if cloned {
        git(&repo, &["submodule", "deinit", "-q", "-f", "lib"]);
        std::fs::remove_dir_all(repo.join(".git/modules/lib")).unwrap();
    }
    let t = e.new_task("Submodule");
    e.ok(&["attach", &t, "--worktree", "always"]);
    let (path, _) = worktree(e, &t);
    git_file(&path, &["submodule", "update", "--init", "--recursive"]);
    assert_eq!(git(&path.join("lib"), &["rev-parse", "HEAD"]), pinned);
    (t, path)
}

#[test]
fn a_submodule_at_a_commit_only_a_tag_reaches_is_no_work_to_keep() {
    let e = env();
    let (t, path) = a_task_with_a_submodule(&e, false, true, |lib| {
        git(lib, &["checkout", "-q", "--detach"]);
        commit(lib, "release.txt", "release");
        git(lib, &["tag", "v1"]);
        git(lib, &["rev-parse", "HEAD"])
    });
    e.ok(&["done", &t]);
    assert!(!path.exists());
}

/// The repository's `main` pushed to a remote of its own, as its upstream.
fn pushed(e: &Env) {
    let origin = e.path("origin.git");
    git(e.dir.path(), &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(&e.repo(), &["remote", "add", "origin", &origin.to_string_lossy()]);
    git(&e.repo(), &["push", "-q", "-u", "origin", "main"]);
}

#[test]
fn a_shallow_submodule_is_no_work_to_keep() {
    let e = env();
    let (t, path) = a_task_with_a_submodule(&e, true, true, |lib| git(lib, &["rev-parse", "HEAD"]));
    assert_eq!(git(&path.join("lib"), &["rev-parse", "--is-shallow-repository"]), "true");
    e.ok(&["done", &t]);
    assert!(!path.exists());
}

#[test]
fn a_commit_in_a_shallow_submodule_merged_here_is_kept() {
    let e = env();
    let (t, path) = a_task_with_a_submodule(&e, true, true, |lib| git(lib, &["rev-parse", "HEAD"]));
    pushed(&e);
    let (_, branch) = worktree(&e, &t);
    commit(&path.join("lib"), "fix.txt", "fix");
    git(&path, &["commit", "-q", "-am", "bump lib"]);
    // Merged by hand, and pushed without the submodule's commit: the pin
    // names work nothing else has.
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    git(&e.repo(), &["push", "-q", "origin", "main"]);
    e.ok(&["done", &t]);
    e.ok(&["git-refresh"]);
    assert!(path.join("lib/fix.txt").exists());
}

#[test]
fn a_submodule_commit_only_the_worktree_has_keeps_it() {
    let e = env();
    let (t, path) = a_task_with_a_submodule(&e, false, false, |lib| git(lib, &["rev-parse", "HEAD"]));
    let (_, branch) = worktree(&e, &t);
    let sub = path.join("lib");
    commit(&sub, "fix.txt", "fix");
    git(&path, &["commit", "-q", "-am", "bump lib"]);
    let out = e.run(&["done", &t, "--remove"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("submodule lib"), "{err}");
    // Merged by hand, the base pins the commit; it is still nowhere else.
    git(&e.repo(), &["merge", "-q", "--no-edit", &branch]);
    let said = e.ok(&["done", &t]);
    assert!(said.contains("submodule lib that its remote does not have, which removing it"), "{said}");
    let out: Value = serde_json::from_str(&e.ok(&["done", &t, "--json"])).unwrap();
    assert_eq!(out["worktree"]["action"], "kept", "{out}");
    assert_eq!(out["worktree"]["stranded"][0], "commits in submodule lib that its remote does not have");
    e.ok(&["git-refresh"]);
    assert!(sub.join("fix.txt").exists());
    // Once the main checkout's own repository for it has the commit, the
    // worktree goes.
    git_file(&e.repo().join("lib"), &["fetch", "-q", &sub.to_string_lossy(), "HEAD"]);
    e.ok(&["git-refresh"]);
    assert!(path.exists(), "fetched only, it is pruned in time");
    git(&e.repo().join("lib"), &["checkout", "-q", "FETCH_HEAD"]);
    e.ok(&["git-refresh"]);
    assert!(!path.exists());
}
