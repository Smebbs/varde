//! `varde new`, `set`, `accept`, `link`, `unlink` and `show`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use varde_core::store::Store;
use varde_core::{memory, project, task};

use crate::{cairn_view, git, now_ms, Paths};

const BUSY: Duration = Duration::from_secs(2);

#[allow(clippy::too_many_arguments)]
pub fn new(
    paths: &Paths,
    title: &[String],
    project_dir: Option<PathBuf>,
    body: Option<String>,
    external: Option<String>,
    from: Option<String>,
    when: Option<(String, Option<bool>)>,
    as_json: bool,
) -> Result<()> {
    let store = paths.open(BUSY)?;
    let body = crate::arg_or_stdin(body)?.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    if body.is_none() && varde_core::title::clean(&title.join(" ")).trim().is_empty() {
        bail!("a task needs a title, or --context TEXT to cut one from");
    }
    let id = match from {
        Some(run) => {
            // By its id, or the six characters a session is told.
            let run = crate::memory_cmd::resolve_run(&store.conn, &run)?;
            let root = project_dir.map(|d| crate::start::project_of(&store, &d)).transpose()?;
            let root = root.map(|r| r.display().to_string());
            task::defer(&store, &title.join(" "), body.as_deref(), &run, root.as_deref(), now_ms())?
        }
        None => {
            let root = match project_dir {
                Some(d) => crate::start::project_of(&store, &d)?,
                None => project::of(&store.conn, &std::env::current_dir().context("current directory")?),
            };
            // Checked before anything is made, so a bad rule leaves nothing behind.
            if let Some((rule, worktree)) = &when {
                crate::schedule::check(&root, rule, *worktree)?;
            }
            task::create(
                &store,
                &title.join(" "),
                body.as_deref(),
                Some(&root.display().to_string()),
                external.as_deref(),
                now_ms(),
            )?
        }
    };
    let scheduled = match when {
        Some((rule, worktree)) => Some(crate::schedule::arm(&store, &id, &rule, worktree)?),
        None => None,
    };
    crate::naming::start(&store, &id);
    if let Some(said) = scheduled.filter(|_| !as_json) {
        eprintln!("varde: {said}");
    }
    if as_json {
        println!("{}", json!({"v": 1, "id": id, "short": task::short(&id)}));
    } else if unsafe { libc::isatty(1) } == 1 {
        let short = task::short(&id);
        println!("created {short}; varde attach {short} starts it");
    } else {
        // Only the id, for `t=$(varde new …)`.
        println!("{}", task::short(&id));
    }
    Ok(())
}

pub fn set(
    paths: &Paths,
    reference: &str,
    title: Option<String>,
    body: Option<String>,
    project_dir: Option<PathBuf>,
) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let body = crate::arg_or_stdin(body)?;
    // A refused move changes nothing, the title and body included, and
    // nothing starts the task between the checks and the move.
    let tx = Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate)?;
    let to = project_dir.map(|d| moving(&store, &id, &d)).transpose()?.flatten();
    let set = task::set(&store, &id, title.as_deref(), body.as_deref())?;
    if let Some(to) = &to {
        task::set_project(&store, &id, to)?;
    }
    tx.commit()?;
    if set.title {
        crate::naming::start(&store, &id);
    }
    // What changed, and nothing when nothing did: clients send the
    // title and context before every start.
    let short = task::short(&id);
    let what = match (set.title, set.body) {
        (true, true) => Some("title and context"),
        (true, false) => Some("title"),
        (false, true) => Some("context"),
        (false, false) => None,
    };
    match (to, what) {
        (Some(to), Some(what)) => println!("moved {short} to {to}; {what} changed"),
        (Some(to), None) => println!("moved {short} to {to}"),
        (None, Some(what)) => println!("{short}: {what} changed"),
        (None, None) => {}
    }
    Ok(())
}

