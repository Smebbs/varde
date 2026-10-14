//! `varde done`: close a task. Its worktree goes as soon as nothing in it
//! can be lost: at once if its work is already in the base, else when
//! `varde git-refresh` sees it land. Git runs first; the task is only
//! closed if what was asked succeeded.
//! The sessions varde launched for it are stopped, before any worktree is
//! removed: their conversations are kept, so starting the task again
//! resumes them. The tmux panes they ran in close once they are back at
//! their shell.
//!
//! `varde park` puts a task aside the same way, but never touches git: it
//! may be picked up again, or never. `varde drop` throws a task away: any
//! work in its worktree is kept under a ref, and the worktree removed;
//! starting the task again brings the work back.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use rusqlite::params;
use serde_json::{json, Value};
use varde_core::store::{self, Store};
use varde_core::task;

use crate::git;
use crate::{now_ms, process, sweep, tmux, Paths};

#[derive(Clone, Copy)]
pub struct Options {
    pub remove: bool,
    pub force: bool,
    /// Throw the work away, keeping it under `refs/varde/discarded/`.
    pub discard: bool,
    /// Leave the task's running sessions alone.
    pub keep_running: bool,
}

pub fn done(paths: &Paths, reference: &str, opts: Options, as_json: bool) -> Result<()> {
    act(paths, reference, as_json, |store, id| close(store, id, opts), describe)
}

/// Do `what` to the task `reference` names, and print its report.
fn act(
    paths: &Paths,
    reference: &str,
    as_json: bool,
    what: impl FnOnce(&Store, &str) -> Result<Value>,
    describe: fn(&Value) -> String,
) -> Result<()> {
    let store = paths.open(Duration::from_secs(2))?;
    let id = task::resolve(&store, reference)?;
    let report = what(&store, &id)?;
    if as_json {
        println!("{report}");
    } else {
        println!("{}", describe(&report));
    }
    Ok(())
}

/// Close task `id`, doing what `opts` asks with its worktree, and queue
/// its cairn's last update. Returns the report `--json` prints.
pub fn close(store: &Store, id: &str, opts: Options) -> Result<Value> {
    let report = close_task(store, id, opts)?;
    settle(store, id);
    Ok(report)
}

/// The task's last cairn update, which brings Status to where it ended.
/// Memory is not worth failing a close over.
fn settle(store: &Store, id: &str) {
    let _ = crate::job::close_out(store, id);
}

/// What a worktree holds that removing it could lose.
struct Look {
    /// Whether `dirty` and `ahead` could be read.
    known: bool,
    dirty: bool,
    ahead: u32,
    by_pr: Option<git::Pr>,
    included: Vec<String>,
    stranded: Vec<String>,
}

