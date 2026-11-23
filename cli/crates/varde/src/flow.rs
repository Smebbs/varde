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
    let real = crate::git::command(&root, &["rev-parse", "--path-format=absolute", "--git-path", "index"])
        .output()
        .ok()?;
    let real = PathBuf::from(String::from_utf8_lossy(&real.stdout).trim());
    if real.is_file() {
        std::fs::copy(&real, scratch.path()).ok()?;
    }
    let ok = crate::git::command(&root, &["add", "-A"])
        .env("GIT_INDEX_FILE", scratch.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let out =
        crate::git::command(&root, &["write-tree"]).env("GIT_INDEX_FILE", scratch.path()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The paths that differ between two trees.
fn changed(dir: &Path, a: &str, b: &str) -> Vec<String> {
    crate::git::command(dir, &["diff-tree", "-r", "--name-only", a, b])
        .output()
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
    pub guard: Vec<String>,
    pub again: u32,
    pub base_tree: Option<String>,
}

pub fn def(store: &Store, task: &str) -> Result<Option<Def>> {
    Ok(store
        .conn
        .query_row(
            "SELECT cmd, review, rubric, reviewer, guard, again, base_tree FROM check_def WHERE task = ?1",
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
                })
            },
        )
        .optional()?)
}

/// Set a task's check, keeping what is not given.
pub fn set_def(store: &Store, task: &str, d: &Def) -> Result<()> {
    store.conn.execute(
        "INSERT INTO check_def (task, cmd, review, rubric, reviewer, guard, again, base_tree, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (task) DO UPDATE SET cmd = ?2, review = ?3, rubric = ?4, reviewer = ?5, guard = ?6,
           again = ?7, base_tree = coalesce(?8, base_tree)",
        params![
            task,
            d.cmd,
            d.review,
            d.rubric,
            d.reviewer,
            (!d.guard.is_empty()).then(|| serde_json::to_string(&d.guard).unwrap_or_default()),
            d.again,
            d.base_tree,
            now_ms()
        ],
    )?;
    Ok(())
}

/// What `varde check` was asked.
pub struct CheckArgs {
    pub cmd: Vec<String>,
    pub review: Option<Option<String>>,
    pub rubric: Option<String>,
    pub guard: Vec<String>,
    pub again: Option<u32>,
    pub now: bool,
    pub off: bool,
}

