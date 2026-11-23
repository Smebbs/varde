//! Checks, loops and trails (`docs/proposal-loops-and-graphs.md`): the
//! commands, the check varde runs itself (`varde verify`), and what the
//! tick starts. What is derived from them lives in `varde_core::flow`.
//!
//! A check runs detached, never in a hook: when a session of a task with
//! a check ends, `varde run` starts `varde verify`, which runs the
//! command in the task's checkout, records what it found, and wakes the
//! tick. The tick then starts a loop's next round, or a trail's next
//! task, in the background, as a schedule starts one.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use varde_core::flow::{self as core, State, Verdict};
use varde_core::plan::Want;
use varde_core::store::Store;
use varde_core::task;

use crate::{config, now_ms, start, Paths};

const BUSY: Duration = Duration::from_secs(2);

/// The most of a check's output a round is shown.
const TAIL: usize = 3000;

/// The most of a diff a review reads.
const DIFF: usize = 60_000;

// ---------------------------------------------------------------- where

/// Where a task's work is: its worktree, the one it shares, where its
/// newest session ran, else its project.
pub fn checkout(store: &Store, task: &str) -> Result<Option<PathBuf>> {
    if let Some((path, ..)) = crate::sweep::active_worktree(store, task)? {
        return Ok(Some(path.into()));
    }
    let (share, project): (Option<String>, Option<String>) =
        store.conn.query_row("SELECT share, project FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if let Some(up) = share.filter(|u| u != task) {
        return checkout(store, &up);
    }
    let cwd: Option<String> = store
        .conn
        .query_row(
            "SELECT cwd FROM run WHERE task = ?1 ORDER BY started DESC, rowid DESC LIMIT 1",
            [task],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(cwd.or(project).map(PathBuf::from).filter(|p| p.is_dir()))
}

/// The tree a checkout holds now, uncommitted and untracked files
/// included (ignored ones not): what `git add -A` would commit, written
/// with a scratch index so the checkout's own is left alone. `None`
/// outside git.
pub fn tree(dir: &Path) -> Option<String> {
    let root = varde_core::project::root(dir);
    if !root.join(".git").exists() {
        return None;
    }
    let scratch = tempfile::NamedTempFile::new().ok()?;
    // Starting from the real index keeps `add` to what changed.
    let real = crate::git::bounded(&mut crate::git::command(
        &root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    ))
    .ok()?;
    let real = PathBuf::from(String::from_utf8_lossy(&real.stdout).trim());
    if real.is_file() {
        std::fs::copy(&real, scratch.path()).ok()?;
    }
    let add = crate::git::add_all(&root);
    let ok = crate::git::bounded(
        crate::git::command(&root, &add.iter().map(String::as_str).collect::<Vec<_>>())
            .env("GIT_INDEX_FILE", scratch.path()),
    )
    .ok()?
    .status
    .success();
    if !ok {
        return None;
    }
    let out = crate::git::bounded(
        crate::git::command(&root, &["write-tree"]).env("GIT_INDEX_FILE", scratch.path()),
    )
    .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The paths that differ between two trees.
/// The checkout `dir`, whose files are the tree `now`, put back to the
/// tree `was`: its files and their modes as they were, and those added
/// since removed.
fn put_back(dir: &Path, was: &str, now: &str) -> bool {
    let Ok(scratch) = tempfile::NamedTempFile::new() else { return false };
    let with = |args: &[&str]| {
        crate::git::bounded(crate::git::command(dir, args).env("GIT_INDEX_FILE", scratch.path()))
            .is_ok_and(|o| o.status.success())
    };
    // What it added goes first, a file where a directory was included;
    // then every file as it was. Nothing through a link.
    let added = crate::git::bounded(&mut crate::git::command(
        dir,
        &["diff-tree", "-r", "--name-only", "--diff-filter=A", was, now],
    ))
    .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect::<Vec<_>>())
    .unwrap_or_default();
    for f in &added {
        let p = dir.join(f);
        let gone = match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&p),
            Ok(_) => std::fs::remove_file(&p),
            Err(_) => Ok(()),
        };
        if gone.is_err() {
            return false;
        }
    }
    with(&["read-tree", was]) && with(&["checkout-index", "-a", "-f"])
}

fn changed(dir: &Path, a: &str, b: &str) -> Vec<String> {
    crate::git::bounded(&mut crate::git::command(dir, &["diff-tree", "-r", "--name-only", a, b]))
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- check

/// A task's check, as set.
#[derive(Debug, Clone)]
pub struct Def {
    pub cmd: Option<String>,
    pub review: bool,
    pub rubric: Option<String>,
    pub reviewer: Option<String>,
    /// What an agent session is asked to verify: a file, or the text.
    pub verify: Option<String>,
    pub guard: Vec<String>,
    pub again: u32,
    pub base_tree: Option<String>,
}

pub fn def(store: &Store, task: &str) -> Result<Option<Def>> {
    Ok(store
        .conn
        .query_row(
            "SELECT cmd, review, rubric, reviewer, guard, again, base_tree, verify FROM check_def WHERE task = ?1",
            [task],
            |r| {
                Ok(Def {
                    cmd: r.get(0)?,
                    review: r.get(1)?,
                    rubric: r.get(2)?,
                    reviewer: r.get(3)?,
                    guard: r
                        .get::<_, Option<String>>(4)?
                        .and_then(|g| serde_json::from_str(&g).ok())
                        .unwrap_or_default(),
                    again: r.get::<_, i64>(5)?.max(1) as u32,
                    base_tree: r.get(6)?,
                    verify: r.get(7)?,
                })
            },
        )
        .optional()?)
}

/// Set a task's check, keeping what is not given.
pub fn set_def(store: &Store, task: &str, d: &Def) -> Result<()> {
    store.conn.execute(
        "INSERT INTO check_def (task, cmd, review, rubric, reviewer, guard, again, base_tree, created, verify)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (task) DO UPDATE SET cmd = ?2, review = ?3, rubric = ?4, reviewer = ?5, guard = ?6,
           again = ?7, base_tree = coalesce(?8, base_tree), verify = ?10",
        params![
            task,
            d.cmd,
            d.review,
            d.rubric,
            d.reviewer,
            (!d.guard.is_empty()).then(|| serde_json::to_string(&d.guard).unwrap_or_default()),
            d.again,
            d.base_tree,
            now_ms(),
            d.verify
        ],
    )?;
    Ok(())
}

/// What `varde check` was asked.
pub struct CheckArgs {
    pub cmd: Vec<String>,
    pub review: Option<Option<String>>,
    pub rubric: Option<String>,
    pub verify: Option<String>,
    pub guard: Vec<String>,
    pub again: Option<u32>,
    pub now: bool,
    pub off: bool,
}

/// `varde check <task> …`.
pub fn check(paths: &Paths, reference: &str, a: CheckArgs) -> Result<()> {
    let changes = !a.cmd.is_empty()
        || a.review.is_some()
        || a.rubric.is_some()
        || !a.guard.is_empty()
        || a.again.is_some()
        || a.verify.is_some()
        || a.now
        || a.off;
    if changes {
        crate::sandbox::not_inside("Setting or running a task's check")?;
    }
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    crate::sandbox::own_project(&store, &id)?;
    let short = task::short(&id);
    if a.off {
        let gone = store.conn.execute("DELETE FROM check_def WHERE task = ?1", [&id])?;
        println!(
            "{}",
            if gone > 0 { format!("{short} has no check now") } else { format!("{short} had no check") }
        );
        return Ok(());
    }
    let asked = !a.cmd.is_empty()
        || a.review.is_some()
        || a.rubric.is_some()
        || a.verify.is_some()
        || !a.guard.is_empty()
        || a.again.is_some();
    let mut d = def(&store, &id)?.unwrap_or(Def {
        cmd: None,
        review: false,
        rubric: None,
        reviewer: None,
        verify: None,
        guard: vec![],
        again: 1,
        base_tree: None,
    });
    if asked {
        if !a.cmd.is_empty() {
            d.cmd = Some(a.cmd.join(" "));
        }
        if let Some(reviewer) = a.review {
            d.review = true;
            d.reviewer = reviewer.or(d.reviewer);
        }
        if let Some(r) = a.rubric {
            d.rubric = Some(rubric_arg(&r)?);
            d.review = true;
        }
        if let Some(v) = a.verify {
            d.verify = Some(rubric_arg(&v)?).filter(|v| !v.trim().is_empty());
        }
        if !a.guard.is_empty() {
            d.guard = a.guard;
        }
        if let Some(n) = a.again {
            d.again = n.max(1);
        }
        if d.cmd.is_none() && !d.review && d.verify.is_none() {
            bail!("a check needs a command (varde check {short} -- CMD), a review (--review) or a verifier (--verify)");
        }
        if d.base_tree.is_none() {
            d.base_tree = checkout(&store, &id)?.and_then(|c| tree(&c));
        }
        set_def(&store, &id, &d)?;
        println!("{short}'s check: {}", describe(&d));
    } else if !a.now {
        match def(&store, &id)? {
            None => println!("{short} has no check; varde check {short} -- CMD sets one"),
            Some(d) => {
                println!("{short}'s check: {}", describe(&d));
                if let Some(c) = core::last_check(&store.conn, &id)? {
                    println!("last: {}", result_line(&c, now_ms()));
                }
            }
        }
    }
    if a.now {
        if def(&store, &id)?.is_none() {
            bail!("{short} has no check; varde check {short} -- CMD sets one");
        }
        drop(store);
        verify(paths, &id, None, true)?;
        let store = paths.open(BUSY)?;
        if let Some(c) = core::last_check(&store.conn, &id)? {
            println!("{}", result_line(&c, now_ms()));
            if let Some(o) = c.output.as_deref() {
                println!("output: {o}");
            }
        }
    }
    Ok(())
}

/// A rubric given as a file is kept as its path, made absolute; as text,
/// as the text.
/// A rubric or a verifier's ask, as kept: a file's text, read now, or
/// the text given. Kept as text, never as a name read later, which what
/// a session writes could then stand in for.
pub fn rubric_arg(r: &str) -> Result<String> {
    let p = Path::new(r);
    Ok(if p.is_file() { std::fs::read_to_string(p)? } else { r.to_string() })
}

/// `cargo test, then a review by review; guarding tests/**`.
pub fn describe(d: &Def) -> String {
    let mut parts = vec![];
    match (&d.cmd, d.again) {
        (Some(c), 1) => parts.push(format!("`{c}`")),
        (Some(c), n) => parts.push(format!("`{c}`, {n} passes in a row")),
        (None, _) => {}
    }
    if d.review {
        let who = d.reviewer.clone().unwrap_or_else(|| config::get().roles.review.clone());
        parts.push(format!("{}a review by {who}", if d.cmd.is_some() { "then " } else { "" }));
    }
    if d.verify.is_some() {
        let who = d.reviewer.clone().unwrap_or_else(|| config::get().roles.review.clone());
        parts.push(format!("{}verified by an agent on {who}", if parts.is_empty() { "" } else { "then " }));
    }
    let mut out = parts.join(", ");
    if !d.guard.is_empty() {
        out.push_str(&format!("; guarding {}", d.guard.join(" ")));
    }
    out
}

/// `✗ fail · 3 failing · 2m ago`.
pub fn result_line(c: &core::Check, now: i64) -> String {
    let mark = match c.verdict {
        Verdict::Pass => "✓",
        Verdict::Fail | Verdict::Error => "✗",
        Verdict::Flagged => "⚑",
        Verdict::Unchecked => "…",
    };
    let what = c.summary.as_deref().map(|s| format!(" · {s}")).unwrap_or_default();
    format!("{mark} {}{what} · {} ago", c.verdict.name(), crate::list::age(now - c.finished))
}

/// The lock that keeps one check of a task running at a time. `None`
/// when another holds it, which is then told to run once more.
fn check_lock(paths: &Paths, task: &str) -> Result<Option<File>> {
    let dir = paths.home.join("checks");
    std::fs::create_dir_all(&dir)?;
    let file = File::create(dir.join(format!("{task}.lock")))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        File::create(dir.join(format!("{task}.again")))?;
        return Ok(None);
    }
    Ok(Some(file))
}

/// Whether a check of `task` is running.
pub fn checking(paths: &Paths, task: &str) -> bool {
    let Ok(file) = File::open(paths.home.join("checks").join(format!("{task}.lock"))) else { return false };
    let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if free {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    !free
}

/// `varde verify <task> [--run RUN]`: run the task's check once (more,
/// if asked again meanwhile), record it, and wake the tick. `wait` runs
/// it here, as `check --now` does, rather than as the detached job.
pub fn verify(paths: &Paths, task: &str, run: Option<String>, wait: bool) -> Result<()> {
    crate::sandbox::not_inside("Checking a task")?;
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, task)?;
    // A turn of a session of yours that changed no file is no round: a
    // question answered, a plan talked through.
    // Nor is one on a loop that is over: it passed, or ran out, and is
    // yours again.
    if let Some(r) = &run {
        if attended_turn(&store.conn, r)?.is_some() {
            let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
            let going = matches!(
                core::state(&store.conn, &id, now_ms(), &alive)?,
                Some(State::Due { .. } | State::Checking { .. } | State::Attended { .. })
            );
            let now = checkout(&store, &id)?.and_then(|c| tree(&c));
            let last = core::checks(&store.conn, &id, 0)?
                .into_iter()
                .rev()
                .find(|c| matches!(c.verdict, Verdict::Pass | Verdict::Fail | Verdict::Flagged))
                .and_then(|c| c.tree);
            if !going || (now.is_some() && now == last) {
                // The session may have ended with it: what comes next is the tick's.
                let _ = crate::detach(&["tick"], "the tick");
                return Ok(());
            }
        }
    }
    drop(store);
    let Some(_lock) = check_lock(paths, &id)? else {
        if wait {
            bail!(
                "a check of {} is running; varde check {} shows it when it is done",
                task::short(&id),
                task::short(&id)
            );
        }
        return Ok(());
    };
    let again = paths.home.join("checks").join(format!("{id}.again"));
    loop {
        let _ = std::fs::remove_file(&again);
        once(paths, &id, run.clone())?;
        if !again.exists() {
            break;
        }
    }
    if let Some(r) = &run {
        attend(paths, &id, r)?;
    }
    let _ = crate::detach(&["tick"], "the tick");
    Ok(())
}

