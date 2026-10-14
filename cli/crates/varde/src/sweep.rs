//! The board's git column, and the cleanup after done tasks. `varde list`
//! only reads the `git_state` cache; when it is due, `kick` starts one
//! detached `varde git-refresh`. That checks every task the board tracks,
//! writes what it found, and removes a done task's worktree once nothing
//! in it can be lost. The store watch redraws the board. It also starts
//! again any cairn update whose process died, and names the tasks that
//! wait to be (`naming.rs`).

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use varde_core::board::{self, Board};
use varde_core::memory;
use varde_core::store::Store;

use crate::git::{self, Facts};
use crate::{done, job, now_ms, process, Paths};

/// Checked again after this long even when nothing varde saw happened: an
/// edit made by hand, or a PR merged in GitHub.
const MAX_AGE_MS: i64 = 60_000;

/// Whether a tracked row was last checked before its task last changed, or
/// too long ago.
pub fn due(board: &Board, now: i64) -> bool {
    board
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .any(|r| r.tracked && r.git_checked.is_none_or(|c| c < r.last || now - c > MAX_AGE_MS))
}

/// Start a detached `varde git-refresh` if the board is due one and none
/// is running.
pub fn kick(paths: &Paths, board: &Board, now: i64) -> Result<()> {
    crate::schedule::kick(board, now, paths)?;
    if !due(board, now) || lock(paths, false)?.is_none() {
        return Ok(());
    }
    crate::detach(&["git-refresh"], "the git refresh")
}

/// The refresh lock, held for as long as the file is open. Without `wait`,
/// `None` when another process holds it.
fn lock(paths: &Paths, wait: bool) -> Result<Option<File>> {
    paths.make_home()?;
    let file = File::create(paths.home.join("git-refresh.lock"))?;
    let how = if wait { libc::LOCK_EX } else { libc::LOCK_EX | libc::LOCK_NB };
    if unsafe { libc::flock(file.as_raw_fd(), how) } != 0 {
        return Ok(None);
    }
    Ok(Some(file))
}