/// Move a task to the project `dir` is in, as `varde set --project`
/// does; what happened, for a message line.
pub fn move_to(store: &Store, id: &str, dir: &Path) -> Result<String> {
    let short = task::short(id);
    let tx = Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate)?;
    let said = match moving(store, id, dir)? {
        Some(to) => {
            task::set_project(store, id, &to)?;
            format!("moved {short} to {to}")
        }
        None => format!("{short} is already in that project"),
    };
    tx.commit()?;
    Ok(said)
}

/// The project a move of the task to `dir` takes it to; `None` when it
/// is there already. Refused while the task has an active worktree, work
/// kept from its drop, or a live session: they belong to the checkout it
/// is in. Its past runs keep their directories and its cairn stays with it.
fn moving(store: &Store, id: &str, dir: &Path) -> Result<Option<String>> {
    let to = crate::start::project_of(store, dir)?.display().to_string();
    let old: Option<String> =
        store.conn.query_row("SELECT project FROM task WHERE id = ?1", [id], |r| r.get(0))?;
    if old.as_deref() == Some(to.as_str()) {
        return Ok(None);
    }
    let short = task::short(id);
    let old = old.unwrap_or_else(|| "its project".into());
    if let Some((path, ..)) = crate::sweep::active_worktree(store, id)? {
        bail!(
            "{short} has an active worktree, {path}, whose work belongs to {old}: \
             it stays there until that work has landed and the worktree is removed"
        );
    }
    // A start brings back what a drop kept, but only in the repo it was
    // dropped from.
    let last: Option<(String, String, String)> = store
        .conn
        .query_row(
            "SELECT repo, path, state FROM worktree WHERE task = ?1 ORDER BY created DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((repo, path, _)) = last.filter(|(_, _, state)| state == "removed") {
        let kept = git::discarded_ref(Path::new(&path));
        if git::rev(Path::new(&repo), &kept).is_some() {
            bail!(
                "{short} has work kept from its drop at {kept} in {repo}, which starting it there brings back: \
                 start it and finish that work first, or delete the ref (git -C {repo} update-ref -d {kept}) \
                 to move it without",
            );
        }
    }
    if let Some(run) = crate::start::run_facts(store, id)?.into_iter().find(|r| r.live()) {
        let place = run.cwd.map(|c| format!(" in {c}")).unwrap_or_default();
        bail!("{short} has a live session{place}, which belongs to {old}: end it first");
    }
    Ok(Some(to))
}