fn once(paths: &Paths, task: &str, run: Option<String>) -> Result<()> {
    let store = paths.open(BUSY)?;
    // A check asked for (`check --now`) is of no session: not a round.
    let Some(d) = def(&store, task)? else { return Ok(()) };
    let started = now_ms();
    let dir = checkout(&store, task)?;
    // In the task's sandbox, as its sessions are: what a session wrote
    // runs here, a build script or a test.
    let plan = crate::sandbox::plan(&store, task);
    drop(store);
    let out_path = paths.home.join("checks").join(format!("{}.log", ulid::Ulid::new()));
    // Read back for the summary, so open for both.
    let mut out =
        std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&out_path)?;
    let mut result = Found { verdict: Verdict::Pass, exit: None, summary: None, fingerprint: None };
    let mut after = None;
    match &dir {
        None => {
            result = Found::error("the task has no checkout to check");
        }
        Some(dir) => {
            let fence = match plan {
                Ok(crate::sandbox::Plan { policy: Some(p), fenced: true }) => {
                    crate::sandbox::wrap(&p, dir, None, &paths.home, None).map(Some)
                }
                Ok(_) => Ok(None),
                Err(e) => Err(e),
            };
            // A repository a sandboxed session left in its checkout runs
            // what its config names in any git run there: a prompt's, an
            // editor's. Held (its `.git` renamed) before any git runs,
            // and the loop stops, saying where.
            let fenced = matches!(fence, Ok(Some(_)));
            let mut held = if fenced { hold(dir) } else { vec![] };
            let before = tree(dir);
            match (&d.cmd, &fence) {
                (_, Err(e)) => result = Found::error(&format!("no sandbox to check it in: {e:#}")),
                (Some(cmd), Ok(fence)) => {
                    result = run_command(dir, cmd, d.again, fence.as_ref(), &mut out)?;
                    if let Some(f) = fence {
                        crate::sandbox::import(&f.1, dir);
                        // What the command made is the session's too.
                        held.extend(hold(dir));
                    }
                }
                (None, Ok(_)) => {}
            }
            after = tree(dir);
            if result.verdict != Verdict::Error && before != after {
                result = Found {
                    verdict: Verdict::Unchecked,
                    exit: result.exit,
                    summary: Some("files changed while it ran".into()),
                    fingerprint: None,
                };
            }
            if let (Some(base), Some(now)) = (&d.base_tree, &after) {
                let hit: Vec<String> = changed(dir, base, now)
                    .into_iter()
                    .filter(|p| d.guard.iter().any(|g| varde_core::steward::glob(g, p)))
                    .collect();
                if !hit.is_empty() {
                    let mut names = hit.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                    if hit.len() > 3 {
                        names.push_str(&format!(" and {} more", hit.len() - 3));
                    }
                    result = Found {
                        verdict: Verdict::Flagged,
                        exit: result.exit,
                        summary: Some(format!("touched {names}")),
                        fingerprint: None,
                    };
                }
            }
            if !held.is_empty() {
                result = Found {
                    verdict: Verdict::Flagged,
                    exit: result.exit,
                    summary: Some(format!(
                        "left what git would run as you at {}: set aside (*.varde-held, links removed), look before you use it",
                        held.join(", ")
                    )),
                    fingerprint: None,
                };
            }
            if result.verdict == Verdict::Pass && d.review {
                let store = paths.open(BUSY)?;
                result = review(paths, &store, task, &d, dir, after.as_deref(), &out_path, &mut out)?;
            }
            if result.verdict == Verdict::Pass && d.verify.is_some() {
                let store = paths.open(BUSY)?;
                result = verifier(paths, &store, task, &d, dir, &mut out)?;
                let more = if fenced { hold(dir) } else { vec![] };
                if !more.is_empty() {
                    result = Found {
                        verdict: Verdict::Flagged,
                        exit: None,
                        summary: Some(format!(
                            "the verifier left what git would run as you at {}: set aside",
                            more.join(", ")
                        )),
                        fingerprint: None,
                    };
                }
                // It looks, and what it touched to look (a mode, a scratch
                // file) is put back: its verdict is of the work as it was.
                let now = tree(dir);
                if now.is_none() && after.is_some() {
                    result = Found {
                        verdict: Verdict::Unchecked,
                        exit: None,
                        summary: Some("after the verifier, its files could not be read".into()),
                        fingerprint: None,
                    };
                }
                if let (Some(was), Some(now)) = (&after, &now) {
                    if was != now {
                        let touched = changed(dir, was, now);
                        let back = put_back(dir, was, now);
                        writeln!(
                            out,
                            "\n== the verifier changed {}; {} ==",
                            touched.join(", "),
                            if back { "put back as they were" } else { "they could not be put back" }
                        )?;
                        if !back || tree(dir).as_ref() != Some(was) {
                            result = Found {
                                verdict: Verdict::Unchecked,
                                exit: None,
                                summary: Some(
                                    "the verifier changed files, which could not be put back".into(),
                                ),
                                fingerprint: None,
                            };
                        }
                    }
                }
            }
        }
    }
    let store = paths.open(BUSY)?;
    // Files that keep changing as it is checked: twice is an error, which
    // stops a loop, not a check run again and again.
    if result.verdict == Verdict::Unchecked {
        let last: Option<String> = store
            .conn
            .query_row(
                "SELECT verdict FROM check_result WHERE task = ?1 AND run IS ?2 ORDER BY id DESC LIMIT 1",
                params![task, run],
                |r| r.get(0),
            )
            .optional()?;
        if last.as_deref() == Some("unchecked") {
            result = Found::error(&format!(
                "{}, twice: what changes them must stop first",
                result.summary.clone().unwrap_or_else(|| "files changed while it ran".into())
            ));
        }
    }
    store.conn.execute(
        "INSERT INTO check_result (task, run, started, finished, verdict, exit_code, tree, fingerprint, output, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            task,
            run,
            started,
            now_ms(),
            result.verdict.name(),
            result.exit,
            after,
            result.fingerprint,
            out_path.display().to_string(),
            result.summary
        ],
    )?;
    if let Some(t) = &after {
        // What the board compares a check's tree with, until the refresh looks again.
        store.conn.execute(
            "INSERT INTO git_state (task, tree, checked) VALUES (?1, ?2, 0)
             ON CONFLICT (task) DO UPDATE SET tree = ?2",
            params![task, t],
        )?;
    }
    Ok(())
}

struct Found {
    verdict: Verdict,
    exit: Option<i32>,
    summary: Option<String>,
    fingerprint: Option<String>,
}

impl Found {
    fn error(why: &str) -> Found {
        Found { verdict: Verdict::Error, exit: None, summary: Some(why.into()), fingerprint: None }
    }
}

/// Run the command `again` times, stopping at the first failure, in the
/// sandbox `fence` if given.
fn run_command(
    dir: &Path,
    cmd: &str,
    again: u32,
    fence: Option<&crate::sandbox::Wrap>,
    out: &mut File,
) -> Result<Found> {
    let limit = Duration::from_secs(config::get().loops.check_minutes.max(1) * 60);
    let mut last = Found { verdict: Verdict::Pass, exit: Some(0), summary: None, fingerprint: None };
    for i in 0..again {
        if again > 1 {
            writeln!(out, "== pass {} of {again} ==", i + 1)?;
        }
        let mut argv: Vec<String> = fence.map(|f| f.0.clone()).unwrap_or_default();
        argv.extend(["sh".to_string(), "-c".to_string(), cmd.to_string()]);
        let mut command = Command::new(&argv[0]);
        if let Some(f) = fence {
            command.env_clear().envs(f.1.clone());
        }
        let mut child = match command
            .args(&argv[1..])
            .current_dir(dir)
            .env("VARDE_INTERNAL", "1")
            .env_remove("VARDE_RUN")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out.try_clone()?)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return Ok(Found::error(&format!("could not start `{cmd}`: {e}"))),
        };
        let started = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if started.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(Found::error(&format!("timed out after {} min", limit.as_secs() / 60)));
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        let code = status.code().unwrap_or(-1);
        // 127: the shell found no such command.
        if code == 127 {
            return Ok(Found::error(&format!("`{cmd}` was not found")));
        }
        if code != 0 {
            out.flush()?;
            let text = tail_of(out, 8000);
            let summary = summarize(&text).unwrap_or_else(|| format!("exit {code}"));
            let summary = if again > 1 { format!("{summary} (pass {} of {again})", i + 1) } else { summary };
            return Ok(Found {
                verdict: Verdict::Fail,
                exit: Some(code),
                summary: Some(summary),
                fingerprint: Some(fingerprint(&text)),
            });
        }
        last.exit = Some(code);
    }
    Ok(last)
}