struct Checked {
    task: String,
    facts: Option<(String, String, Option<&'static str>, String)>,
    checked: i64,
}

/// `varde git-refresh`: check every task the board tracks.
pub fn run(paths: &Paths) -> Result<()> {
    let _lock = lock(paths, true)?;
    let store = paths.open(Duration::from_secs(5))?;
    let board = board::build(&store, now_ms(), crate::list::local_midnight_ms(), false, &process::alive)?;
    let tasks: Vec<&str> =
        board.groups.iter().flat_map(|g| &g.rows).filter(|r| r.tracked).map(|r| r.task.as_str()).collect();

    // A project checkout is shared by its tasks, so it is read once.
    let mut shared: HashMap<String, Option<Facts>> = HashMap::new();
    let mut out = Vec::new();
    let mut tidied = Vec::new();
    for task in tasks {
        // Taken before git runs, so a change made meanwhile is seen next time.
        let checked = now_ms();
        let project: Option<String> =
            store.conn.query_row("SELECT project FROM task WHERE id = ?1", [task], |r| r.get(0))?;
        let facts = match active_worktree(&store, task)? {
            Some((path, repo, branch, base)) => {
                let p = Path::new(&path);
                let repo = &git::home(Path::new(&repo), p);
                let branch = follow(&store.conn, &path, repo, &branch).unwrap_or(branch);
                let facts = git::facts(p, Some(&base), true).ok();
                // Asking gh can take seconds, in which the task may have been
                // started again: its state then, not before, says to tidy.
                let state: String =
                    store.conn.query_row("SELECT state FROM task WHERE id = ?1", [task], |r| r.get(0))?;
                if state == "done" && done::live_runs(&store, task)?.is_empty() {
                    let action = match &facts {
                        // A failure here must not stop the others being checked.
                        Some(f) => tidy(repo, p, &branch, f).unwrap_or(None),
                        // The repo moved or was deleted, and took them with it.
                        None if !repo.join(".git").exists() => Some("removed"),
                        // Deleted by hand: its commits are still on the branch.
                        None if !p.exists() => git::remove(repo, p).ok().map(|_| "removed"),
                        None => None,
                    };
                    if let Some(action) = action {
                        tidied.push((path, action));
                        continue;
                    }
                }
                facts.as_ref().map(row)
            }
            None => {
                let Some(project) = project else { continue };
                shared
                    .entry(project.clone())
                    .or_insert_with(|| git::facts(Path::new(&project), None, true).ok())
                    .as_ref()
                    .map(row)
            }
        };
        out.push(Checked { task: task.to_string(), facts, checked });
    }

    let tx = store.conn.unchecked_transaction()?;
    for (path, action) in tidied {
        set_worktree_state(&tx, &path, action)?;
    }
    for c in out {
        let (stage, label, checks, facts) = match c.facts {
            Some((s, l, c, f)) => (Some(s), Some(l), c, Some(f)),
            None => (None, None, None, None),
        };
        tx.execute(
            "INSERT INTO git_state (task, stage, label, checks, facts, checked) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (task) DO UPDATE SET stage = ?2, label = ?3, checks = ?4, facts = ?5, checked = ?6",
            params![c.task, stage, label, checks, facts, c.checked],
        )?;
    }
    tx.commit()?;

    job::sync_memory(paths);
    for id in memory::requeue_stuck(&store.conn, now_ms())? {
        job::spawn(&id)?;
    }
    // Tasks a hook made, titled from their first prompt, wait for a name.
    let _ = crate::naming::sweep(&store);
    // Whether what cairns say about code still names code that exists.
    let _ = crate::stale::check_due(&store, now_ms());
    Ok(())
}

fn row(f: &Facts) -> (String, String, Option<&'static str>, String) {
    let checks = f.pr.as_ref().filter(|p| p.state == "open").and_then(|p| p.checks);
    (f.stage().to_string(), f.label(), checks, f.to_json().to_string())
}

/// Close the worktree at `path` as `state`, `merged` or `removed`.
pub fn set_worktree_state(conn: &rusqlite::Connection, path: &str, state: &str) -> Result<()> {
    conn.execute(
        "UPDATE worktree SET state = ?2, closed = ?3 WHERE path = ?1",
        params![path, state, now_ms()],
    )?;
    Ok(())
}

/// The task's active worktree: path, repo, branch and base.
pub fn active_worktree(store: &Store, task: &str) -> Result<Option<(String, String, String, String)>> {
    Ok(store
        .conn
        .query_row(
            "SELECT path, repo, branch, base FROM worktree WHERE task = ?1 AND state = 'active'
             ORDER BY created DESC LIMIT 1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?)
}

/// Remove a done task's worktree and branch if nothing in them can be
/// lost: no changes, and every commit in the base, or in a PR merged at
/// the branch's tip. Ignored files go with it, as `git worktree remove`
/// treats them, except those `.worktreeinclude` names that differ from
/// the main checkout's. Returns the worktree's new state, `merged` for
/// work that landed and `removed` for a branch that never had any.
pub fn tidy(repo: &Path, path: &Path, branch: &str, f: &Facts) -> Result<Option<&'static str>> {
    if f.changes > 0 || !(f.landed || f.ahead == 0) {
        return Ok(None);
    }
    if git::included_changes(repo, path).map_or(true, |c| !c.is_empty()) {
        return Ok(None);
    }
    if !git::stranded(path)?.is_empty() {
        return Ok(None);
    }
    git::remove(repo, path)?;
    if f.branch == branch {
        git::git(repo, &["branch", "-D", branch])?;
    } else if let Some(base) = f.target.as_deref().filter(|_| git::has_branch(repo, branch)) {
        // A branch that was there before the task, or a detached HEAD, was
        // checked out instead: that stays, and the task's own goes only
        // when it holds nothing the base lacks.
        git::delete_if_merged(repo, base, branch)?;
    }
    Ok(Some(if f.landed { "merged" } else { "removed" }))
}

/// The task's branch in its worktree at `path`, recorded as `branch`: the
/// branch checked out there instead, when it was made there (renamed from
/// it, or cut by the agent: `git::born_since`), which is then recorded, so
/// that closing, dropping and starting again follow the work. The branch
/// it replaces is deleted when the new one has all its commits.
pub fn follow(conn: &rusqlite::Connection, path: &str, repo: &Path, branch: &str) -> Result<String> {
    let p = Path::new(path);
    let created: i64 =
        conn.query_row("SELECT created FROM worktree WHERE path = ?1", [path], |r| r.get(0))?;
    let Some(live) = git::born_since(p, created).filter(|b| b != branch) else {
        return Ok(branch.to_string());
    };
    if git::has_branch(repo, branch)
        && git::git(repo, &["merge-base", "--is-ancestor", branch, &live]).is_ok()
    {
        // Checked out in another worktree, git refuses, and it is kept.
        let _ = git::git(repo, &["branch", "-D", branch]);
    }
    conn.execute("UPDATE worktree SET branch = ?2 WHERE path = ?1", params![path, live])?;
    Ok(live)
}