pub fn accept(paths: &Paths, references: &[String]) -> Result<()> {
    let store = paths.open(BUSY)?;
    // Every reference is checked before any task moves.
    let ids = references.iter().map(|r| task::resolve(&store, r)).collect::<Result<Vec<_>>>()?;
    let mut outside = Vec::new();
    for id in ids {
        let inbox: bool =
            store.conn.query_row("SELECT inbox FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
        if inbox {
            task::accept(&store, &id)?;
            println!("accepted {}", task::short(&id));
        } else {
            outside.push(task::short(&id));
        }
    }
    // Named alone, a task not in the Inbox is a mistake, as on the board.
    match outside.as_slice() {
        [] => {}
        [one] if references.len() == 1 => bail!("{one} is not in the Inbox: accept is for Inbox tasks"),
        _ if outside.len() == references.len() => {
            bail!("none of them is in the Inbox: accept is for Inbox tasks")
        }
        _ => println!("not in the Inbox, so left as they are: {}", outside.join(", ")),
    }
    Ok(())
}

pub fn link(paths: &Paths, reference: &str, external: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    task::link(&store, &id, external)
}

pub fn unlink(paths: &Paths, reference: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    task::unlink(&store, &id)
}

pub fn show(paths: &Paths, reference: &str, history: bool, with_git: bool, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let d = task::detail(&store, &id, &crate::process::alive)?;
    let cairn = memory::cairn(&store.conn, &id)?.map(|(c, _)| c);
    let usage = memory::usage(&store.conn, memory::Owner::Task(&id))?;
    let notes: Vec<String> = memory::pending_notes(&store.conn, &id)?.into_iter().map(|(_, t)| t).collect();
    let versions = if history { memory::history(&store.conn, &id)? } else { Vec::new() };
    let schedule = crate::schedule::of(&store, &id)?;
    let said = match &schedule {
        Some(s) if !as_json => Some(s.text(&store, paths, &id)?),
        _ => None,
    };
    // A conversation left by `/clear` or `/resume` has ended while its
    // process goes on: where the session went.
    let moved: Vec<(String, String)> = d
        .runs
        .iter()
        .filter(|r| r.status == Some("exited") && r.why.alive == Some(true))
        .filter_map(|r| {
            let (next, task): (String, Option<String>) = store
                .conn
                .query_row(
                    "SELECT id, task FROM run WHERE parent_run = ?1 ORDER BY started DESC, rowid DESC LIMIT 1",
                    [&r.id],
                    |x| Ok((x.get(0)?, x.get(1)?)),
                )
                .ok()?;
            let other = task.filter(|t| *t != id).map(|t| format!(" of task {}", task::short(&t)));
            Some((r.id.clone(), format!("moved on to run {}{}", task::short(&next), other.unwrap_or_default())))
        })
        .collect();
    // Nothing is held open while git and gh run.
    drop(store);
    let facts = if with_git { git_facts(&d) } else { None };
    if as_json {
        let mut v = serde_json::to_value(&d)?;
        // `body` is the old name, kept for v2 clients.
        v["context"] = v["body"].clone();
        v["cairn"] = serde_json::to_value(&cairn)?;
        v["usage"] = serde_json::to_value(&usage)?;
        v["notes"] = json!(notes);
        v["limits"] = varde_core::cairn::Shape::Task.limits();
        v["schedule"] = schedule.as_ref().map_or(Value::Null, crate::schedule::Shown::json);
        if with_git {
            v["git"] = facts.as_ref().map_or(Value::Null, git::Facts::to_json);
        }
        if history {
            v["history"] = serde_json::to_value(&versions)?;
        }
        println!("{v}");
        return Ok(());
    }
    let git = facts.as_ref().map(describe_git);
    let pr = facts.as_ref().and_then(|f| f.pr.as_ref()).map(|p| {
        let draft = if p.draft { " draft" } else { "" };
        let checks = p.checks.map(|c| format!(", checks {c}")).unwrap_or_default();
        format!("#{} {}{draft}{checks}  {}", p.number, p.state, p.url)
    });
    let view = View { git, pr, moved, sessions: usize::MAX, schedule: said, home: paths.home.clone() };
    let add = format!("varde edit {} --add findings TEXT adds a line now", d.short);
    let block =
        cairn_view::lines(cairn_view::Whose::Task, cairn.as_ref(), &usage, &notes, &add, false, now_ms());
    for (kind, line) in view.lines(&d, cairn_view::text(&block), now_ms()) {
        match kind {
            Part::Heading => println!("\n{line}"),
            _ => println!("{line}"),
        }
    }
    crate::print_history(&versions);
    Ok(())
}

/// What a line of a task's view is, for whoever styles it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Title,
    /// Where it stands, its project and worktree: one line each.
    Fact,
    /// A section's name.
    Heading,
    Text,
    /// What only matters when something looks wrong.
    Quiet,
}

/// What a task's view shows besides the task itself: its git status,
/// its PR, where moved sessions went, and how many sessions to list.
pub struct View {
    pub git: Option<String>,
    pub pr: Option<String>,
    pub moved: Vec<(String, String)>,
    pub sessions: usize,
    /// Its schedule, as `varde schedule` says it.
    pub schedule: Option<String>,
    /// The store's directory, under which a background run's log is.
    pub home: PathBuf,
}

impl View {
    /// The task as `varde show` prints it: what it is and where it
    /// stands, its `cairn`'s lines, the context it was given, then its
    /// sessions, newest first.
    pub fn lines(&self, d: &task::Detail, cairn: Vec<(Part, String)>, now: i64) -> Vec<(Part, String)> {
        let mut out = vec![(Part::Title, d.title.clone())];
        out.extend(self.facts(d));
        out.extend(cairn);
        out.extend(self.rest(d, now));
        out
    }