fn close_task(store: &Store, id: &str, opts: Options) -> Result<Value> {
    let id = id.to_string();
    let wt = sweep::active_worktree(store, &id)?;

    // Sessions are stopped before their worktree goes, never after; and
    // only once what was asked looks as if it cannot fail.
    let mut stopped: Option<(Vec<String>, Vec<String>)> = None;
    let stop = |stopped: &mut Option<(Vec<String>, Vec<String>)>| -> Result<()> {
        if stopped.is_none() && !opts.keep_running {
            *stopped = Some(stop_sessions(store, &id, false)?);
        }
        Ok(())
    };
    let mut report = json!({"v": 1, "task": id, "worktree": null});
    if let Some((path, repo, branch, base)) = wt {
        let p = Path::new(&path);
        let repo_p = &git::home(Path::new(&repo), p);
        let branch = sweep::follow(&store.conn, &path, repo_p, &branch).unwrap_or(branch);
        let look = || -> Result<Look> {
            // Keeping the worktree needs none of these facts, so a repo in
            // an odd state (base deleted, repo moved) must not stop the
            // task closing; removing does need them.
            let facts = (|| -> Result<(bool, u32)> {
                Ok((p.exists() && git::dirty(p)?, git::ahead(repo_p, &base, &branch)?))
            })();
            let known = facts.is_ok();
            let (dirty, ahead) = match facts {
                Ok(f) => f,
                Err(e) if opts.remove => return Err(e),
                Err(_) => (false, 0),
            };
            // A PR merged in GitHub is often squashed, so its commits look
            // unmerged here; a PR merged with exactly this branch's tip landed.
            let by_pr =
                if opts.remove && ahead > 0 && !dirty { git::merged_pr(repo_p, &branch) } else { None };
            // Ignored files `.worktreeinclude` copied in and the task
            // changed, such as an `.env`: no branch keeps them.
            let included =
                if p.exists() { git::included_changes(repo_p, p).unwrap_or_default() } else { vec![] };
            // What no ref can keep, so removing the worktree would destroy
            // it. A worktree only kept merely reports it, so there a
            // failure to read it must not stop the task closing.
            let stranded = if !p.exists() {
                vec![]
            } else if opts.remove || opts.discard {
                git::stranded(p)?
            } else {
                git::stranded(p).unwrap_or_default()
            };
            Ok(Look { known, dirty, ahead, by_pr, included, stranded })
        };
        let refuse = |l: &Look| -> Result<()> {
            if opts.remove {
                if !l.stranded.is_empty() && !opts.force {
                    bail!(
                        "{path} has {}, which removing it would destroy; push or move them, or pass --force",
                        l.stranded.join(" and ")
                    );
                }
                let unmerged = if l.by_pr.is_some() { 0 } else { l.ahead };
                if (l.dirty || unmerged > 0 || !l.included.is_empty()) && !opts.force {
                    let mut what = vec![];
                    if l.dirty {
                        what.push("uncommitted changes".to_string());
                    }
                    if unmerged > 0 {
                        what.push(format!(
                            "{unmerged} unmerged {}",
                            if unmerged == 1 { "commit" } else { "commits" }
                        ));
                    }
                    if !l.included.is_empty() {
                        what.push(format!("changed {} (from .worktreeinclude)", l.included.join(", ")));
                    }
                    bail!(
                        "{path} has {}; pass --force to remove it anyway (committed work stays on {branch})",
                        what.join(" and ")
                    );
                }
            } else if opts.discard {
                // Dropping takes the worktree away, and this session would
                // be left running in a directory that is gone.
                let me = std::process::id();
                if live_runs(store, &id)?.iter().any(|(_, pid, start)| process::descends(me, (*pid, *start)))
                {
                    bail!("this session runs in {path}; end it, then drop the task");
                }
                if !l.stranded.is_empty() {
                    bail!(
                        "{path} has {}, which no ref can keep; push or move them, then drop it again",
                        l.stranded.join(" and ")
                    );
                }
                // Ignored files are not in the ref: a changed `.env` would be lost.
                if !l.included.is_empty() {
                    bail!(
                        "{path} has changed {} (from .worktreeinclude), which the discarded ref cannot keep; \
                         copy them out or put them back, then drop it again",
                        l.included.join(", ")
                    );
                }
            }
            Ok(())
        };
        let mut l = look()?;
        refuse(&l)?;
        if (opts.remove || opts.discard) && !opts.keep_running {
            // A session may still write as it stops, after the look above:
            // it is waited for, and the worktree looked at again.
            let s = stop_sessions(store, &id, true)?;
            let any = !s.0.is_empty();
            stopped = Some(s);
            if any {
                // Refused now, the task stays open but its sessions are gone.
                l = look().and_then(|l| refuse(&l).map(|_| l)).map_err(|e| {
                    anyhow::anyhow!("{e:#}; its sessions were stopped, and starting the task resumes them")
                })?;
            }
        }
        let mut action = "kept";
        let mut saved = None;
        // A session still in the worktree keeps it, whatever its work.
        let mut session = false;

        if opts.remove {
            let left = match &stopped {
                Some((_, left)) => !left.is_empty(),
                None => !live_runs(store, &id)?.is_empty(),
            };
            if left {
                // The agent closing its own task runs in this worktree: it
                // stays until that session ends, and the refresh then
                // removes it as it does any done task's that landed. So
                // does one `--keep-running` left alone.
                session = true;
            } else {
                git::remove(repo_p, p)?;
                // Once its work is in the base, or in a PR merged with
                // exactly its tip.
                if l.by_pr.is_some() {
                    git::git(repo_p, &["branch", "-D", &branch])?;
                } else {
                    git::delete_if_merged(repo_p, &base, &branch)?;
                }
                action = "removed";
            }
        } else if opts.discard {
            // A repo that is gone took the worktree and branch with it.
            if repo_p.join(".git").exists() {
                let saved_ref = git::discarded_ref(p);
                saved = git::save(repo_p, p, &branch, &saved_ref, &task::short(&id))?.map(|_| saved_ref);
                git::remove(repo_p, p)?;
                // `save` kept the branch's commits, so deleting it loses nothing.
                if git::has_branch(repo_p, &branch) {
                    git::git(repo_p, &["branch", "-D", &branch])?;
                }
            }
            action = "discarded";
        } else if !opts.keep_running && p.exists() && live_runs(store, &id)?.is_empty() {
            // Already landed: nothing to wait for. A PR merged in GitHub is
            // left to the refresh, which asks gh, as is a worktree a live
            // session is still in.
            if let Ok(f) = git::facts(p, Some(&base), false) {
                if let Some(a) = sweep::tidy(repo_p, p, &branch, &f)? {
                    action = a;
                }
            }
        } else {
            session = p.exists() && !live_runs(store, &id)?.is_empty();
        }

        if action != "kept" {
            let state = if action == "discarded" { "removed" } else { action };
            sweep::set_worktree_state(&store.conn, &path, state)?;
        }
        report["worktree"] = json!({"path": path, "branch": branch, "base": base,
            "ahead": l.known.then_some(l.ahead), "dirty": l.known.then_some(l.dirty), "included": l.included,
            "action": action, "session": session, "stranded": l.stranded,
            "pr": l.by_pr.map(|p| json!({"number": p.number, "url": p.url})), "saved": saved});
    }

    store.conn.execute(
        "UPDATE task SET state = 'done', closed = ?2, inbox = 0 WHERE id = ?1 AND state != 'done'",
        params![id, now_ms()],
    )?;

    stop(&mut stopped)?;
    let (stopped, left) = stopped.unwrap_or_default();
    report["stopped"] = json!(stopped);
    report["left"] = json!(left);
    Ok(report)
}