/// Hold every repository a sandboxed session left in the checkout `dir`:
/// its `.git` renamed, so no git run there reads its config. The paths,
/// held or not: one that could not be is flagged all the same. The
/// checkout is the one marked before the session ran that holds `at`,
/// whatever directory below its top it ran in.
pub(crate) fn hold(at: &Path) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let top = crate::git::untrusted_root(at).unwrap_or_else(|| at.to_path_buf());
    let dir = top.as_path();
    let mut found = vec![];
    // As text: the names are the session's to choose.
    let rel = |p: &Path| plain(&p.strip_prefix(dir).unwrap_or(p).display().to_string());
    let there = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    // Rename `from` to a name of its own, though the session took the first.
    let set_aside = |from: &Path| -> PathBuf {
        let parent = from.parent().unwrap_or(dir);
        let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let to = (0..)
            .map(|n| {
                parent.join(if n == 0 {
                    format!("{name}.varde-held")
                } else {
                    format!("{name}.varde-held.{n}")
                })
            })
            .find(|t| std::fs::symlink_metadata(t).is_err())
            .expect("a free name");
        if std::fs::rename(from, &to).is_err() {
            // A directory it made read-only, to keep it: made writable.
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::rename(from, &to);
        }
        to
    };
    let (gd, common) = git_dirs(dir);
    // A worktree's own git directory, which the session could write (a
    // main checkout's it cannot): what it left there for git to wait on
    // (a pipe for HEAD or the index) goes before git runs. And where its
    // branch is, which it could write too.
    let linked = matches!((&gd, &common), (Some(g), Some(c)) if g != c);
    let prefix = config::get().branch_prefix().trim_end_matches('/').to_string();
    let mut swept = gd.iter().filter(|_| linked).cloned().collect::<Vec<_>>();
    if let (Some(c), false) = (&common, prefix.is_empty()) {
        swept.extend([c.join("refs/heads").join(&prefix), c.join("logs/refs/heads").join(&prefix)]);
    }
    for d in &swept {
        match crate::git::sweep(d) {
            (0, _) => {}
            (_, 0) => {
                found.push("what it left in its git directory for git to wait on or write through".into())
            }
            (_, _) => found.push(
                "what it left in its git directory for git to wait on or write through (still there)".into(),
            ),
        }
    }
    // Its branches: each a commit, never a name of another (`ref: …`),
    // which your next commit there would move.
    let mut branches: Vec<String> = vec![];
    if let (Some(c), false) = (&common, prefix.is_empty()) {
        let mut stack = vec![c.join("refs/heads").join(&prefix)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let Ok(t) = e.file_type() else { continue };
                if t.is_dir() {
                    stack.push(e.path());
                    continue;
                }
                let text = crate::git::small_text(&e.path()).unwrap_or_default();
                let id = text.trim();
                if !((id.len() == 40 || id.len() == 64) && id.chars().all(|c| c.is_ascii_hexdigit())) {
                    let _ = std::fs::remove_file(e.path());
                    found.push(format!("a branch of varde's that names {}", plain(id)));
                } else {
                    branches.push(id.to_string());
                }
            }
        }
    }
    let real = |p: &Path| p.canonicalize().ok();
    // Where git keeps submodules: the repository's, and a worktree's own.
    let modules: Vec<PathBuf> =
        gd.iter().chain(common.iter()).filter_map(|g| real(&g.join("modules"))).collect();
    let sandboxed = crate::sandbox::writable(dir);
    let root = real(dir).unwrap_or(dir.to_path_buf());
    // A directory git would take for a repository, or take one from.
    let bare = |d: &Path| {
        there(&d.join("HEAD"))
            && (there(&d.join("commondir")) || (there(&d.join("objects")) && there(&d.join("refs"))))
    };
    // Whether a link at `d` to `l` leads out of the checkout to where a
    // sandbox writes or into a git directory (any worktree's is written by
    // its session): judged at every step of the way as written, not only
    // where it ends (a `..` after a place a sandbox writes is undone there
    // by a link it makes), and as the kernel would follow each. One whose
    // meaning is the follower's (its cwd, its fds) is out too.
    let theirs = |p: &Path| {
        let inside = p.starts_with(dir) || p.starts_with(&root);
        let git = p.ancestors().any(|a| {
            a.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(".git")) || bare(a)
        });
        !inside && (git || sandboxed.iter().any(|w| p.starts_with(w)))
    };
    // (A step on the way to the checkout itself is not one.)
    let on_way = |t: &Path| dir.starts_with(t) || root.starts_with(t);
    let leads_out = |d: &Path, l: &Path| {
        let whole = lexical(&d.join(l));
        let mut at = d.to_path_buf();
        whole.starts_with("/proc")
            || whole.starts_with("/dev/fd")
            || l.components().any(|c| {
                at.push(c);
                let t = lexical(&at);
                !on_way(&t) && (theirs(&t) || theirs(&resolved(&t)))
            })
    };
    // Every directory, walked: no link followed, nothing passed over for
    // being ignored or unreadable, as git finds a repository wherever one is.
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => {
                // One it made unreadable, to hide what is in it: opened.
                let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
                match std::fs::read_dir(&d) {
                    Ok(e) => e,
                    Err(_) => {
                        found.push(format!("{} (could not be looked into)", rel(&d)));
                        continue;
                    }
                }
            }
        };
        // A repository as git finds one: HEAD, and objects and refs (or a
        // commondir naming them), whatever kind of file each is.
        if d != dir && bare(&d) {
            set_aside(&d.join("HEAD"));
            found.push(rel(&d));
        }
        for e in entries.flatten() {
            let path = e.path();
            let Ok(t) = e.file_type() else { continue };
            // A pipe: git would wait on it for ever.
            if std::os::unix::fs::FileTypeExt::is_fifo(&t) {
                if std::fs::remove_file(&path).is_err() {
                    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
                    if std::fs::remove_file(&path).is_err() {
                        found.push(format!("{} (a pipe that could not be removed)", rel(&path)));
                    }
                }
                continue;
            }
            // `.git` as git finds it, on a disk that ignores case too.
            if e.file_name().to_string_lossy().eq_ignore_ascii_case(".git") {
                if d == dir {
                    continue;
                }
                // A submodule git keeps in the repository's own `modules/`.
                if t.is_file() && points_into(&path, &modules) {
                    continue;
                }
                let held = set_aside(&path);
                found.push(rel(&d));
                if std::fs::symlink_metadata(&held).is_ok_and(|m| m.is_dir()) {
                    stack.push(held);
                }
                continue;
            }
            // A link out of the checkout to where a sandbox writes (its git
            // directory, a cache, temporary files), to whatever is there
            // now or once filled: another session's sandbox writes there
            // too. git run where it leads, or an editor's for a file there,
            // would run what it finds. Removed, not renamed: by any name it
            // leads there. As written, and as it resolves.
            if t.is_symlink() {
                // Or one that climbs out of the checkout as written, which
                // from the main checkout, once committed, leads elsewhere.
                let at = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
                let out =
                    std::fs::read_link(&path).is_ok_and(|l| leads_out(&d, &l) || leaves(&at, &l, false));
                if out {
                    if std::fs::remove_file(&path).is_err() {
                        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
                        let _ = std::fs::remove_file(&path);
                    }
                    found.push(rel(&path));
                }
                continue;
            }
            if t.is_dir() {
                stack.push(path);
            }
        }
    }
    // A worktree's own git directory: a rebase or cherry-pick it left
    // (whose steps, `exec` among them, your `--continue` would run) set
    // aside, and a HEAD that names neither a branch nor a commit flagged
    // (git writes the name where a reflog is).
    if let (Some(gd), true) = (&gd, linked) {
        for name in ["rebase-merge", "rebase-apply", "sequencer"] {
            let p = gd.join(name);
            if there(&p) {
                set_aside(&p);
                let left = if there(&p) { " (still there)" } else { "" };
                found.push(format!("a {name} in its git directory{left}"));
            }
        }
        // Its index, which `git restore` writes out: no path in it out of
        // the checkout or into a `.git`, as git itself would refuse to add.
        let listed = crate::git::bounded(&mut crate::git::command(dir, &["ls-files", "-z"]));
        let bad = listed.is_ok_and(|o| {
            o.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).any(|p| {
                let p = String::from_utf8_lossy(p);
                p.starts_with('/')
                    || p.split('/')
                        .any(|c| c.is_empty() || c == "." || c == ".." || c.eq_ignore_ascii_case(".git"))
            })
        });
        // And no link in it, or in what it committed, that would lead out
        // of the repository once git writes it (a checkout, a merge, `git
        // restore`), wherever that checkout is: judged by what it says,
        // not by where it leads from here. Each listing's links, as (path,
        // blob): `ls-files -s` (the blob its second field) or `ls-tree -r`
        // (its third); `None` for one that could not be read.
        // More than a repository has (100 000) is not read on.
        let listing = |at: &Path, args: &[&str], field: usize| -> Option<Vec<(String, String)>> {
            let o = crate::git::bounded(&mut crate::git::command(at, args))
                .ok()
                .filter(|o| o.status.success())?;
            let all: Vec<(String, String)> = String::from_utf8_lossy(&o.stdout)
                .split('\0')
                .filter_map(|e| e.split_once('\t'))
                // A link by its type bits, whatever else its mode says (an
                // index it wrote can say `120755`, and git writes a link).
                .filter(|(meta, _)| {
                    meta.split(' ')
                        .next()
                        .and_then(|m| u32::from_str_radix(m, 8).ok())
                        .is_some_and(|m| m & 0o170000 == 0o120000)
                })
                .filter_map(|(meta, path)| Some((path.to_string(), meta.split(' ').nth(field)?.to_string())))
                .take(100_001)
                .collect();
            (all.len() <= 100_000).then_some(all)
        };
        // Those the repository's own branch has already are yours, in your
        // checkout already: only the new are judged.
        // Not when that branch is one a session can move (a varde one, the
        // main checkout on it to look), by any case of its name.
        let repo = common.clone().unwrap_or(gd.clone());
        // As git reads it (`symbolic-ref`, through any chain): detached
        // (exit 1) is a commit no session moves; one it cannot say, movable.
        let on = crate::git::bounded(&mut crate::git::command(&repo, &["symbolic-ref", "-q", "HEAD"]));
        let movable = match on {
            Ok(o) if o.status.success() => {
                let h = String::from_utf8_lossy(&o.stdout).trim().to_ascii_lowercase();
                prefix.is_empty() || h.starts_with(&format!("refs/heads/{}/", prefix.to_ascii_lowercase()))
            }
            Ok(o) => o.status.code() != Some(1),
            Err(_) => true,
        };
        let yours: std::collections::HashSet<(String, String)> = match movable {
            true => Default::default(),
            false => {
                listing(&repo, &["ls-tree", "-r", "-z", "HEAD"], 2).unwrap_or_default().into_iter().collect()
            }
        };
        let links = |at: &Path, args: &[&str], field: usize| {
            listing(at, args, field).map(|l| l.into_iter().filter(|e| !yours.contains(e)).collect::<Vec<_>>())
        };
        // Those that lead out. A target is read only when small (a path):
        // sizes first, then the small ones in one go, each as git and the
        // kernel take it, up to a NUL. One larger, or missing, is out.
        let out_of = |links: Option<Vec<(String, String)>>| -> Vec<String> {
            let Some(links) = links else { return vec!["(a listing that could not be read)".into()] };
            let batch = |format: &str, oids: &[&str]| {
                let input: String = oids.iter().map(|o| format!("{o}\n")).collect();
                let mut c = crate::git::command(dir, &["cat-file", format]);
                crate::git::bounded_input(&mut c, input.as_bytes()).map(|o| o.stdout).unwrap_or_default()
            };
            let mut oids: Vec<&str> = links.iter().map(|(_, o)| o.as_str()).collect();
            oids.sort();
            oids.dedup();
            let sizes = String::from_utf8_lossy(&batch("--batch-check=%(objectname) %(objectsize)", &oids))
                .into_owned();
            let small: Vec<&str> = sizes
                .lines()
                .filter_map(|l| l.split_once(' '))
                .filter(|(_, size)| size.parse::<usize>().is_ok_and(|n| n <= 4096))
                .map(|(oid, _)| oid)
                .collect();
            let read = batch("--batch", &small);
            let mut targets = std::collections::HashMap::new();
            let mut rest = read.as_slice();
            while let Some(nl) = rest.iter().position(|b| *b == b'\n') {
                let head = String::from_utf8_lossy(&rest[..nl]).into_owned();
                rest = &rest[nl + 1..];
                let mut f = head.split(' ');
                let (Some(oid), Some(_), Some(size)) = (f.next(), f.next(), f.next()) else { continue };
                let Ok(size) = size.parse::<usize>() else { break };
                let body = &rest[..size.min(rest.len())];
                let cut = body.split(|b| *b == 0).next().unwrap_or_default();
                targets.insert(oid.to_string(), std::ffi::OsStr::from_bytes(cut).to_os_string());
                rest = &rest[(size + 1).min(rest.len())..];
            }
            links
                .into_iter()
                .filter(|(path, oid)| {
                    targets.get(oid).is_none_or(|t| leaves(Path::new(path), Path::new(t), true))
                })
                .map(|(path, _)| plain(&path))
                .collect()
        };
        let staged = out_of(links(dir, &["ls-files", "-s", "-z"], 1));
        // What it committed: on HEAD and on every branch of varde's, any of
        // which a merge brings in.
        // Read in the repository's own git directory, which the session
        // cannot write (a HEAD it broke stops no git there), HEAD as its
        // file says. Each tree once, though many branches have it, named in
        // one go; one not there (lost, or never brought in) said, as is a
        // listing that could not be read.
        let head_file = crate::git::small_text(&gd.join("HEAD")).unwrap_or_default();
        let head_rev = match head_file.trim().strip_prefix("ref: ") {
            Some(r) => r.to_string(),
            None => head_file.trim().to_string(),
        };
        let revs: Vec<String> = std::iter::once(head_rev).chain(branches.iter().cloned()).collect();
        let input: String = revs.iter().map(|b| format!("{b}^{{tree}}\n")).collect();
        let mut c = crate::git::command(&repo, &["cat-file", "--batch-check=%(objectname)"]);
        let named = crate::git::bounded_input(&mut c, input.as_bytes()).map(|o| o.stdout).unwrap_or_default();
        let named = String::from_utf8_lossy(&named).into_owned();
        let named: Vec<&str> = named.lines().collect();
        let mut trees: Vec<String> =
            named.iter().filter(|l| !l.ends_with(" missing")).map(|l| l.to_string()).collect();
        // An unborn HEAD names nothing yet; any other not there is a loss.
        let unborn = !gd.join("HEAD").exists() || head_file.trim().starts_with("ref: ");
        let lost = named.len() != revs.len()
            || named.iter().enumerate().any(|(i, l)| l.ends_with(" missing") && !(i == 0 && unborn));
        if lost {
            found.push("a branch or HEAD naming a commit varde does not have (its work may be lost)".into());
        }
        trees.sort();
        trees.dedup();
        // Past so many, each a listing to read, it is said, not read.
        if trees.len() > 64 {
            found.push(format!("{} trees on its branches, more than varde looks through", trees.len()));
            trees.truncate(64);
        }
        let mut committed: Vec<String> =
            trees.iter().flat_map(|t| out_of(links(&repo, &["ls-tree", "-r", "-z", t], 2))).collect();
        committed.sort();
        committed.dedup();
        if bad || !staged.is_empty() {
            // Made again from what HEAD has, so a commit does not take every
            // file for removed.
            set_aside(&gd.join("index"));
            let _ = std::fs::remove_file(gd.join("index.lock"));
            let made = crate::git::bounded(&mut crate::git::command(dir, &["read-tree", "HEAD"]))
                .is_ok_and(|o| o.status.success());
            found.push(format!(
                "its index, which names paths out of the checkout or into a .git, or a link out ({})",
                if made { "made again from HEAD" } else { "none now: `git reset` makes one" }
            ));
        } else if gd.join("index").exists() {
            // Else made again from its entries alone: what it says beside
            // them (a tree cached for a commit, a file's times) is the
            // session's, and git would trust it over the files.
            let _ = std::fs::remove_file(gd.join("index.lock"));
            let fresh = gd.join("index.varde-new");
            let _ = std::fs::remove_file(&fresh);
            let _ = std::fs::remove_file(gd.join("index.varde-new.lock"));
            let entries = crate::git::bounded(&mut crate::git::command(dir, &["ls-files", "-s", "-z"]))
                .ok()
                .filter(|o| o.status.success())
                .map(|o| o.stdout);
            let made = entries.is_some_and(|e| {
                let mut c = crate::git::command(dir, &["update-index", "-z", "--index-info"]);
                c.env("GIT_INDEX_FILE", &fresh);
                crate::git::bounded_input(&mut c, &e).is_ok_and(|o| o.status.success())
                    && (e.is_empty() || fresh.exists())
            });
            // (No entries, no index: git takes none for empty.)
            let placed = made
                && match fresh.exists() {
                    true => std::fs::rename(&fresh, gd.join("index")).is_ok(),
                    false => std::fs::remove_file(gd.join("index")).is_ok(),
                };
            if made && placed {
                let _ =
                    crate::git::bounded(&mut crate::git::command(dir, &["update-index", "-q", "--refresh"]));
            } else {
                let _ = std::fs::remove_file(&fresh);
                set_aside(&gd.join("index"));
                found.push("its index, which could not be made again from its entries".into());
            }
        }
        if !committed.is_empty() {
            let more = committed.len().saturating_sub(5);
            let mut names = committed.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            if more > 0 {
                names.push_str(&format!(" and {more} more"));
            }
            found.push(format!("a link out it committed, at {names}"));
        }
        let head = crate::git::small_text(&gd.join("HEAD")).unwrap_or_default();
        let head = head.trim();
        // A branch name as git would make one (no way out of refs/ by
        // `..`), and not the one the main checkout has, which your next
        // commit here would move.
        let main = common.as_ref().and_then(|c| crate::git::small_text(&c.join("HEAD"))).unwrap_or_default();
        let main = Some(main.trim()).filter(|_| common.as_ref().is_some_and(|c| c.ends_with(".git")));
        let branch = head.strip_prefix("ref: refs/heads/").is_some_and(|b| {
            b.chars().all(|c| !c.is_control() && !" ~^:?*[\\".contains(c))
                && b.split('/').all(|p| !p.is_empty() && !p.starts_with('.') && !p.ends_with(".lock"))
                && !b.contains("..")
                && !b.contains("@{")
        }) && main.is_none_or(|m| !head.eq_ignore_ascii_case(m));
        let commit = head.len() >= 40 && head.chars().all(|c| c.is_ascii_hexdigit());
        if !branch && !commit {
            found.push(format!("its HEAD, which names {}", plain(head)));
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Whether a link at `at` (its path in the repository) to `target` leads
/// out of the repository, wherever it is checked out: by a `..` above its
/// top. `strict`, for one git will write (staged, committed): also one
/// absolute, into a `.git`, or with a `..` after a name, which a link
/// there could turn elsewhere.
fn leaves(at: &Path, target: &Path, strict: bool) -> bool {
    use std::path::Component;
    let mut depth = at.parent().map_or(0, |p| p.components().count());
    let mut named = false;
    for c in target.components() {
        match c {
            Component::Normal(n) => {
                if strict && n.to_string_lossy().eq_ignore_ascii_case(".git") {
                    return true;
                }
                named = true;
                depth += 1;
            }
            Component::ParentDir if depth == 0 || (strict && named) => return true,
            Component::ParentDir => depth -= 1,
            Component::CurDir => {}
            _ => return strict,
        }
    }
    false
}

/// `p` with `.` and `..` taken as written, no link followed.
pub(crate) fn lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// `text` with every control character, and every one that turns text
/// around, shown as `?`: what a session named, printed to a terminal.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            let turns = matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
            if c.is_control() || turns {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// Whether the `.git` file `git` points into one of `modules`.
fn points_into(git: &Path, modules: &[PathBuf]) -> bool {
    let Some(text) = crate::git::small_text(git) else { return false };
    let Some(target) = text.trim().strip_prefix("gitdir:") else { return false };
    let target = git.parent().unwrap_or(git).join(target.trim());
    target.canonicalize().is_ok_and(|t| modules.iter().any(|m| t.starts_with(m)))
}

/// The git directory of the checkout `dir` and its repository's, read
/// from its `.git`, not from git, which would wait on a pipe left there.
pub(crate) fn git_dirs(dir: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let own = dir.join(".git");
    let gd = if std::fs::symlink_metadata(&own).is_ok_and(|m| m.is_dir()) {
        Some(own)
    } else {
        crate::git::small_text(&own)
            .and_then(|t| t.trim().strip_prefix("gitdir:").map(|g| dir.join(g.trim())))
    };
    let common = gd.as_ref().map(|gd| match crate::git::small_text(&gd.join("commondir")) {
        Some(c) => lexical(&gd.join(c.trim())),
        None => gd.clone(),
    });
    (gd, common)
}

/// `p` as the kernel would take it: as far as it exists, through every
/// link; the rest as written.
fn resolved(p: &Path) -> PathBuf {
    for a in p.ancestors() {
        if let Ok(real) = a.canonicalize() {
            let rest = p.strip_prefix(a).unwrap_or(Path::new(""));
            return lexical(&real.join(rest));
        }
    }
    lexical(p)
}

/// An agent session that verifies the work as the check's `verify`
/// asks, with its tools and skills: on the review profile, in the task's
/// sandbox, changing nothing, its last line PASS or FAIL. Its hooks are
/// varde's own (`VARDE_INTERNAL`): it is no session of the task.
fn verifier(paths: &Paths, store: &Store, task: &str, d: &Def, dir: &Path, out: &mut File) -> Result<Found> {
    let name = d.reviewer.clone().unwrap_or_else(|| config::get().roles.review.clone());
    let Some(profile) = config::get().profile(&name) else {
        return Ok(Found::error(&format!("there is no profile {name} to verify with")));
    };
    let agent_name = profile.agent.clone().unwrap_or_else(start::default_agent);
    let Some(agent) = varde_agent::by_name(&agent_name) else {
        return Ok(Found::error(&format!("varde does not know the agent {agent_name}")));
    };
    let (title, body): (String, Option<String>) =
        store.conn.query_row("SELECT title, body FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    // Kept as text (a file's read when it was set): nothing is read now.
    let ask = d.verify.clone();
    let prompt = format!(
        "Verify this work for the person who will land it. Change no file and commit nothing: find out \
         whether it does what the task asks, with whatever you need (run it, test it, read it, your skills).\n\n\
         # The task\n{title}\n\n{}\n\n# What to verify\n{}\n\n\
         End with at most eight findings, each citing path:line, and then a last line that is exactly PASS or FAIL.",
        body.unwrap_or_default(),
        ask.unwrap_or_default()
    );
    let id = agent.new_native_id();
    let launch = varde_agent::Launch::New {
        id: id.as_deref(),
        name: &format!("verify: {title}"),
        prompt: Some(&prompt),
    };
    let (argv, env) = agent.launch(launch);
    let argv = agent.tuned(argv, profile.model.as_deref(), profile.effort.as_deref());
    let plan = crate::sandbox::plan(store, task)?;
    let grant = varde_agent::Grant {
        fenced: plan.fenced,
        trusted: plan.policy.is_none(),
        commands: vec![],
        deny: plan.policy.as_ref().map(|p| p.deny.clone()).unwrap_or_default(),
    };
    let argv = match agent.unattended(agent.background(argv), &grant) {
        Ok(a) => config::get().launch(&agent_name, a),
        Err(e) => return Ok(Found::error(&format!("the verifier cannot run: {e}"))),
    };
    let fence = match plan.policy.filter(|_| plan.fenced) {
        Some(p) => match crate::sandbox::wrap(&p, dir, Some(agent.as_ref()), &paths.home, None) {
            Ok(w) => Some(w),
            Err(e) => return Ok(Found::error(&format!("no sandbox to verify it in: {e:#}"))),
        },
        None => None,
    };
    writeln!(out, "\n== verified by an agent on {name} ==")?;
    out.flush()?;
    let from = out.metadata()?.len();
    let mut full: Vec<String> = fence.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    full.extend(argv);
    let mut command = Command::new(&full[0]);
    if let Some(f) = &fence {
        command.env_clear().envs(f.1.clone());
    }
    let child = command
        .args(&full[1..])
        .envs(env)
        .env("VARDE_INTERNAL", "1")
        .env_remove("VARDE_RUN")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out.try_clone()?)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Ok(Found::error(&format!("could not start the verifier: {e}"))),
    };
    let limit = Duration::from_secs(config::get().loops.check_minutes.max(1) * 60);
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(f) = &fence {
                crate::sandbox::import(&f.1, dir);
            }
            return Ok(Found::error(&format!("the verifier timed out after {} min", limit.as_secs() / 60)));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some(f) = &fence {
        crate::sandbox::import(&f.1, dir);
    }
    out.flush()?;
    let len = out.metadata()?.len();
    let said = tail_of(out, len.saturating_sub(from).min(16_000));
    let lines: Vec<&str> = said.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let last =
        lines.last().copied().unwrap_or("").trim_matches(|c: char| !c.is_ascii_alphabetic()).to_uppercase();
    let finding = lines.iter().rev().nth(1).map(|l| l.chars().take(60).collect::<String>());
    Ok(match last.as_str() {
        "PASS" => Found {
            verdict: Verdict::Pass,
            exit: Some(0),
            summary: Some(format!("verified by {name}")),
            fingerprint: None,
        },
        "FAIL" => Found {
            verdict: Verdict::Fail,
            exit: None,
            summary: Some(format!("{name}: {}", finding.unwrap_or_else(|| "fail".into()))),
            fingerprint: Some(fingerprint(&said)),
        },
        _ => Found::error("the verifier ended on neither PASS nor FAIL"),
    })
}

/// The end of what `f` holds.
fn tail_of(f: &mut File, n: u64) -> String {
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut f = match f.try_clone() {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(n)));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s
}