    /// Where the task stands, its project and worktree: one line each.
    pub fn facts(&self, d: &task::Detail) -> Vec<(Part, String)> {
        let mut out = Vec::new();
        let live = d.runs.last().and_then(|r| r.status).and_then(crate::look::word);
        let mut facts = vec![d.short.clone(), live.map_or(d.state.clone(), |(w, _)| w.to_string())];
        if d.inbox {
            facts.push("in the Inbox".into());
        }
        if let Some(p) = &d.project {
            facts.push(format!("{} ({})", crate::list::project_name(Some(p)), tilde(p)));
        }
        if let Some(o) = &d.origin {
            facts.push(format!("deferred from {}", task::short(o)));
        }
        if let Some(e) = &d.external {
            facts.push(format!("linked to {e}"));
        }
        out.push((Part::Fact, facts.join(" · ")));
        if let Some(p) = &d.prompt_title {
            out.push((Part::Quiet, format!("named by the model; first titled: {p}")));
        }
        if let Some(s) = &self.schedule {
            out.push((Part::Fact, format!("scheduled {s}")));
        }
        if let Some(w) = d.worktree.as_ref().filter(|w| w.state == "active") {
            out.push((Part::Fact, format!("worktree {} on {} from {}", tilde(&w.path), w.branch, w.base)));
        }
        if let Some(g) = &self.git {
            out.push((Part::Fact, format!("git {g}")));
        }
        if let Some(p) = &self.pr {
            out.push((Part::Fact, format!("PR {p}")));
        }
        out
    }

    /// What follows the cairn: the context the task was given, then its
    /// sessions, newest first.
    pub fn rest(&self, d: &task::Detail, now: i64) -> Vec<(Part, String)> {
        let mut out = Vec::new();
        if let Some(b) = d.body.as_deref().filter(|b| !b.trim().is_empty()) {
            out.push((Part::Heading, "Context".into()));
            out.extend(b.lines().map(|l| (Part::Text, format!("  {l}"))));
        }
        let runs: Vec<&task::RunDetail> = d.runs.iter().rev().collect();
        if !runs.is_empty() {
            out.push((Part::Heading, "Sessions".into()));
        }
        // Of a schedule's runs that found nothing, the newest few: one every
        // 30 minutes would bury the rest.
        let (mut quiet, mut folded) = (0, 0);
        for r in runs.iter().take(self.sessions) {
            if r.background && r.done.is_some() {
                quiet += 1;
                if quiet > 10 {
                    folded += 1;
                    continue;
                }
            }
            let status = match r.status {
                _ if !r.launched => "not launched yet",
                Some(s) => crate::look::word(s).map_or(if s == "exited" { "ended" } else { s }, |(w, _)| w),
                None => "starting",
            };
            let held = if r.held_out { " · held out of memory" } else { "" };
            // What a run its schedule started left you: a result, or nothing.
            // One that failed to start, or died, says so, and where it said why.
            let failed = r.background && r.exit_code.is_some_and(|c| c != 0 && c != 143);
            let held = match (r.background, r.done.is_some(), r.unread) {
                (false, ..) => held.to_string(),
                _ if failed => format!(
                    "{held} · in the background{} · failed (exit {}): {}",
                    if r.unread { " · unread" } else { "" },
                    r.exit_code.unwrap_or_default(),
                    tilde(&self.home.join("background").join(format!("{}.log", r.id)).to_string_lossy())
                ),
                (true, true, _) => format!("{held} · in the background · nothing to report"),
                (true, _, true) => format!("{held} · in the background · unread"),
                (true, ..) => format!("{held} · in the background"),
            };
            out.push((
                Part::Text,
                format!(
                    "  {}  {status} · {} {}{}{held}",
                    task::short(&r.id),
                    match r.relation.as_str() {
                        "resume" => "resumed",
                        "fork" => "forked",
                        _ => "started",
                    },
                    clock(r.started, now),
                    // Where it ran, when that is not where the task is.
                    r.cwd
                        .as_deref()
                        .filter(|c| Some(*c) != d.project.as_deref()
                            && Some(*c) != d.worktree.as_ref().map(|w| w.path.as_str()))
                        .map(|c| format!(" · in {}", tilde(c)))
                        .unwrap_or_default()
                ),
            ));
            if let Some(why) = describe_why(&r.why, now) {
                let moved = self.moved.iter().find(|(run, _)| *run == r.id).map(|(_, to)| format!("; {to}"));
                out.push((Part::Quiet, format!("          {why}{}", moved.unwrap_or_default())));
            }
        }
        if folded > 0 {
            out.push((Part::Quiet, format!("  and {folded} earlier that found nothing")));
        }
        if runs.len() > self.sessions {
            out.push((
                Part::Quiet,
                format!("  and {} earlier: varde show {}", runs.len() - self.sessions, d.short),
            ));
        }
        out
    }
}