pub fn park(paths: &Paths, reference: &str, keep_running: bool, as_json: bool) -> Result<()> {
    act(paths, reference, as_json, |store, id| shelve(store, id, keep_running), describe_park)
}

/// Park task `id`: off the board's working groups, its worktree left as it
/// is. Returns the report `--json` prints.
pub fn shelve(store: &Store, id: &str, keep_running: bool) -> Result<Value> {
    store.conn.execute(
        "UPDATE task SET state = 'parked', closed = ?2, inbox = 0 WHERE id = ?1 AND state != 'parked'",
        params![id, now_ms()],
    )?;
    let (stopped, left) = if keep_running { Default::default() } else { stop_sessions(store, id, false)? };
    settle(store, id);
    Ok(json!({"v": 1, "task": id, "stopped": stopped, "left": left}))
}

pub fn drop(paths: &Paths, reference: &str, as_json: bool) -> Result<()> {
    act(paths, reference, as_json, reject, describe_drop)
}

/// Throw task `id` away, stopping its sessions. Work in its worktree is
/// first kept under `refs/varde/discarded/`, and the worktree removed.
pub fn reject(store: &Store, id: &str) -> Result<Value> {
    let opts = Options { remove: false, force: false, discard: true, keep_running: false };
    let report = close_task(store, id, opts)?;
    store.conn.execute("UPDATE task SET state = 'dropped', inbox = 0 WHERE id = ?1", [id])?;
    settle(store, id);
    Ok(report)
}

pub fn describe_drop(report: &Value) -> String {
    describe_as(report, "dropped")
}

/// `varde reopen`: back to Open from done, parked or dropped, as starting does.
pub fn reopen(paths: &Paths, reference: &str) -> Result<()> {
    let store = paths.open(Duration::from_secs(2))?;
    let id = task::resolve(&store, reference)?;
    if crate::start::reopen(&store.conn, &id)? {
        println!("reopened {}", task::short(&id));
    } else {
        println!("{} is already open", task::short(&id));
    }
    Ok(())
}

pub fn describe_park(report: &Value) -> String {
    format!("{}{}", closed_as(report, "parked"), left_running(report))
}

/// What a report says of the session the command ran in, which is never
/// stopped under it.
fn left_running(report: &Value) -> &'static str {
    if report["left"].as_array().is_some_and(|l| !l.is_empty()) {
        "; this session is left running: end it when you are finished"
    } else {
        ""
    }
}

