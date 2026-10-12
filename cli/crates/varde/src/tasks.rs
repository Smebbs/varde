//! `varde new`, `set`, `accept`, `link`, `unlink` and `show`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use varde_core::store::Store;
use varde_core::{memory, project, task};

use crate::{git, now_ms, Paths};

const BUSY: Duration = Duration::from_secs(2);

pub fn new(
    paths: &Paths,
    title: &[String],
    project_dir: Option<PathBuf>,
    body: Option<String>,
    external: Option<String>,
    from: Option<String>,
    as_json: bool,
) -> Result<()> {
    let store = paths.open(BUSY)?;
    let body = crate::arg_or_stdin(body)?.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    let id = match from {
        Some(run) => {
            let root = project_dir.map(|d| crate::start::project_of(&store, &d)).transpose()?;
            let root = root.map(|r| r.display().to_string());
            task::defer(&store, &title.join(" "), body.as_deref(), &run, root.as_deref(), now_ms())?
        }
        None => {
            let root = match project_dir {
                Some(d) => crate::start::project_of(&store, &d)?,
                None => project::of(&store.conn, &std::env::current_dir().context("current directory")?),
            };
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
    task::set(&store, &id, title.as_deref(), body.as_deref())?;
    if let Some(to) = &to {
        task::set_project(&store, &id, to)?;
    }
    tx.commit()?;
    if let Some(to) = to {
        println!("moved {} to {to}", task::short(&id));
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
            "SELECT repo, branch, state FROM worktree WHERE task = ?1 ORDER BY created DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((repo, branch, _)) = last.filter(|(_, _, state)| state == "removed") {
        let kept = git::discarded_ref(&branch);
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
    for id in ids {
        let inbox: bool =
            store.conn.query_row("SELECT inbox FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
        task::accept(&store, &id)?;
        if inbox {
            println!("accepted {}", task::short(&id));
        } else {
            println!("{} was not in the Inbox", task::short(&id));
        }
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
    let notes: Vec<String> = memory::pending_notes(&store.conn, &id)?.into_iter().map(|(_, t)| t).collect();
    let versions = if history { memory::history(&store.conn, &id)? } else { Vec::new() };
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
        v["cairn"] = serde_json::to_value(&cairn)?;
        v["notes"] = json!(notes);
        if with_git {
            v["git"] = facts.as_ref().map_or(Value::Null, git::Facts::to_json);
        }
        if history {
            v["history"] = serde_json::to_value(&versions)?;
        }
        println!("{v}");
        return Ok(());
    }
    println!("{}  {}", d.short, d.title);
    println!("  state    {}{}", d.state, if d.inbox { " (inbox)" } else { "" });
    if let Some(p) = &d.project {
        println!("  project  {p}");
    }
    if let Some(e) = &d.external {
        println!("  linked   {e}");
    }
    if let Some(o) = &d.origin {
        println!("  from     {}", task::short(o));
    }
    if let Some(w) = &d.worktree {
        println!("  worktree {} [{} from {}] {}", w.path, w.branch, w.base, w.state);
    }
    if let Some(f) = &facts {
        println!("  git      {}", describe_git(f));
        if let Some(p) = &f.pr {
            let draft = if p.draft { " draft" } else { "" };
            let checks = p.checks.map(|c| format!(", checks {c}")).unwrap_or_default();
            println!("  pr       #{} {}{draft}{checks}  {}", p.number, p.state, p.url);
        }
    }
    if let Some(b) = &d.body {
        println!();
        for line in b.lines() {
            println!("  {line}");
        }
    }
    if let Some(c) = &cairn {
        println!("\ncairn");
        for line in c.display().lines() {
            println!("  {line}");
        }
    }
    if !notes.is_empty() {
        println!(
            "\n{}",
            if crate::config::get().memory.enabled {
                "notes, for the next cairn update"
            } else {
                "notes (cairn updates are off)"
            }
        );
        for n in &notes {
            println!("  - {n}");
        }
    }
    crate::print_history(&versions);
    if !d.runs.is_empty() {
        println!("\nruns");
        for r in &d.runs {
            println!(
                "  {}  {:<6} {:<8} {:<8} {}{}",
                task::short(&r.id),
                r.relation,
                r.launch,
                if r.launched { r.status.unwrap_or("-") } else { "never launched" },
                r.cwd.as_deref().unwrap_or(""),
                if r.held_out { "  (held out of memory)" } else { "" }
            );
            if let Some(why) = describe_why(&r.why, now_ms()) {
                match moved.iter().find(|(run, _)| *run == r.id) {
                    Some((_, to)) => println!("          {why}; {to}"),
                    None => println!("          {why}"),
                }
            }
        }
    }
    Ok(())
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
    let mut parts = vec![format!("{} at {}", why.event.as_deref()?, clock(why.at?, now))];
    if let Some(t) = why.idle_at {
        parts.push(format!("idle at {}", clock(t, now)));
    }
    if let Some(t) = why.seen_at {
        parts.push(format!("seen at {}", clock(t, now)));
    }
    match why.alive {
        Some(true) => parts.push("process running".into()),
        Some(false) => parts.push("process gone".into()),
        None => {}
    }
    Some(parts.join(", "))
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
        Some(w) if w.state == "active" => git::facts(std::path::Path::new(&w.path), Some(&w.base), true).ok(),
        Some(_) => None,
        None => git::facts(std::path::Path::new(d.project.as_deref()?), None, true).ok(),
    }
}

/// `committed: 2 ahead, 5 behind main, 3 uncommitted, not pushed`.
fn describe_git(f: &git::Facts) -> String {
    let mut parts = vec![];
    if f.shared {
        parts.push(format!("{} (shared checkout)", f.branch));
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
    format!("{}: {}", f.stage(), parts.join(", "))
}