/// The end of a file, by path.
pub fn tail(path: &Path, n: usize) -> String {
    match File::open(path) {
        Ok(mut f) => tail_of(&mut f, n as u64),
        Err(_) => String::new(),
    }
}

/// One line that says how a check failed: the last that names failures
/// or errors, at most 60 characters.
fn summarize(text: &str) -> Option<String> {
    // As text: what a check printed is the session's to write.
    let text: String =
        text.chars().map(|c| if c.is_control() && c != '\n' && c != '\t' { '?' } else { c }).collect();
    let line = text.lines().map(str::trim).rev().find(|l| {
        let l = l.to_lowercase();
        (l.contains("fail") || l.contains("error")) && l.len() > 3
    })?;
    let mut s: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        s.push('…');
    }
    Some(s)
}

/// The same failure twice gives the same print: digits, hashes, paths
/// under /tmp and spacing set aside.
fn fingerprint(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut norm = String::with_capacity(text.len());
    let mut hexrun = 0;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_digit() {
            norm.push('#');
        } else if ch.is_whitespace() {
            if !norm.ends_with(' ') {
                norm.push(' ');
            }
        } else {
            norm.push(ch);
        }
        hexrun = if ch.is_ascii_hexdigit() { hexrun + 1 } else { 0 };
    }
    let _ = hexrun;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    norm.trim().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The review: a headless call of the review profile, never a session.