/// The task's runs `varde run` launched that are still alive. A run
/// whose conversation ended, or whose process moved on to another run
/// (perhaps another task's), is not: its process is not this task's.
pub fn live_runs(store: &Store, id: &str) -> Result<Vec<(String, i64, i64)>> {
    let mut stmt = store.conn.prepare(
        "SELECT id, pid, pid_start FROM run
             WHERE task = ?1 AND launch = 'managed' AND pid > 0 AND exit_code IS NULL",
    )?;
    let mut live = Vec::new();
    for row in
        stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<i64>>(2)?.unwrap_or(0))))?
    {
        let (run, pid, start): (String, i64, i64) = row?;
        if process::alive(pid, start)
            && store::run_status(&store.conn, &run)? != Some(varde_core::status::Status::Exited)
            && !store::moved_on(&store.conn, &run)?
        {
            live.push((run, pid, start));
        }
    }
    Ok(live)
}

/// Stop the task's live runs, and return their ids and those of the runs
/// left running. Each gets SIGTERM now. Unless `wait`, a detached `varde
/// reap` waits and escalates to SIGKILL, so nothing interactive waits on
/// a session shutting down; with it, this does, so a worktree about to go
/// is no longer written to. Either way the reaper then closes the task's
/// tmux panes. `varde run` records the exit. The run this command itself
/// runs under, an agent closing its own task, is left: stopping it would
/// kill the command mid-call.
fn stop_sessions(store: &Store, id: &str, wait: bool) -> Result<(Vec<String>, Vec<String>)> {
    let me = std::process::id();
    let (own, live): (Vec<_>, Vec<_>) =
        live_runs(store, id)?.into_iter().partition(|(_, pid, start)| process::descends(me, (*pid, *start)));
    for (_, pid, _) in &live {
        unsafe { libc::kill(*pid as libc::pid_t, libc::SIGTERM) };
    }
    if wait {
        end(&live);
    }
    let ids = |runs: Vec<(String, i64, i64)>| runs.into_iter().map(|(run, _, _)| run).collect::<Vec<_>>();
    let (live, own) = (ids(live), ids(own));
    let mut args = vec!["reap", id];
    for run in &own {
        args.extend(["--spare", run]);
    }
    // Detached, so the wait for the sessions to go outlives this process
    // and the terminal it ran in.
    crate::detach(&args, "the session reaper")?;
    Ok((live, own))
}