/// `varde check <task> …`.
pub fn check(paths: &Paths, reference: &str, a: CheckArgs) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
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
        || !a.guard.is_empty()
        || a.again.is_some();
    let mut d = def(&store, &id)?.unwrap_or(Def {
        cmd: None,
        review: false,
        rubric: None,
        reviewer: None,
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
        if !a.guard.is_empty() {
            d.guard = a.guard;
        }
        if let Some(n) = a.again {
            d.again = n.max(1);
        }
        if d.cmd.is_none() && !d.review {
            bail!("a check needs a command (varde check {short} -- CMD) or a review (--review)");
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
fn rubric_arg(r: &str) -> Result<String> {
    let p = Path::new(r);
    Ok(if p.is_file() { p.canonicalize()?.display().to_string() } else { r.to_string() })
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
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, task)?;
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
    let _ = crate::detach(&["tick"], "the tick");
    Ok(())
}

fn once(paths: &Paths, task: &str, run: Option<String>) -> Result<()> {
    let store = paths.open(BUSY)?;
    let Some(d) = def(&store, task)? else { return Ok(()) };
    let run = match run {
        Some(r) => Some(r),
        None => store
            .conn
            .query_row(
                "SELECT id FROM run WHERE task = ?1 ORDER BY started DESC, rowid DESC LIMIT 1",
                [task],
                |r| r.get(0),
            )
            .optional()?,
    };
    let started = now_ms();
    let dir = checkout(&store, task)?;
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
            let before = tree(dir);
            if let Some(cmd) = &d.cmd {
                result = run_command(dir, cmd, d.again, &mut out)?;
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
            if result.verdict == Verdict::Pass && d.review {
                let store = paths.open(BUSY)?;
                result = review(paths, &store, task, &d, dir, after.as_deref(), &out_path, &mut out)?;
            }
        }
    }
    let store = paths.open(BUSY)?;
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

/// Run the command `again` times, stopping at the first failure.
fn run_command(dir: &Path, cmd: &str, again: u32, out: &mut File) -> Result<Found> {
    let limit = Duration::from_secs(config::get().loops.check_minutes.max(1) * 60);
    let mut last = Found { verdict: Verdict::Pass, exit: Some(0), summary: None, fingerprint: None };
    for i in 0..again {
        if again > 1 {
            writeln!(out, "== pass {} of {again} ==", i + 1)?;
        }
        let mut child = match Command::new("sh")
            .args(["-c", cmd])
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
    let rubric = d.rubric.as_deref().map(|r| std::fs::read_to_string(r).unwrap_or_else(|_| r.to_string()));
    let diff = match (&d.base_tree, after) {
        (Some(a), Some(b)) => {
            crate::git::command(dir, &["diff", a, b]).output().map(|o| o.stdout).unwrap_or_default()
        }
        _ => crate::git::command(dir, &["diff", "HEAD"]).output().map(|o| o.stdout).unwrap_or_default(),
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
    let next: Vec<String> = store
        .conn
        .prepare("SELECT task FROM edge WHERE after = ?1")?
        .query_map([&id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let short = task::short(&id);
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
            Some(cmd) => set_def(&store, &id, &Def { cmd: Some(cmd), review: false, rubric: None, reviewer: None, guard: vec![], again: 1, base_tree: None })?,
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
        "{short} loops until {} passes, at most {} rounds, in the background; varde show {short} follows it",
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
        if let Some(w) = config::get().background_warning(p.split(':').next().unwrap_or(&p)) {
            eprintln!("varde: {w}");
        }
    }
    drop(store);
    crate::schedule::tick(paths)
}

/// A session of yours on a task takes it over: its loop pauses.
pub fn taken_over(store: &Store, task: &str) -> Result<bool> {
    Ok(store.conn.execute("UPDATE loop SET paused = 1 WHERE task = ?1 AND paused = 0", [task])? > 0)
}

// ---------------------------------------------------------------- edges

/// `varde after <task> <dep>…`: the task waits until each is verified
/// (`approved`: until you approve each).
pub fn after(paths: &Paths, reference: &str, deps: &[String], approved: bool, off: bool) -> Result<()> {
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
        add_edge(&store, &id, &dep, approved)?;
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

pub fn add_edge(store: &Store, task: &str, after: &str, approved: bool) -> Result<()> {
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
    Ok(())
}

// ---------------------------------------------------------------- trail

/// `varde trail <task> [--start] [--format text|mermaid] [--json]`.
pub fn trail(paths: &Paths, reference: &str, go: bool, format: &str, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let tasks = core::trail(&store.conn, &id)?;
    if go {
        let mut n = 0;
        for t in &tasks {
            n += store
                .conn
                .execute("UPDATE task SET auto = 1, inbox = 0 WHERE id = ?1 AND state = 'open'", [t])?;
        }
        println!("set {n} tasks going; each starts in the background once what it waits on is verified");
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
        let rounds = core::rounds(&store.conn, task, l.started, now, &alive)?;
        let checks = core::checks(&store.conn, task, l.started)?;
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
                task::short(&r.run)
            ));
        }
    }
    let waits = core::waits(&store.conn, task)?;
    let dependents: Vec<String> = store
        .conn
        .prepare("SELECT task FROM edge WHERE after = ?1")?
        .query_map([task], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
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

/// `1 round`, `3 rounds`.
fn n_rounds(n: i64) -> String {
    format!("{n} round{}", if n == 1 { "" } else { "s" })
}

/// A loop's state, in words.
pub fn state_words(st: &State) -> String {
    match st {
        State::Paused { rounds } => format!("paused after {}; varde loop resumes it", n_rounds(*rounds)),
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
        State::Asked { .. } => "a round asked you something; answer it by opening the task".into(),
    }
}

/// The same, as `show --json` has it.
pub fn json(paths: &Paths, store: &Store, task: &str) -> Result<serde_json::Value> {
    let now = now_ms();
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let check = def(store, task)?.map(|d| {
        serde_json::json!({"cmd": d.cmd, "review": d.review, "reviewer": d.reviewer, "rubric": d.rubric,
                           "guard": d.guard, "again": d.again, "running": checking(paths, task)})
    });
    let last = core::last_check(&store.conn, task)?;
    let lp = match core::loop_of(&store.conn, task)? {
        Some(l) => {
            let st = core::state(&store.conn, task, now, &alive)?;
            let rounds = core::rounds(&store.conn, task, l.started, now, &alive)?;
            Some(serde_json::json!({"def": l, "state": st, "rounds": rounds,
                                    "checks": core::checks(&store.conn, task, l.started)?}))
        }
        None => None,
    };
    let before: Vec<String> = store
        .conn
        .prepare("SELECT task FROM edge WHERE after = ?1 ORDER BY rowid")?
        .query_map([task], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (profile, recipe): (Option<String>, Option<String>) =
        store.conn.query_row("SELECT profile, recipe FROM task WHERE id = ?1", [task], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    Ok(serde_json::json!({
        "profile": profile, "recipe": recipe, "before": before,
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
    opts.context = Some(prompt(store, task, round, &ups)?);
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
    fn the_same_failure_prints_the_same() {
        assert_eq!(fingerprint("test a failed in 0.31s\n"), fingerprint("test a failed in 1.07s"));
        assert_ne!(fingerprint("test a failed"), fingerprint("test b failed"));
        assert_eq!(
            summarize("running 3 tests\ntest result: FAILED. 2 passed; 1 failed\n").as_deref(),
            Some("test result: FAILED. 2 passed; 1 failed")
        );
    }
}