/// It reads the task, your rubric, the diff and the command's output, and
/// answers PASS or FAIL on its first line.
#[allow(clippy::too_many_arguments)]
fn review(
    paths: &Paths,
    store: &Store,
    task: &str,
    d: &Def,
    dir: &Path,
    after: Option<&str>,
    out_path: &Path,
    out: &mut File,
) -> Result<Found> {
    let name = d.reviewer.clone().unwrap_or_else(|| config::get().roles.review.clone());
    let Some(profile) = config::get().profile(&name) else {
        return Ok(Found::error(&format!("there is no profile {name} to review with")));
    };
    let agent_name = profile.agent.clone().unwrap_or_else(start::default_agent);
    let Some(agent) = varde_agent::by_name(&agent_name) else {
        return Ok(Found::error(&format!("varde does not know the agent {agent_name}")));
    };
    let (title, body): (String, Option<String>) =
        store.conn.query_row("SELECT title, body FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let rubric = d.rubric.clone();
    let diff = match (&d.base_tree, after) {
        (Some(a), Some(b)) => crate::git::bounded(&mut crate::git::command(dir, &["diff", a, b]))
            .map(|o| o.stdout)
            .unwrap_or_default(),
        _ => crate::git::bounded(&mut crate::git::command(dir, &["diff", "HEAD"]))
            .map(|o| o.stdout)
            .unwrap_or_default(),
    };
    let mut diff = String::from_utf8_lossy(&diff).into_owned();
    if diff.len() > DIFF {
        let mut cut = DIFF;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n[diff cut here]");
    }
    let cmd_out = tail(out_path, TAIL);
    let mut material = format!("# The task\n{title}\n\n{}\n\n", body.unwrap_or_default());
    if let Some(r) = rubric {
        material.push_str(&format!("# The rubric\n{r}\n\n"));
    }
    if let Some(c) = &d.cmd {
        material.push_str(&format!("# The check `{c}` passed; the end of its output\n{cmd_out}\n\n"));
    }
    material.push_str(&format!(
        "# The change\n{}\n",
        if diff.trim().is_empty() { "(no diff: the work may be outside git)" } else { &diff }
    ));
    let system = "You review a change for the person who will land it. You read the task, their rubric, the \
                  change and what the checks printed. Your first line is exactly PASS or FAIL: FAIL when the \
                  change does not do what the task asks, breaks the rubric, or weakens or removes a test. Then \
                  at most eight findings, one per line, each citing a file and line (path:line). Nothing else.";
    let model = profile.model.clone();
    let answer = match crate::job::ask_model(&paths.home, agent.as_ref(), model.as_deref(), system, &material)
    {
        Ok(a) => a.text,
        Err(e) => return Ok(Found::error(&format!("the review failed: {e:#}"))),
    };
    writeln!(out, "\n== review by {name} ==\n{answer}")?;
    let first = answer.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_uppercase();
    let finding = answer
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .nth(1)
        .map(|l| l.chars().take(60).collect::<String>());
    Ok(if first.starts_with("PASS") {
        Found {
            verdict: Verdict::Pass,
            exit: Some(0),
            summary: Some(format!("{name}: pass")),
            fingerprint: None,
        }
    } else if first.starts_with("FAIL") {
        Found {
            verdict: Verdict::Fail,
            exit: None,
            summary: Some(format!("{name}: {}", finding.unwrap_or_else(|| "fail".into()))),
            fingerprint: Some(fingerprint(&answer)),
        }
    } else {
        Found::error("the review said neither PASS nor FAIL")
    })
}

// ---------------------------------------------------------------- approve

/// `varde approve <task>`: you looked at its work and it is good, for the
/// files there now.
pub fn approve(paths: &Paths, reference: &str) -> Result<()> {
    crate::sandbox::not_inside("Approving a task's work")?;
    let store = paths.open(BUSY)?;
    // A session must not approve its own gate, or the one it waits on.
    crate::steward::yours(&store, "Approving a task's work")?;
    let id = task::resolve(&store, reference)?;
    let t = checkout(&store, &id)?.and_then(|c| tree(&c));
    let now = now_ms();
    store.conn.execute("INSERT INTO approval (task, tree, ts) VALUES (?1, ?2, ?3)", params![id, t, now])?;
    if let Some(t) = &t {
        store.conn.execute(
            "INSERT INTO git_state (task, tree, checked) VALUES (?1, ?2, 0) ON CONFLICT (task) DO UPDATE SET tree = ?2",
            params![id, t],
        )?;
    }
    varde_core::store::mark_seen(&store.conn, &id, now)?;
    let short = task::short(&id);
    let next = |store: &Store| -> Result<Vec<String>> {
        Ok(store
            .conn
            .prepare("SELECT task FROM edge WHERE after = ?1")?
            .query_map([&id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    // A plan whose output is a trail: approved, its tasks are made and
    // set going, after it. Once.
    let (output, title, project, sandbox): (Option<String>, String, Option<String>, Option<String>) = store
        .conn
        .query_row("SELECT output, title, project, sandbox FROM task WHERE id = ?1", [&id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    if output.as_deref() == Some("trail") {
        let file = checkout(&store, &id)?.map(|c| c.join(crate::recipe::TRAIL)).filter(|f| f.is_file());
        let Some(file) = file else {
            println!(
                "approved {short}, but it wrote no {}: varde start {short} asks it to",
                crate::recipe::TRAIL
            );
            return Ok(());
        };
        drop(store);
        // Its tasks in the plan's sandbox: the plan wrote them from inside it.
        let root = checkout(&paths.open(BUSY)?, &id)?.context("its checkout")?;
        crate::sandbox::holding(sandbox, root, || {
            crate::recipe::new(
                paths,
                &file.display().to_string(),
                &[title],
                project.map(PathBuf::from),
                None,
                &[],
                &[],
                None,
                (std::slice::from_ref(&id), true),
                true,
                false,
            )
        })
        .with_context(|| format!("approved {short}, but its {} makes no trail", crate::recipe::TRAIL))?;
        // Made once: approving it again approves it, and makes nothing.
        paths.open(BUSY)?.conn.execute("UPDATE task SET output = 'trail made' WHERE id = ?1", [&id])?;
        println!("approved {short}: its trail is going; varde trail {short} shows it");
        return Ok(());
    }
    let next = next(&store)?;
    if next.is_empty() {
        println!("approved {short}");
    } else {
        println!(
            "approved {short}; what waits on it can go: {}",
            next.iter().map(|t| task::short(t)).collect::<Vec<_>>().join(" ")
        );
    }
    let _ = crate::detach(&["tick"], "the tick");
    Ok(())
}

// ---------------------------------------------------------------- loop

pub struct LoopArgs {
    pub check: Option<String>,
    pub max: Option<i64>,
    pub minutes: Option<i64>,
    pub escalate: Option<String>,
    pub off: bool,
    pub pause: bool,
}

/// `varde loop <task> …`.
pub fn set_loop(paths: &Paths, reference: &str, a: LoopArgs) -> Result<()> {
    crate::sandbox::not_inside("Changing a loop")?;
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    if a.off {
        let gone = store.conn.execute("DELETE FROM loop WHERE task = ?1", [&id])?;
        println!(
            "{}",
            if gone > 0 { format!("{short} no longer loops") } else { format!("{short} had no loop") }
        );
        return Ok(());
    }
    let existing = core::loop_of(&store.conn, &id)?;
    if a.pause {
        store.conn.execute("UPDATE loop SET paused = 1 WHERE task = ?1", [&id])?;
        println!("{short}'s loop is paused; varde loop {short} resumes it");
        return Ok(());
    }
    // A check is what a loop runs to.
    if let Some(cmd) = &a.check {
        let mut d = def(&store, &id)?.unwrap_or(Def {
            cmd: None,
            review: false,
            rubric: None,
            reviewer: None,
            verify: None,
            guard: vec![],
            again: 1,
            base_tree: None,
        });
        d.cmd = Some(cmd.clone());
        set_def(&store, &id, &d)?;
    }
    if def(&store, &id)?.is_none() {
        let project: Option<String> =
            store.conn.query_row("SELECT project FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
        let repo =
            project.as_deref().map(|p| config::Repo::read(Path::new(p))).transpose()?.unwrap_or_default();
        match repo.check {
            Some(cmd) => set_def(&store, &id, &Def { cmd: Some(cmd), review: false, rubric: None, reviewer: None, verify: None, guard: vec![], again: 1, base_tree: None })?,
            None => bail!("a loop runs until a check passes: varde loop {short} --check CMD, or varde check {short} first"),
        }
    }
    let escalate = a
        .escalate
        .map(|e| e.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>());
    if let Some(list) = &escalate {
        for p in list {
            if config::get().profile(p).is_none() {
                bail!("there is no profile {p} to escalate to");
            }
        }
    }
    let now = now_ms();
    match existing {
        // Resumed as it was, unless told otherwise.
        Some(l) if l.paused && a.max.is_none() && a.minutes.is_none() && escalate.is_none() => {
            store.conn.execute("UPDATE loop SET paused = 0 WHERE task = ?1", [&id])?;
        }
        _ => {
            store.conn.execute(
                "INSERT INTO loop (task, max, minutes, escalate, started, paused) VALUES (?1, ?2, ?3, ?4, ?5, 0)
                 ON CONFLICT (task) DO UPDATE SET max = ?2, minutes = ?3, escalate = ?4, started = ?5, paused = 0",
                params![
                    id,
                    a.max.or(existing.as_ref().map(|l| l.max)).unwrap_or(3).max(1),
                    a.minutes.or(existing.as_ref().and_then(|l| l.minutes)),
                    escalate
                        .or_else(|| existing.as_ref().map(|l| l.escalate.clone()).filter(|e| !e.is_empty()))
                        .map(|e| serde_json::to_string(&e).unwrap_or_default()),
                    now
                ],
            )?;
            // The guard compares with the files as they are when it starts.
            if let Some(t) = checkout(&store, &id)?.and_then(|c| tree(&c)) {
                store.conn.execute("UPDATE check_def SET base_tree = ?2 WHERE task = ?1", params![id, t])?;
            }
        }
    }
    store.conn.execute("UPDATE task SET auto = 1, inbox = 0 WHERE id = ?1", [&id])?;
    let d = def(&store, &id)?.context("a check")?;
    println!(
        "{short} loops in the background until its check passes ({}), at most {} rounds; varde follow {short} watches it",
        describe(&d),
        core::loop_of(&store.conn, &id)?.map_or(3, |l| l.max)
    );
    if crate::schedule::heartbeat().is_none() {
        println!("nothing wakes varde between rounds yet: varde schedule --install");
    }
    let profile: Option<String> =
        store.conn.query_row("SELECT profile FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
    let chain =
        profile.into_iter().chain(core::loop_of(&store.conn, &id)?.map(|l| l.escalate).unwrap_or_default());
    for p in chain {
        if let Some(w) = config::get().profile_warning(p.split(':').next().unwrap_or(&p)) {
            eprintln!("varde: {w}");
        }
    }
    drop(store);
    crate::schedule::tick(paths)
}

/// The task of `run`, when it is a session of yours on a task whose loop
/// goes on: its turn's end is a round's. One indexed read, for the hook.
pub fn attended_turn(conn: &rusqlite::Connection, run: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT r.task FROM run r JOIN loop l ON l.task = r.task JOIN check_def c ON c.task = r.task
             WHERE r.id = ?1 AND l.paused = 0 AND NOT coalesce(r.background, 0) AND r.round IS NULL",
            [run],
            |r| r.get(0),
        )
        .optional()?)
}

/// After a check of a turn of yours, on a loop that goes on: the failure
/// goes into your session as its next prompt, through its own inbox as
/// `varde say` sends, unless its last words asked you something.
fn attend(paths: &Paths, task: &str, run: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    if attended_turn(&store.conn, run)?.is_none() {
        return Ok(());
    }
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let now = now_ms();
    let Some(State::Attended { round }) = core::state(&store.conn, task, now, &alive)? else { return Ok(()) };
    let checks = core::checks(&store.conn, task, 0)?;
    let mine: Vec<&core::Check> = checks.iter().filter(|c| c.run.as_deref() == Some(run)).collect();
    let Some(last) = mine.last().filter(|c| c.verdict == Verdict::Fail) else { return Ok(()) };
    if asked_last(&store, run)? {
        return Ok(());
    }
    let d = def(&store, task)?.context("a check")?;
    let max = core::loop_of(&store.conn, task)?.map_or(3, |l| l.max);
    let mut text = format!("Round {round} of {max}. The check ({}) failed", describe(&d));
    match last.output.as_deref() {
        Some(o) => text.push_str(&format!(":\n\n```\n{}\n```", tail(Path::new(o), TAIL).trim())),
        None => text.push('.'),
    }
    let before = mine.len().checked_sub(2).map(|i| mine[i]);
    if before.is_some_and(|b| b.fingerprint == last.fingerprint && b.tree == last.tree) {
        text.push_str(
            "\n\nThe same failure as the turn before, with nothing changed: step back and try another way.",
        );
    }
    text.push_str("\n\nFix it; varde checks again when your turn ends.");
    crate::steward::deliver(&store, task, &text, "varde's loop", None, false, now)?;
    Ok(())
}

/// Whether `run`'s last words were a question: then the session waits
/// for its user, and the loop with it.
fn asked_last(store: &Store, run: &str) -> Result<bool> {
    let (agent, transcript): (String, Option<String>) =
        store.conn.query_row("SELECT agent, transcript FROM run WHERE id = ?1", [run], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let (Some(adapter), Some(t)) = (varde_agent::by_name(&agent), transcript) else { return Ok(false) };
    let path = Path::new(&t);
    let from = std::fs::metadata(path).map(|m| m.len().saturating_sub(200_000)).unwrap_or(0);
    let (digest, _) = adapter.digest(path, from, 4000).unwrap_or_default();
    let last = digest.rsplit("Reported back:").next().unwrap_or("").trim();
    Ok(last.lines().map(str::trim).rfind(|l| !l.is_empty()).is_some_and(|l| l.ends_with('?')))
}

// ---------------------------------------------------------------- edges

/// `varde after <task> <dep>…`: the task waits until each is verified
/// (`approved`: until you approve each).
pub fn after(paths: &Paths, reference: &str, deps: &[String], approved: bool, off: bool) -> Result<()> {
    crate::sandbox::not_inside("Changing what a task waits on")?;
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    if deps.is_empty() {
        let waits = core::waits(&store.conn, &id)?;
        if waits.is_empty() {
            println!("{short} waits on nothing");
        }
        for w in waits {
            println!(
                "{} {} ({}){}",
                if w.met { "✓" } else { "⧗" },
                task::short(&w.after),
                w.kind,
                if w.met { "" } else { ", waiting" }
            );
        }
        return Ok(());
    }
    for d in deps {
        let dep = task::resolve(&store, d)?;
        if off {
            store.conn.execute("DELETE FROM edge WHERE task = ?1 AND after = ?2", params![id, dep])?;
            continue;
        }
        if !add_edge(&store, &id, &dep, approved)? && !approved {
            println!(
                "{} has no check to be verified by: {short} waits on your approval of it",
                task::short(&dep)
            );
        }
    }
    println!(
        "{}",
        if off {
            format!("{short} no longer waits on those")
        } else {
            format!("{short} waits on {}", deps.join(" "))
        }
    );
    Ok(())
}

/// `task` waits on `after`: verified, unless `approved` or `after` has no
/// check, which could never verify it. Whether it waits on a check.
pub fn add_edge(store: &Store, task: &str, after: &str, approved: bool) -> Result<bool> {
    let checked: bool =
        store
            .conn
            .query_row("SELECT EXISTS (SELECT 1 FROM check_def WHERE task = ?1)", [after], |r| r.get(0))?;
    let approved = approved || !checked;
    if core::would_cycle(&store.conn, task, after)? {
        bail!(
            "{} already comes before {}: that would be a loop of waiting",
            task::short(task),
            task::short(after)
        );
    }
    store.conn.execute(
        "INSERT INTO edge (task, after, kind) VALUES (?1, ?2, ?3) ON CONFLICT (task, after) DO UPDATE SET kind = ?3",
        params![task, after, if approved { "approved" } else { "verified" }],
    )?;
    Ok(!approved)
}

// ---------------------------------------------------------------- trail

/// `varde trail <task> [--start] [--format text|mermaid] [--json]`.
pub fn trail(paths: &Paths, reference: &str, go: bool, format: &str, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    crate::sandbox::own_project(&store, &id)?;
    let tasks = core::trail(&store.conn, &id)?;
    if go {
        crate::sandbox::not_inside("Setting a trail going")?;
        let mut n = 0;
        for t in &tasks {
            n += store
                .conn
                .execute("UPDATE task SET auto = 1, inbox = 0 WHERE id = ?1 AND state = 'open'", [t])?;
        }
        if n == 1 {
            println!("set 1 task going: it starts in the background once what it waits on is met");
        } else {
            println!("set {n} tasks going: each starts in the background once what it waits on is met");
        }
        if crate::schedule::heartbeat().is_none() {
            println!("nothing wakes varde between steps yet: varde schedule --install");
        }
        drop(store);
        crate::schedule::tick(paths)?;
        let store = paths.open(BUSY)?;
        return print_trail(&store, &tasks, format, as_json);
    }
    print_trail(&store, &tasks, format, as_json)
}

/// One line per node of a trail: the facts `trail` and `recipe show` print.
pub struct Node {
    pub id: String,
    pub title: String,
    pub profile: Option<String>,
    pub label: String,
    pub after: Vec<(String, String)>,
}

fn nodes(store: &Store, tasks: &[String]) -> Result<Vec<Node>> {
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let now = now_ms();
    tasks
        .iter()
        .map(|t| {
            let (title, profile, state): (String, Option<String>, String) =
                store.conn.query_row("SELECT title, profile, state FROM task WHERE id = ?1", [t], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            let label = match core::row(&store.conn, t, &state, now, &alive)? {
                Some(f) => f.label,
                None if state != "open" => state.clone(),
                None => {
                    let started: bool = store.conn.query_row(
                        "SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)",
                        [t],
                        |r| r.get(0),
                    )?;
                    if started {
                        "started".into()
                    } else {
                        "not started".into()
                    }
                }
            };
            let after = core::waits(&store.conn, t)?.into_iter().map(|w| (w.after, w.kind)).collect();
            Ok(Node { id: t.clone(), title, profile, label, after })
        })
        .collect()
}

fn print_trail(store: &Store, tasks: &[String], format: &str, as_json: bool) -> Result<()> {
    let nodes = nodes(store, tasks)?;
    if as_json {
        let v: Vec<_> = nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "task": n.id, "short": task::short(&n.id), "title": n.title, "profile": n.profile,
                    "label": n.label,
                    "after": n.after.iter().map(|(a, k)| serde_json::json!({"task": a, "kind": k})).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::json!({"v": 1, "tasks": v}));
        return Ok(());
    }
    let lines: Vec<(String, String)> = nodes
        .iter()
        .map(|n| {
            let mut text = n.title.clone();
            if let Some(p) = &n.profile {
                text.push_str(&format!(" · {p}"));
            }
            text.push_str(&format!(" · {}", n.label));
            (n.id.clone(), text)
        })
        .collect();
    let edges: Vec<(String, String, String)> = nodes
        .iter()
        .flat_map(|n| n.after.iter().map(|(a, k)| (n.id.clone(), a.clone(), k.clone())))
        .collect();
    if format == "mermaid" {
        println!("{}", mermaid(&lines, &edges, &|id| task::short(id)));
    } else {
        for l in draw(&lines, &edges, &|id| task::short(id)) {
            println!("{l}");
        }
    }
    Ok(())
}

/// A graph as a tree, as `git log --graph` draws history: each node
/// under the first it waits on, and a note where it waits on more.
/// `lines` are (id, text) in order; `edges` (node, after, kind).
pub fn draw(
    lines: &[(String, String)],
    edges: &[(String, String, String)],
    name: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let ups = |id: &str| -> Vec<&(String, String, String)> { edges.iter().filter(|e| e.0 == id).collect() };
    let mut out = vec![];
    let mut placed: Vec<String> = vec![];
    #[allow(clippy::too_many_arguments)]
    fn walk(
        id: &str,
        prefix: &str,
        last: bool,
        root: bool,
        lines: &[(String, String)],
        edges: &[(String, String, String)],
        placed: &mut Vec<String>,
        out: &mut Vec<String>,
        name: &dyn Fn(&str) -> String,
    ) {
        let text = lines.iter().find(|l| l.0 == id).map(|l| l.1.clone()).unwrap_or_default();
        let others: Vec<String> = edges
            .iter()
            .filter(|e| e.0 == id)
            .skip(1)
            .map(|e| format!("{}{}", name(&e.1), if e.2 == "approved" { " (approved)" } else { "" }))
            .collect();
        let also =
            if others.is_empty() { String::new() } else { format!("  (also after {})", others.join(", ")) };
        let gate = edges.iter().find(|e| e.0 == id).filter(|e| e.2 == "approved").map_or("", |_| "T ");
        let branch = if root {
            ""
        } else if last {
            "└─ "
        } else {
            "├─ "
        };
        out.push(format!("{prefix}{branch}{gate}{text}  {}{also}", name(id)));
        placed.push(id.to_string());
        let kids: Vec<&String> = lines
            .iter()
            .map(|l| &l.0)
            .filter(|k| edges.iter().find(|e| &e.0 == *k).is_some_and(|e| e.1 == id) && !placed.contains(k))
            .collect();
        let next =
            if root { String::new() } else { format!("{prefix}{}", if last { "   " } else { "│  " }) };
        for (i, k) in kids.iter().enumerate() {
            walk(k, &next, i + 1 == kids.len(), false, lines, edges, placed, out, name);
        }
    }
    for (id, _) in lines {
        if ups(id).is_empty() && !placed.contains(id) {
            walk(id, "", true, true, lines, edges, &mut placed, &mut out, name);
        }
    }
    // Whatever only a cycle could hide.
    for (id, text) in lines {
        if !placed.contains(id) {
            out.push(format!("{text}  {}", name(id)));
        }
    }
    out
}

/// The same graph for mermaid.
pub fn mermaid(
    lines: &[(String, String)],
    edges: &[(String, String, String)],
    name: &dyn Fn(&str) -> String,
) -> String {
    let mut out = String::from("graph TD\n");
    let quote = |s: &str| s.replace('"', "'");
    for (id, text) in lines {
        out.push_str(&format!("  n{}[\"{}\"]\n", name(id), quote(text)));
    }
    for (task, after, kind) in edges {
        let arrow = if kind == "approved" { "-- you approve -->" } else { "-->" };
        out.push_str(&format!("  n{} {arrow} n{}\n", name(after), name(task)));
    }
    out
}

// ---------------------------------------------------------------- show

/// The lines `varde show` prints for a task's check, loop and trail.
pub fn show(paths: &Paths, store: &Store, task: &str) -> Result<Vec<String>> {
    let now = now_ms();
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let mut out = vec![];
    let (profile, recipe): (Option<String>, Option<String>) =
        store.conn.query_row("SELECT profile, recipe FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    match (profile, recipe) {
        (Some(p), Some(r)) => out.push(format!("profile {p} · made from recipe {r}")),
        (Some(p), None) => out.push(format!("profile {p}")),
        (None, Some(r)) => out.push(format!("made from recipe {r}")),
        (None, None) => {}
    }
    if let Some(d) = def(store, task)? {
        out.push(format!("Check   {}", describe(&d)));
        if checking(paths, task) {
            out.push("        … checking now".into());
        }
        if let Some(c) = core::last_check(&store.conn, task)? {
            out.push(format!("        last {}", result_line(&c, now)));
        }
    }
    if let Some(l) = core::loop_of(&store.conn, task)? {
        let st = core::state(&store.conn, task, now, &alive)?.context("a loop")?;
        let esc = if l.escalate.is_empty() {
            String::new()
        } else {
            format!(", escalating to {}", l.escalate.join(", "))
        };
        let budget = l.minutes.map(|m| format!(", {m} min")).unwrap_or_default();
        out.push(format!("Loop    {} · at most {} rounds{budget}{esc}", state_words(&st), l.max));
        let (rounds, checks) = core::with_turns(&store.conn, task, &l, now, &alive)?;
        for r in &rounds {
            let c = checks.iter().rev().find(|c| c.run.as_deref() == Some(&r.run));
            let mins = r
                .ended
                .map(|e| format!("{}m", (e - r.started).max(0) / 60_000))
                .unwrap_or_else(|| "now".into());
            let verdict = match c {
                Some(c) => format!(
                    "{} {}",
                    match c.verdict {
                        Verdict::Pass => "✓",
                        Verdict::Fail | Verdict::Error => "✗",
                        Verdict::Flagged => "⚑",
                        Verdict::Unchecked => "…",
                    },
                    c.summary.clone().unwrap_or_else(|| c.verdict.name().into())
                ),
                None if r.live => "at work".into(),
                None => "not checked yet".into(),
            };
            out.push(format!(
                "        #{} {:<7} {:>4}  {verdict}{}  run {}",
                r.n,
                r.profile.clone().unwrap_or_else(|| "-".into()),
                mins,
                if r.asked { "  (asked you)" } else { "" },
                match r.run.split_once('#') {
                    // A turn of a session of yours.
                    Some((run, _)) => format!("{} (yours)", task::short(run)),
                    None => task::short(&r.run),
                }
            ));
        }
    }
    let waits = core::waits(&store.conn, task)?;
    let dependents: Vec<String> = store
        .conn
        .prepare("SELECT task FROM edge WHERE after = ?1")?
        .query_map([task], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (named, auto): (Option<String>, bool) =
        store.conn.query_row("SELECT sandbox, coalesce(auto, 0) FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if named.is_some() || auto || def(store, task)?.is_some() || !waits.is_empty() {
        out.push(format!("Sandbox {}", sandbox_words(named.as_deref())));
    }
    // A plan whose trail is yours to accept: where to read it first.
    let output: Option<String> =
        store.conn.query_row("SELECT output FROM task WHERE id = ?1", [task], |r| r.get(0))?;
    if output.as_deref() == Some("trail") {
        let short = task::short(task);
        match checkout(store, task)?.map(|c| c.join(crate::recipe::TRAIL)).filter(|f| f.is_file()) {
            Some(f) => out.push(format!(
                "Plan    varde recipe show {} draws its trail; varde approve {short} makes it and sets it going",
                f.display()
            )),
            None => out.push(format!("Plan    {short} has written no {} yet", crate::recipe::TRAIL)),
        }
    }
    if !waits.is_empty() || !dependents.is_empty() {
        let w: Vec<String> = waits
            .iter()
            .map(|w| {
                format!(
                    "{} {}{}",
                    if w.met { "✓" } else { "⧗" },
                    task::short(&w.after),
                    if w.kind == "approved" { " (your approval)" } else { "" }
                )
            })
            .collect();
        let mut line = String::from("Trail  ");
        if !w.is_empty() {
            line.push_str(&format!(" after {}", w.join(", ")));
        }
        if !dependents.is_empty() {
            line.push_str(&format!(
                "{}before {}",
                if w.is_empty() { " " } else { "; " },
                dependents.iter().map(|d| task::short(d)).collect::<Vec<_>>().join(", ")
            ));
        }
        line.push_str(&format!("  (varde trail {})", task::short(task)));
        out.push(line);
    }
    Ok(out)
}

/// `default, in bwrap`: the sandbox a task's background sessions and
/// checks run in, as things are here.
fn sandbox_words(named: Option<&str>) -> String {
    let name = named.unwrap_or("default");
    let p = match crate::sandbox::policy(Some(name)) {
        Ok(Some(p)) => p,
        Ok(None) => return "off: its background sessions run on what their agent is granted".into(),
        Err(e) => return format!("{name}: {e:#}"),
    };
    let mut out = match (crate::sandbox::enabled(), crate::sandbox::backend()) {
        (Ok(true), Some(b)) => format!("{name}, in {}", b.name()),
        (Ok(_), _) => {
            format!("{name}, but none runs here: its background sessions run on what their agent is granted")
        }
        (Err(e), _) => format!("{name}: {e:#}"),
    };
    if !p.deny.is_empty() {
        out.push_str(&format!(" · denies {}", p.deny.join(", ")));
    }
    out
}

/// `1 round`, `3 rounds`.
fn n_rounds(n: i64) -> String {
    format!("{n} round{}", if n == 1 { "" } else { "s" })
}

/// A loop's state, in words.
pub fn state_words(st: &State) -> String {
    match st {
        State::Paused { rounds } => format!("paused after {}", n_rounds(*rounds)),
        State::Working { round } => format!("round {round} at work"),
        State::Checking { round, .. } => format!("round {round} ended; checking"),
        State::Due { round, profile } => {
            format!("round {round} next{}", profile.as_ref().map(|p| format!(", on {p}")).unwrap_or_default())
        }
        State::Verified { rounds } => format!("verified after {}", n_rounds(*rounds)),
        State::Flagged { why, .. } => format!("flagged: {why}"),
        State::Error { why, .. } => format!("stopped: {why}"),
        State::Spent { why, .. } => format!("spent: {why}"),
        State::Stuck { rounds } => {
            format!("stuck: the same failure twice with no change, after {}", n_rounds(*rounds))
        }
        State::Asked { .. } => "a round asked you something: open the task to answer".into(),
        State::Attended { round } => {
            format!("round {round}, in your session: each turn is checked as it ends")
        }
    }
}

/// The same, as `show --json` has it.
pub fn json(paths: &Paths, store: &Store, task: &str) -> Result<serde_json::Value> {
    let now = now_ms();
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let check = def(store, task)?.map(|d| {
        serde_json::json!({"cmd": d.cmd, "review": d.review, "reviewer": d.reviewer, "rubric": d.rubric,
                           "guard": d.guard, "again": d.again, "running": checking(paths, task), "verify": d.verify,
                           "words": describe(&d)})
    });
    let last = core::last_check(&store.conn, task)?;
    let lp = match core::loop_of(&store.conn, task)? {
        Some(l) => {
            let st = core::state(&store.conn, task, now, &alive)?;
            let (rounds, checks) = core::with_turns(&store.conn, task, &l, now, &alive)?;
            let words = st.as_ref().map(state_words);
            // A turn of a session of yours: its run, and the check that made it a round.
            let rounds: Vec<serde_json::Value> = rounds
                .iter()
                .map(|r| {
                    let mut v = serde_json::to_value(r).unwrap_or_default();
                    if let Some((run, turn)) = r.run.split_once('#') {
                        v["run"] = run.into();
                        v["turn"] = turn.parse::<i64>().ok().into();
                    }
                    v
                })
                .collect();
            Some(
                serde_json::json!({"def": l, "state": st, "words": words, "rounds": rounds, "checks": checks}),
            )
        }
        None => None,
    };
    let before: Vec<String> = store
        .conn
        .prepare("SELECT task FROM edge WHERE after = ?1 ORDER BY rowid")?
        .query_map([task], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (profile, recipe, sandbox): (Option<String>, Option<String>, Option<String>) =
        store.conn.query_row("SELECT profile, recipe, sandbox FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    Ok(serde_json::json!({
        "profile": profile, "recipe": recipe, "before": before,
        "sandbox": {"name": sandbox.as_deref().unwrap_or("default"), "words": sandbox_words(sandbox.as_deref())},
        "check": check, "last_check": last, "loop": lp,
        "after": core::waits(&store.conn, task)?,
        "verified": core::verified(&store.conn, task)?,
        "approved": core::approved(&store.conn, task)?,
    }))
}

// ---------------------------------------------------------------- profiles

/// `varde set <task> --profile P`: what its new sessions run on.
pub fn set_profile(paths: &Paths, reference: &str, profile: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let profile = profile.trim();
    if !profile.is_empty() && config::get().profile(profile).is_none() {
        bail!("there is no profile {profile}: varde profiles lists them");
    }
    store.conn.execute(
        "UPDATE task SET profile = ?2 WHERE id = ?1",
        params![id, (!profile.is_empty()).then_some(profile)],
    )?;
    let short = task::short(&id);
    if profile.is_empty() {
        println!("{short} runs on the work role's profile");
    } else {
        println!("{short} runs on {profile} from its next session");
    }
    Ok(())
}

/// `varde profiles`.
pub fn profiles(as_json: bool) -> Result<()> {
    let c = config::get();
    if as_json {
        let all: Vec<_> = c
            .profiles()
            .into_iter()
            .map(|(name, p)| {
                serde_json::json!({"name": name, "agent": p.agent, "model": p.model, "effort": p.effort,
                                   "yours": c.profiles.contains_key(&name)})
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"v": 1, "profiles": all, "roles": {"work": c.roles.work, "review": c.roles.review}})
        );
        return Ok(());
    }
    for (name, p) in c.profiles() {
        let mine = if c.profiles.contains_key(&name) { "yours" } else { "ships with varde" };
        println!(
            "{name:<8} {:<7} {:<10} {:<7} {mine}",
            p.agent.clone().unwrap_or_else(start::default_agent),
            p.model.clone().unwrap_or_else(|| "default".into()),
            p.effort.clone().unwrap_or_else(|| "default".into()),
        );
    }
    println!(
        "
roles: work = {}, review = {}",
        c.roles.work.clone().unwrap_or_else(|| "the agent's own default".into()),
        c.roles.review
    );
    Ok(())
}

// ---------------------------------------------------------------- the tick

/// What the tick does next.
#[derive(Debug)]
enum Action {
    /// Check a loop's round that ended.
    Verify { task: String, run: String },
    /// Start a task in the background: a loop's round (`round` from 1),
    /// or a trail's task with no loop (`round` 0).
    Start { task: String, round: i64, profile: Option<String> },
}

/// The tick's part: start what loops and trails have ready, no more than
/// `loops.parallel` at once. Called after the schedules have fired.
pub fn tick(paths: &Paths, store: &mut Store) -> Result<()> {
    let now = now_ms();
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let mut busy = at_work(store, &alive)?;
    let cap = config::get().loops.parallel.max(1);
    for a in next(paths, store, now, &alive)? {
        if busy >= cap {
            break;
        }
        match a {
            Action::Verify { task, run } => {
                crate::detach(&["verify", &task, "--run", &run], "a check")?;
            }
            Action::Start { task, round, profile } => {
                if let Err(e) = fire(store, &task, round, profile, now) {
                    crate::hook::log(paths, &format!("could not start {}: {e:#}", task::short(&task)));
                    continue;
                }
            }
        }
        busy += 1;
    }
    Ok(())
}

/// Background sessions at work now.
fn at_work(store: &Store, alive: &dyn Fn(i64, i64) -> bool) -> Result<usize> {
    let live: Vec<(i64, i64)> = store
        .conn
        .prepare(
            "SELECT pid, coalesce(pid_start, 0) FROM run WHERE background AND exit_code IS NULL AND pid > 0",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(live.into_iter().filter(|(p, s)| alive(*p, *s)).count())
}

fn next(paths: &Paths, store: &Store, now: i64, alive: &dyn Fn(i64, i64) -> bool) -> Result<Vec<Action>> {
    let mut out = vec![];
    // Tasks set going (a loop, or a trail started) that are open.
    let going: Vec<String> = store
        .conn
        .prepare("SELECT id FROM task WHERE coalesce(auto, 0) = 1 AND state = 'open' ORDER BY created")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for t in going {
        if core::held(&store.conn, &t)?.is_some() {
            continue;
        }
        // A session of yours, or any still at work, holds it.
        let live = start::run_facts(store, &t)?.into_iter().any(|r| r.live());
        if live || updating(store, &t)? {
            continue;
        }
        match core::state(&store.conn, &t, now, alive)? {
            Some(State::Due { round, profile }) => out.push(Action::Start { task: t, round, profile }),
            Some(State::Checking { run, .. }) if !checking(paths, &t) => {
                // A turn of a session of yours is its run's.
                let run = run.split('#').next().unwrap_or(&run).to_string();
                out.push(Action::Verify { task: t, run })
            }
            Some(_) => {}
            // A trail's task with no loop: once, when it has never run.
            None => {
                let started: bool =
                    store
                        .conn
                        .query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [&t], |r| r.get(0))?;
                if !started {
                    out.push(Action::Start { task: t, round: 0, profile: None });
                }
            }
        }
    }
    Ok(out)
}

/// Whether a cairn update of `task` is queued or running: the next round
/// waits for it, to start from what the last one learned.
fn updating(store: &Store, task: &str) -> Result<bool> {
    Ok(store.conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM job j JOIN run r ON r.id = j.run
                        WHERE r.task = ?1 AND j.state IN ('queued', 'running'))",
        [task],
        |r| r.get(0),
    )?)
}

/// Start `task` in the background: a round of its loop, or its one run in
/// a trail.
fn fire(store: &mut Store, task: &str, round: i64, profile: Option<String>, now: i64) -> Result<()> {
    let (project, share): (Option<String>, Option<String>) =
        store.conn.query_row("SELECT project, share FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let mut opts = start::Options::fresh(None);
    opts.want = Want::Fresh;
    opts.round = Some(round);
    opts.profile = profile;
    // In its upstream's worktree, or a worktree cut from its upstream's
    // branch, or as it ran before.
    let ups: Vec<String> = core::waits(&store.conn, task)?.into_iter().map(|w| w.after).collect();
    if let Some(up) = share {
        opts.share = checkout(store, &up)?;
        if opts.share.is_none() {
            bail!("its upstream {} has no checkout to share", task::short(&up));
        }
    } else {
        let has_tree = crate::sweep::active_worktree(store, task)?.is_some();
        let ran: bool =
            store
                .conn
                .query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [task], |r| r.get(0))?;
        let repo =
            project.as_deref().map(Path::new).filter(|p| varde_core::project::root(p).join(".git").exists());
        opts.worktree = if has_tree {
            None
        } else if ran {
            Some(false)
        } else {
            Some(repo.is_some_and(crate::git::has_commits))
        };
        if let [up] = ups.as_slice() {
            if let Some((_, _, branch, _)) = crate::sweep::active_worktree(store, up)? {
                opts.base = Some(branch);
            }
        }
    }
    let mut context = prompt(store, task, round, &ups)?;
    // Told of its sandbox if it will be in one; one that cannot be made
    // is for its run to say, as the error its loop stops on.
    if let Ok(crate::sandbox::Plan { policy: Some(p), fenced }) = crate::sandbox::plan(store, task) {
        let words = crate::sandbox::words(&p, fenced);
        if !words.is_empty() {
            context.push_str("\n\n");
            context.push_str(words.trim());
        }
    }
    opts.context = Some(context.trim().to_string());
    let (run, cwd) = match start::prepare(store, task, opts, false)? {
        start::Outcome::Ready { run, cwd, .. } => (run, cwd),
        start::Outcome::Focus { .. } => return Ok(()),
        start::Outcome::Ask(ask) => bail!(ask.message()),
    };
    store.conn.execute("UPDATE run SET background = 1 WHERE id = ?1", [&run])?;
    // The guard compares with the files as the first round found them.
    if store
        .conn
        .query_row("SELECT base_tree IS NULL FROM check_def WHERE task = ?1", [task], |r| r.get::<_, bool>(0))
        .optional()?
        .unwrap_or(false)
    {
        if let Some(t) = tree(&cwd) {
            store.conn.execute("UPDATE check_def SET base_tree = ?2 WHERE task = ?1", params![task, t])?;
        }
    }
    if let Err(e) = crate::schedule::launch(&run, &cwd) {
        store.record_exit(&run, 127, now)?;
        return Err(e);
    }
    Ok(())
}

/// What a round is told first, after the task's title and context.
fn prompt(store: &Store, task: &str, round: i64, ups: &[String]) -> Result<String> {
    let mut out = String::new();
    for up in ups {
        let (title, state): (String, String) =
            store.conn.query_row("SELECT title, state FROM task WHERE id = ?1", [up], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        out.push_str(&format!("This task follows \"{title}\" ({}, {state})", task::short(up)));
        if let Some((_, _, branch, _)) = crate::sweep::active_worktree(store, up)? {
            out.push_str(&format!(", whose work is on branch {branch}"));
        }
        out.push('.');
        if let Some((c, _)) = varde_core::memory::cairn(&store.conn, up)? {
            if !c.status.is_empty() {
                out.push_str(&format!(" Where it stood: {}", c.status));
            }
        }
        out.push_str("\n\n");
    }
    if ups.len() > 1 {
        out.push_str(
            "Merge the branches of the tasks it follows into this one first, and resolve what conflicts.\n\n",
        );
    }
    let Some(d) = def(store, task)? else { return Ok(out.trim_end().to_string()) };
    let what = describe(&d);
    if round == 0 {
        out.push_str(&format!(
            "varde checks this work when you finish: {what}. Commit your work on this branch before you end.",
        ));
        return Ok(out);
    }
    let max = core::loop_of(&store.conn, task)?.map_or(3, |l| l.max);
    if round == 1 {
        out.push_str(&format!(
            "This task runs as a loop: varde starts a fresh session for each round until its check passes, \
             at most {max} rounds. The check is {what}."
        ));
    } else {
        let last = core::last_check(&store.conn, task)?;
        out.push_str(&format!("Round {round} of {max}. After the last round the check ({what}) failed"));
        match last.as_ref().and_then(|c| c.output.clone()) {
            Some(o) => out.push_str(&format!(":\n\n```\n{}\n```", tail(Path::new(&o), TAIL).trim())),
            None => out.push('.'),
        }
    }
    if let Some(c) = &d.cmd {
        out.push_str(&format!(
            "\n\nRun `{c}` yourself to see where it stands; varde runs it again after you finish."
        ));
    }
    if !d.guard.is_empty() {
        out.push_str(&format!(
            " Do not change {}: those are the check's, and a change to them stops the loop.",
            d.guard.join(" ")
        ));
    }
    out.push_str(
        " Commit your work on this branch before you end your turn. A tool you may not use is no question for the \
         user: carry on without it. If only the user can decide something that blocks you, end with that question.",
    );
    Ok(out)
}

/// A background session of `task` that could not start, for `why`: an
/// error its loop stops on, in place of a check of work nobody did.
pub fn not_started(store: &Store, task: &str, run: &str, why: &str) -> Result<()> {
    if def(store, task)?.is_some() {
        let now = now_ms();
        store.conn.execute(
            "INSERT INTO check_result (task, run, started, finished, verdict, summary)
             VALUES (?1, ?2, ?3, ?3, 'error', ?4)",
            params![task, run, now, format!("did not start: {why}")],
        )?;
    }
    let _ = crate::detach(&["tick"], "the tick");
    Ok(())
}

/// Called by `varde run` as a session of `task` ends: its check, if it
/// has one, else the tick, if a loop or trail might move.
pub fn ended(store: &Store, task: &str, run: &str) -> Result<()> {
    let has_check: bool =
        store
            .conn
            .query_row("SELECT EXISTS (SELECT 1 FROM check_def WHERE task = ?1)", [task], |r| r.get(0))?;
    if has_check {
        crate::detach(&["verify", task, "--run", run], "a check")?;
        return Ok(());
    }
    let moves: bool = store.conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM loop WHERE task = ?1) OR EXISTS (SELECT 1 FROM edge WHERE after = ?1)",
        [task],
        |r| r.get(0),
    )?;
    if moves {
        crate::detach(&["tick"], "the tick")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trail_draws_as_a_tree() {
        let lines: Vec<(String, String)> =
            ["spec", "store", "api", "integrate"].iter().map(|s| (s.to_string(), s.to_uppercase())).collect();
        let e = |a: &str, b: &str, k: &str| (a.to_string(), b.to_string(), k.to_string());
        let edges = vec![
            e("store", "spec", "verified"),
            e("api", "spec", "verified"),
            e("integrate", "store", "approved"),
            e("integrate", "api", "approved"),
        ];
        let drawn = draw(&lines, &edges, &|s| s.to_string());
        assert_eq!(
            drawn,
            vec![
                "SPEC  spec",
                "├─ STORE  store",
                "│  └─ T INTEGRATE  integrate  (also after api (approved))",
                "└─ API  api",
            ]
        );
        let m = mermaid(&lines, &edges, &|s| s.to_string());
        assert!(m.contains("nstore -- you approve --> nintegrate"), "{m}");
    }

    #[test]
    fn a_repository_left_inside_a_checkout_is_found_and_left_out_of_adding() {
        let r = crate::git::tests::repo();
        assert!(crate::git::nested(r.path(), true).is_empty());
        crate::git::git(r.path(), &["init", "-q", "a/sub"]).unwrap();
        assert_eq!(crate::git::nested(r.path(), true), ["a/sub"]);
        assert!(crate::git::add_all(r.path()).contains(&":(exclude)a/sub".to_string()));
    }

    #[test]
    fn every_repository_a_session_leaves_is_held_however_deep_bare_or_hidden() {
        use std::os::unix::fs::PermissionsExt;
        let r = crate::git::tests::repo();
        let dir = r.path();
        for d in ["a/b/c", "a/b/c/d/e", "hide/inner"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            crate::git::git(&dir.join(d), &["init", "-q"]).unwrap();
        }
        crate::git::git(dir, &["init", "-q", "--bare", "docs/evil"]).unwrap();
        // A bare one whose objects and refs are links: git takes it all the same.
        crate::git::git(dir, &["init", "-q", "--bare", "linked"]).unwrap();
        std::fs::rename(dir.join("linked/objects"), dir.join("objs")).unwrap();
        std::os::unix::fs::symlink(dir.join("objs"), dir.join("linked/objects")).unwrap();
        std::fs::set_permissions(dir.join("hide"), std::fs::Permissions::from_mode(0o111)).unwrap();
        let fifo = |p: &str| {
            let c = std::ffi::CString::new(p).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        };
        fifo(&format!("{}/a/.gitignore", dir.display()));
        let held = hold(dir);
        for want in ["a/b/c", "a/b/c/d/e", "docs/evil", "linked", "hide/inner"] {
            assert!(held.iter().any(|h| h == want), "{want} in {held:?}");
        }
        assert!(!dir.join("a/b/c/.git").exists() && !dir.join("hide/inner/.git").exists());
        assert!(!dir.join("docs/evil/HEAD").exists() && !dir.join("linked/HEAD").exists());
        assert!(!dir.join("a/.gitignore").exists(), "the pipe is gone");
        // git runs there again, waiting on nothing.
        assert!(crate::git::git(dir, &["status", "--porcelain"]).is_ok());
        // Held, they are held: a second look finds nothing new.
        assert!(hold(dir).is_empty(), "{:?}", hold(dir));
    }

    #[test]
    fn links_out_to_its_repositories_and_pipes_it_locked_in_are_held_too() {
        use std::os::unix::fs::PermissionsExt;
        let r = crate::git::tests::repo();
        let dir = r.path();
        let fifo = |p: &Path| {
            let c = std::ffi::CString::new(p.display().to_string()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        };
        // A repository where it could write outside (a cache), a link to it.
        let cache = tempfile::tempdir().unwrap();
        crate::git::git(cache.path(), &["init", "-q", "evil"]).unwrap();
        std::fs::create_dir(cache.path().join("evil/sub")).unwrap();
        std::os::unix::fs::symlink(cache.path().join("evil/sub"), dir.join("docs")).unwrap();
        // Or out to where a sandbox writes, a file there, nothing yet, by
        // a relative way, or by way of a link kept; but a link within the
        // checkout stays, its own `.git` (read-only to a session) within.
        let gd = PathBuf::from(crate::git::git(dir, &["rev-parse", "--absolute-git-dir"]).unwrap());
        std::os::unix::fs::symlink(gd.join("refs"), dir.join("refs-link")).unwrap();
        std::os::unix::fs::symlink("/", dir.join("slash")).unwrap();
        let through = format!("slash{}/notyet/sub", cache.path().display());
        std::os::unix::fs::symlink(&through, dir.join("through")).unwrap();
        // Out of where a sandbox writes again by `..`: a link made there
        // later would turn it back.
        std::os::unix::fs::symlink(format!("{}/a/../..", cache.path().display()), dir.join("dotdot"))
            .unwrap();
        // A hook of the user's, a link in the repository's own `.git`, stays.
        std::os::unix::fs::symlink("../../a", gd.join("hooks/pre-push")).unwrap();
        std::os::unix::fs::symlink("/usr/share", dir.join("share")).unwrap();
        std::fs::write(cache.path().join("notes.md"), "").unwrap();
        std::os::unix::fs::symlink(cache.path().join("notes.md"), dir.join("NOTES.md")).unwrap();
        std::os::unix::fs::symlink(cache.path().join("later"), dir.join("later")).unwrap();
        std::os::unix::fs::symlink("../elsewhere", dir.join("up")).unwrap();
        // But a link out to what no sandbox writes (a venv's python) stays.
        std::os::unix::fs::symlink("/usr/bin/env", dir.join("python")).unwrap();
        std::os::unix::fs::symlink("ro/../ro", dir.join("inner")).unwrap();
        // A bare one whose objects lead nowhere from here.
        crate::git::git(dir, &["init", "-q", "--bare", "tools"]).unwrap();
        std::fs::remove_dir_all(dir.join("tools/objects")).unwrap();
        std::os::unix::fs::symlink("/proc/self/cwd/.o", dir.join("tools/objects")).unwrap();
        // A pipe in a directory it made read-only, and one named to move
        // the terminal.
        std::fs::create_dir(dir.join("ro")).unwrap();
        fifo(&dir.join("ro/.gitignore"));
        std::fs::set_permissions(dir.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
        crate::git::git(dir, &["init", "-q", "x\x1b[2Jy"]).unwrap();
        // Its own branch a pipe.
        std::fs::create_dir_all(gd.join("refs/heads/varde")).unwrap();
        fifo(&gd.join("refs/heads/varde/t"));
        let held = hold(dir);
        let links = ["docs", "NOTES.md", "later", "up", "through", "dotdot"];
        for want in links.iter().chain(&["tools", "x?[2Jy"]) {
            assert!(held.iter().any(|h| h == want), "{want} in {held:?}");
        }
        for gone in links {
            assert!(std::fs::symlink_metadata(dir.join(gone)).is_err(), "{gone}");
        }
        assert!(std::fs::symlink_metadata(gd.join("hooks/pre-push")).is_ok());
        for stays in ["inner", "share", "python", "refs-link", "slash"] {
            assert!(!held.iter().any(|h| h == stays), "{stays} in {held:?}");
            assert!(std::fs::symlink_metadata(dir.join(stays)).is_ok(), "{stays}");
        }
        assert!(!held.iter().any(|h| h.contains('\x1b')), "{held:?}");
        assert!(std::fs::symlink_metadata(dir.join("ro/.gitignore")).is_err(), "the pipe is gone");
        assert!(std::fs::symlink_metadata(gd.join("refs/heads/varde/t")).is_err());
        assert!(crate::git::git(dir, &["status", "--porcelain"]).is_ok());
        assert!(hold(dir).is_empty(), "{:?}", hold(dir));
    }

    #[test]
    fn a_rebase_or_a_head_its_worktree_git_directory_was_left_with_is_held() {
        let r = crate::git::tests::repo();
        let wt = r.path().join("wt");
        crate::git::git(r.path(), &["worktree", "add", "-q", "-b", "varde/t", wt.to_str().unwrap()]).unwrap();
        let gd = PathBuf::from(crate::git::git(&wt, &["rev-parse", "--absolute-git-dir"]).unwrap());
        assert!(hold(&wt).is_empty(), "{:?}", hold(&wt));
        std::fs::create_dir(gd.join("rebase-merge")).unwrap();
        std::fs::write(gd.join("rebase-merge/git-rebase-todo"), "exec touch PWNED\n").unwrap();
        std::fs::write(gd.join("HEAD"), "ref: refs/heads/varde/../../../x\n").unwrap();
        // A link git would append a reflog through, and a pipe for its index.
        let rc = tempfile::NamedTempFile::new().unwrap();
        let _ = std::fs::remove_file(gd.join("logs/HEAD"));
        std::os::unix::fs::symlink(rc.path(), gd.join("logs/HEAD")).unwrap();
        let _ = std::fs::remove_file(gd.join("index"));
        let c = std::ffi::CString::new(gd.join("index").display().to_string()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // Links to its git directory, or into it by way of another, go.
        std::fs::create_dir(gd.join("w")).unwrap();
        std::os::unix::fs::symlink(&gd, gd.join("w/.git")).unwrap();
        std::os::unix::fs::symlink(gd.join("w"), wt.join("back")).unwrap();
        std::os::unix::fs::symlink(gd.join("refs"), wt.join("refs-link")).unwrap();
        // A submodule of its own, kept in its git directory's `modules/`.
        std::fs::create_dir_all(gd.join("modules/sm")).unwrap();
        std::fs::create_dir(wt.join("sm")).unwrap();
        std::fs::write(wt.join("sm/.git"), format!("gitdir: {}\n", gd.join("modules/sm").display())).unwrap();
        let held = hold(&wt);
        assert!(held.iter().any(|h| h == "back") && held.iter().any(|h| h == "refs-link"), "{held:?}");
        assert!(wt.join("sm/.git").is_file() && !held.iter().any(|h| h == "sm"), "{held:?}");
        assert!(held.iter().any(|h| h.contains("rebase-merge")), "{held:?}");
        assert!(held.iter().any(|h| h.starts_with("its HEAD")), "{held:?}");
        assert!(held.iter().any(|h| h.contains("write through")), "{held:?}");
        assert!(
            !gd.join("rebase-merge").exists() && std::fs::symlink_metadata(gd.join("logs/HEAD")).is_err()
        );
        assert!(std::fs::symlink_metadata(gd.join("index")).is_err());
        // A HEAD out of refs/, or on the main checkout's branch, is flagged;
        // its own branch is not.
        let main = std::fs::read_to_string(r.path().join(".git/HEAD")).unwrap();
        for head in ["ref: refs/heads/../../../x\n", main.as_str()] {
            std::fs::write(gd.join("HEAD"), head).unwrap();
            assert!(hold(&wt).iter().any(|h| h.starts_with("its HEAD")), "{head}");
        }
        std::fs::write(gd.join("HEAD"), "ref: refs/heads/varde/t\n").unwrap();
        assert!(hold(&wt).is_empty(), "{:?}", hold(&wt));
        // A link out committed, or only staged, and gone from the files:
        // git would write it back on a checkout, a merge, a restore.
        let cache = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| crate::git::git(&wt, args).unwrap();
        std::os::unix::fs::symlink(cache.path(), wt.join("docs")).unwrap();
        git(&["add", "docs"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "docs"]);
        std::fs::remove_file(wt.join("docs")).unwrap();
        std::os::unix::fs::symlink(cache.path(), wt.join("staged")).unwrap();
        git(&["add", "staged"]);
        std::fs::remove_file(wt.join("staged")).unwrap();
        let held = hold(&wt);
        assert!(held.iter().any(|h| h == "a link out it committed, at docs"), "{held:?}");
        assert!(held.iter().any(|h| h.contains("or a link out")), "{held:?}");
        // Set aside, and made again from HEAD: without what it staged.
        assert!(gd.join("index.varde-held").exists() && !git(&["ls-files"]).contains("staged"));
        // Committed on its branch and left by a HEAD elsewhere; a target cut
        // short by a NUL, as git writes it; a link into a `.git`.
        git(&["reset", "-q"]);
        git(&["rm", "-q", "--cached", "docs"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "clean"]);
        let blob = |t: &[u8]| {
            let mut c = crate::git::command(&wt, &["hash-object", "-w", "--stdin"]);
            let o = crate::git::bounded_input(&mut c, t).unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let mut nul = cache.path().as_os_str().as_bytes().to_vec();
        nul.extend(b"\0x");
        let big = vec![b'a'; 5000];
        let links = [
            ("nul", nul),
            ("hook", b".git/hooks/pre-commit".to_vec()),
            // Out of the repository only from where it is merged.
            ("esc", b"../.cache/pip/evil".to_vec()),
            ("turn", b"sub/../../x".to_vec()),
            ("big", big),
            ("fine", b"README".to_vec()),
        ];
        for (name, target) in links {
            let info = format!("120000,{},{name}", blob(&target));
            git(&["update-index", "--add", "--cacheinfo", &info]);
        }
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "links"]);
        git(&["checkout", "-q", "--detach", "HEAD~1"]);
        let held = hold(&wt);
        let committed =
            held.iter().find(|h| h.starts_with("a link out it committed")).cloned().unwrap_or_default();
        for name in ["nul", "hook", "esc", "turn", "big"] {
            assert!(committed.contains(name), "{name}: {held:?}");
        }
        assert!(!committed.contains("fine"), "{held:?}");
        // A HEAD it broke stops no look at its branch; a branch naming a
        // commit not there is said.
        std::fs::write(gd.join("HEAD"), "garbage\n").unwrap();
        let held = hold(&wt);
        assert!(
            held.iter().any(|h| h.starts_with("a link out it committed") && h.contains("esc")),
            "{held:?}"
        );
        let common = r.path().join(".git");
        std::fs::write(common.join("refs/heads/varde/gone"), format!("{}\n", "ab".repeat(20))).unwrap();
        assert!(hold(&wt).iter().any(|h| h.contains("does not have")), "{:?}", hold(&wt));
    }

    #[test]
    fn a_link_out_the_repository_has_already_is_not_the_sessions() {
        let r = crate::git::tests::repo();
        std::os::unix::fs::symlink("/usr/share/zoneinfo", r.path().join("zi")).unwrap();
        crate::git::git(r.path(), &["add", "zi"]).unwrap();
        crate::git::git(r.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "zi"])
            .unwrap();
        let wt = r.path().join("wt");
        crate::git::git(r.path(), &["worktree", "add", "-q", "-b", "varde/t", wt.to_str().unwrap()]).unwrap();
        assert!(hold(&wt).is_empty(), "{:?}", hold(&wt));
        assert!(crate::git::git(&wt, &["status", "--porcelain"]).unwrap().is_empty());
        // Its index made again from its entries alone: no tree cached for a
        // commit (which git would trust over them) is left.
        crate::git::git(&wt, &["read-tree", "HEAD"]).unwrap();
        crate::git::git(&wt, &["write-tree"]).unwrap();
        let gd = PathBuf::from(crate::git::git(&wt, &["rev-parse", "--absolute-git-dir"]).unwrap());
        assert!(std::fs::read(gd.join("index")).unwrap().windows(4).any(|w| w == b"TREE"));
        assert!(hold(&wt).is_empty(), "{:?}", hold(&wt));
        assert!(!std::fs::read(gd.join("index")).unwrap().windows(4).any(|w| w == b"TREE"));
        assert!(crate::git::git(&wt, &["status", "--porcelain"]).unwrap().is_empty());
        // But not by a branch a session can move: the main checkout on a
        // varde one, moved to its commit.
        crate::git::git(r.path(), &["checkout", "-q", "-b", "varde/b"]).unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(cache.path(), wt.join("docs")).unwrap();
        crate::git::git(&wt, &["add", "docs"]).unwrap();
        crate::git::git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "docs"])
            .unwrap();
        std::fs::remove_file(wt.join("docs")).unwrap();
        let id = crate::git::git(&wt, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(r.path().join(".git/refs/heads/varde/b"), format!("{id}\n")).unwrap();
        let held = hold(&wt);
        assert!(
            held.iter().any(|h| h.starts_with("a link out it committed") && h.contains("docs")),
            "{held:?}"
        );
    }

    #[test]
    fn the_same_failure_prints_the_same() {
        assert_eq!(fingerprint("test a failed in 0.31s\n"), fingerprint("test a failed in 1.07s"));
        assert_ne!(fingerprint("test a failed"), fingerprint("test b failed"));
        assert_eq!(
            summarize("running 3 tests\ntest result: FAILED. 2 passed; 1 failed\n").as_deref(),
            Some("test result: FAILED. 2 passed; 1 failed")
        );
    }
}