/// Give the SIGTERM'd `runs` a few seconds to go, then SIGKILL any that
/// have not, and wait for them to go.
fn end(runs: &[(String, i64, i64)]) {
    let gone = |tries| {
        (0..tries).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            runs.iter().all(|(_, pid, start)| !process::alive(*pid, *start))
        })
    };
    if !runs.is_empty() && !gone(50) {
        for (_, pid, start) in runs {
            if process::alive(*pid, *start) {
                unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        gone(20);
    }
}

/// `varde reap <task>` (detached from `varde done`): end the task's
/// SIGTERM'd sessions, but the `spare` runs, and close the task's tmux
/// panes.
pub fn reap(paths: &Paths, reference: &str, spare: &[String]) -> Result<()> {
    let store = paths.open(Duration::from_secs(2))?;
    let id = task::resolve(&store, reference)?;
    let mut live = live_runs(&store, &id)?;
    live.retain(|(run, _, _)| !spare.contains(run));
    std::mem::drop(store);
    end(&live);
    // A pane is closed once `varde run` has recorded the exit and its shell
    // is back; one busy with something else is left. No tmux, nothing to do.
    let t = tmux::server();
    for _ in 0..30 {
        match t.close(&id) {
            Ok(0) | Err(_) => break,
            Ok(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    Ok(())
}

pub fn describe(report: &Value) -> String {
    describe_as(report, "done")
}

/// `abc123 is <verb>`, and what stopping its sessions did.
fn closed_as(report: &Value, verb: &str) -> String {
    let short = report["task"].as_str().map(task::short).unwrap_or_default();
    let what = if short.is_empty() { verb.to_string() } else { format!("{short} is {verb}") };
    match report["stopped"].as_array().map_or(0, Vec::len) {
        0 => what,
        1 => format!("{what}; its session was stopped"),
        n => format!("{what}; its {n} sessions were stopped"),
    }
}

/// `1 commit`, `2 commits`.
fn commits(n: u64) -> String {
    format!("{n} commit{}", if n == 1 { "" } else { "s" })
}

fn describe_as(report: &Value, verb: &str) -> String {
    let done = closed_as(report, verb);
    if let Some(w) = report["worktree"].as_object() {
        let s = |k: &str| w[k].as_str().unwrap_or("");
        let included: Vec<&str> = w["included"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let stranded: Vec<&str> = w
            .get("stranded")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let what = match w["action"].as_str() {
            Some("merged" | "removed") if w["pr"].is_object() => {
                format!(
                    "PR #{} was merged; worktree and {} removed",
                    w["pr"]["number"],
                    s("branch")
                )
            }
            Some("discarded") => match w["saved"].as_str() {
                Some(saved) => format!(
                    "worktree and {} discarded; the work is kept at {saved}, and starting the task again brings it back",
                    s("branch")
                ),
                None => "worktree discarded; there was no work in it to keep".to_string(),
            },
            Some("merged") => format!("{} had landed; worktree removed", s("branch")),
            Some("removed") if w["ahead"] == 0 && w["dirty"] == false && included.is_empty() => {
                format!("worktree and {} removed; nothing was in them", s("branch"))
            }
            Some("removed") => format!(
                "worktree removed{} ({} kept if unmerged)",
                if included.is_empty() { String::new() } else { format!(" with its changed {}", included.join(", ")) },
                s("branch")
            ),
            _ if !included.is_empty() => {
                let mut also = vec![];
                match w["ahead"].as_u64() {
                    Some(0) | None => {}
                    Some(n) => also.push(format!("{} ahead of {}", commits(n), s("base"))),
                }
                if w["dirty"] == true {
                    also.push("uncommitted changes".to_string());
                }
                format!(
                    "worktree kept at {}: its {} {} from the main checkout's{}; \
                     `varde done {} --remove --force` deletes {} with it",
                    s("path"),
                    included.join(", "),
                    if included.len() == 1 { "differs" } else { "differ" },
                    if also.is_empty() { String::new() } else { format!(" ({})", also.join(", ")) },
                    report["task"].as_str().map(task::short).unwrap_or_default(),
                    if included.len() == 1 { "it" } else { "them" },
                )
            }
            _ if !stranded.is_empty() => format!(
                "worktree kept at {}: it has {}, which removing it would destroy; push or move them, \
                 and it goes once its work lands",
                s("path"),
                stranded.join(" and ")
            ),
            // Its work has landed: only the session keeps it.
            _ if w["session"] == true && w["ahead"] == 0 && w["dirty"] == false => format!(
                "worktree kept at {} until the session in it has ended; its work has landed, so it goes then",
                s("path")
            ),
            _ => match w["ahead"].as_u64() {
                Some(ahead) => {
                    let mut why = vec![];
                    if ahead > 0 {
                        why.push(format!("{} ahead of {}", commits(ahead), s("base")));
                    }
                    if w["dirty"] == true {
                        why.push("uncommitted changes".to_string());
                    }
                    let why = if why.is_empty() { String::new() } else { format!(" ({})", why.join(", ")) };
                    format!("worktree kept at {} until its work lands{why}", s("path"))
                }
                None => format!(
                    "worktree kept at {} (its git state could not be read)",
                    s("path")
                ),
            },
        };
        format!("{done}; {what}{}", left_running(report))
    } else {
        format!("{done}{}", left_running(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_worktree_says_which_changed_files_went_with_it() {
        let report = |included: Value| {
            json!({"task": "01AAAAAAAAAAAAAAAAAAABCDEF", "stopped": [], "worktree": {"path": "/w",
                "branch": "varde/x", "base": "main", "ahead": 0, "dirty": false, "included": included,
                "action": "removed", "pr": null, "saved": null}})
        };
        assert!(describe(&report(json!([]))).contains("nothing was in them"));
        let said = describe(&report(json!([".env"])));
        assert!(said.contains("with its changed .env") && !said.contains("nothing"), "{said}");
    }

    #[test]
    fn a_worktree_kept_says_what_keeps_it() {
        let report = |session: bool, ahead: u32, included: Value| {
            describe(&json!({"task": "01AAAAAAAAAAAAAAAAAAABCDEF", "stopped": [], "worktree": {"path": "/w",
                "branch": "varde/x", "base": "main", "ahead": ahead, "dirty": false, "included": included,
                "action": "kept", "session": session, "pr": null, "saved": null}}))
        };
        let said = report(true, 0, json!([]));
        assert!(
            said.contains("until the session in it has ended; its work has landed")
                && !said.contains("until its work lands"),
            "{said}"
        );
        let said = report(true, 2, json!([]));
        assert!(said.contains("until its work lands (2 commits ahead of main)"), "{said}");
        let said = report(false, 0, json!([".env"]));
        assert!(
            said.contains("its .env differs from the main checkout's;") && !said.contains("ahead"),
            "{said}"
        );
    }
}