/// `path` with the home directory as `~`.
pub fn tilde(path: &str) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute() && h != Path::new("/"));
    match home.and_then(|h| Path::new(path).strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.to_string(),
    }
}

/// Someone looked at the task's sessions: what they finished is read.
pub fn seen(paths: &Paths, reference: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    varde_core::store::mark_seen(&store.conn, &id, now_ms())?;
    Ok(())
}

/// `turn_end at 14:03:12, seen 14:05:40, process gone`: the facts a run's
/// status was derived from. `None` before its first status event.
pub fn describe_why(why: &varde_core::store::Why, now: i64) -> Option<String> {
    let event = why.event.as_deref()?;
    let said = match event {
        "prompt" => "prompted",
        "turn_end" => "turn ended",
        "delegated" => "turn ended, sub-agents at work",
        "blocked" => "asked for input",
        "unblocked" => "input given",
        other => other,
    };
    let mut parts = vec![format!("{said} {}", clock(why.at?, now))];
    if let Some(t) = why.idle_at {
        parts.push(format!("idle {}", clock(t, now)));
    }
    if let Some(t) = why.seen_at {
        parts.push(format!("seen {}", clock(t, now)));
    }
    // An ended conversation's process is gone, or has gone on to another
    // conversation: either way, nothing more to say of it here.
    match why.alive {
        _ if event == "ended" => {}
        Some(true) => parts.push("running".into()),
        Some(false) => parts.push("process gone".into()),
        None => {}
    }
    Some(parts.join(" · "))
}

/// The time of day for today, else the date too.
fn clock(ms: i64, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let (Some(t), Some(n)) =
        (Local.timestamp_millis_opt(ms).single(), Local.timestamp_millis_opt(now).single())
    else {
        return ms.to_string();
    };
    if t.date_naive() == n.date_naive() {
        t.format("%H:%M:%S").to_string()
    } else {
        t.format("%b %-d %H:%M").to_string()
    }
}

/// Git status for the task's active worktree, else for its project's
/// checkout, which it shares. `None` when there is neither to read.
fn git_facts(d: &task::Detail) -> Option<git::Facts> {
    match &d.worktree {
        Some(w) if w.state == "active" => git::facts(Path::new(&w.path), Some(&w.base), true).ok(),
        Some(_) => None,
        None => git::facts(Path::new(d.project.as_deref()?), None, true).ok(),
    }
}

/// `committed: 2 ahead, 5 behind main, 3 uncommitted, not pushed`.
fn describe_git(f: &git::Facts) -> String {
    let mut parts = vec![];
    if f.shared {
        parts.push(format!("on {}, shared with other tasks", f.branch));
    }
    if let Some(t) = &f.target {
        parts.push(format!("{} ahead, {} behind {t}", f.ahead, f.behind));
    }
    if f.changes > 0 {
        parts.push(format!("{} uncommitted", f.changes));
    }
    if !f.shared {
        match f.unpushed {
            None => parts.push("not pushed".to_string()),
            Some(0) => {}
            Some(n) => parts.push(format!("{n} unpushed")),
        }
    }
    if parts.is_empty() {
        return f.stage().to_string();
    }
    format!("{} · {}", f.stage(), parts.join(", "))
}
