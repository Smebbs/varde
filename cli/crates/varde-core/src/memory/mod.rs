//! Memory in the store: cairns and project cairns, their bullets and
//! history, notes pinned for the next update, whether anchors still
//! resolve, and what each run was shown. The jobs that update memory are
//! in `job`, and what searches it in `recall`.

use std::collections::HashMap;

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::cairn::{Cairn, Item, Section, Shape};
use crate::store::Store;

mod change;
mod job;
mod recall;

pub use change::*;
pub use job::*;
pub use recall::*;

const ITEM_ORDER: &str = "CASE section WHEN 'findings' THEN 0 WHEN 'decisions' THEN 1 WHEN 'open' THEN 2
                                       WHEN 'conventions' THEN 3 WHEN 'gotchas' THEN 4 ELSE 5 END, pos";

/// Whose memory a document is: a task's cairn, or a project's cairn.
/// Both are bullets in `cairn_item`, told apart by which of `task` and
/// `project` they carry, with a version row and a history of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner<'a> {
    Task(&'a str),
    Project(&'a str),
}

impl Owner<'_> {
    fn key(&self) -> &str {
        match self {
            Owner::Task(k) | Owner::Project(k) => k,
        }
    }

    /// The column of `cairn_item`, and of the version and history tables,
    /// that names it.
    fn column(&self) -> &'static str {
        match self {
            Owner::Task(_) => "task",
            Owner::Project(_) => "project",
        }
    }

    fn versions(&self) -> &'static str {
        match self {
            Owner::Task(_) => "cairn",
            Owner::Project(_) => "project_cairn",
        }
    }

    /// Whether the user wrote the Status; a project's cairn has none.
    fn status_mine(&self) -> &'static str {
        match self {
            Owner::Task(_) => "status_mine",
            Owner::Project(_) => "0",
        }
    }

    fn history(&self) -> &'static str {
        match self {
            Owner::Task(_) => "cairn_history",
            Owner::Project(_) => "project_cairn_history",
        }
    }

    pub fn shape(&self) -> Shape {
        match self {
            Owner::Task(_) => Shape::Task,
            Owner::Project(_) => Shape::Project,
        }
    }
}

/// A task's cairn as it is shown now, and its version (0 for none).
pub fn cairn(conn: &Connection, task: &str) -> Result<Option<(Cairn, i64)>> {
    document(conn, Owner::Task(task))
}

/// A project's cairn as it is shown now, and its version.
pub fn project_cairn(conn: &Connection, project: &str) -> Result<Option<(Cairn, i64)>> {
    document(conn, Owner::Project(project))
}

/// A task's or a project's cairn as it is shown now, and its version.
pub fn document(conn: &Connection, owner: Owner) -> Result<Option<(Cairn, i64)>> {
    let (col, key) = (owner.column(), owner.key());
    let row: Option<(String, i64, bool)> = conn
        .prepare_cached(&format!(
            "SELECT status, version, {} FROM {} WHERE {col} = ?1",
            owner.status_mine(),
            owner.versions()
        ))?
        .query_row([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((status, version, status_mine)) = row else { return Ok(None) };
    let mut c = Cairn { status, status_mine, shape: owner.shape(), ..Default::default() };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT id, section, text, anchor, mine, coalesce(stale, 0), confirmed FROM cairn_item
         WHERE {col} = ?1 AND state = 'current' ORDER BY {ITEM_ORDER}"
    ))?;
    let rows = stmt.query_map([key], |r| {
        Ok((
            r.get::<_, String>(1)?,
            Item {
                id: r.get(0)?,
                text: r.get(2)?,
                anchor: r.get(3)?,
                mine: r.get(4)?,
                stale: r.get(5)?,
                confirmed: r.get(6)?,
            },
        ))
    })?;
    for row in rows {
        let (section, item) = row?;
        if let Some(s) = Section::parse(&section) {
            c.section_mut(s).push(item);
        }
    }
    Ok(Some((c, version)))
}

/// How a new version of a cairn is written.
pub struct Write<'a> {
    /// `job` or `user`.
    pub author: &'a str,
    /// The run whose update wrote it.
    pub run: Option<&'a str>,
    /// What changed, for the version's history.
    pub reason: Option<&'a str>,
    /// Why each bullet that leaves left; one not named here left for
    /// `default_gone`.
    pub gone: &'a [(i64, String)],
    pub default_gone: &'a str,
    /// New bullets that went straight to history (`cap`).
    pub capped: &'a [(Section, Item)],
    pub now: i64,
}

/// Write `new` as `owner`'s document, if it is still at `expected` (0
/// for none yet). Returns false, writing nothing, when someone else
/// updated it first. A bullet of the current document that `new` leaves
/// out moves to history with its reason; one whose text changed keeps its
/// id, and its old words go to history as `edited`. A bullet with id 0 is
/// new. All of it is written, or none of it.
pub fn save_doc(conn: &Connection, owner: Owner, expected: i64, new: &Cairn, w: &Write) -> Result<bool> {
    // A savepoint: inside a caller's transaction, or one of its own.
    conn.execute_batch("SAVEPOINT save_doc")?;
    match write_doc(conn, owner, expected, new, w) {
        Ok(saved) => {
            conn.execute_batch("RELEASE save_doc")?;
            Ok(saved)
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK TO save_doc; RELEASE save_doc")?;
            Err(e)
        }
    }
}

fn write_doc(conn: &Connection, owner: Owner, expected: i64, new: &Cairn, w: &Write) -> Result<bool> {
    let (col, key, versions) = (owner.column(), owner.key(), owner.versions());
    let changed = if expected == 0 {
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO {versions} ({col}, status, version, updated) VALUES (?1, ?2, 1, ?3)"
            ),
            params![key, new.status, w.now],
        )?
    } else {
        conn.execute(
            &format!(
                "UPDATE {versions} SET status = ?2, version = version + 1, updated = ?3
                 WHERE {col} = ?1 AND version = ?4"
            ),
            params![key, new.status, w.now, expected],
        )?
    };
    if changed == 0 {
        return Ok(false);
    }
    if let Owner::Task(task) = owner {
        conn.execute("UPDATE cairn SET status_mine = ?2 WHERE task = ?1", params![task, new.status_mine])?;
    }
    let before: HashMap<i64, String> = conn
        .prepare(&format!("SELECT id, text FROM cairn_item WHERE {col} = ?1 AND state = 'current'"))?
        .query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let staying: Vec<i64> = new.items().map(|(_, i)| i.id).filter(|id| before.contains_key(id)).collect();
    for id in before.keys().filter(|id| !staying.contains(id)) {
        let reason = w.gone.iter().find(|(g, _)| g == id).map_or(w.default_gone, |(_, r)| r.as_str());
        conn.execute(
            "UPDATE cairn_item SET state = 'history', reason = ?2, left_at = ?3 WHERE id = ?1",
            params![id, reason, w.now],
        )?;
    }
    let insert = |s: Section, pos: usize, i: &Item, state: &str, reason: Option<&str>| {
        conn.execute(
            &format!(
                "INSERT INTO cairn_item ({col}, section, pos, text, anchor, mine, author, run, created, confirmed,
                                         state, reason, left_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, CASE WHEN ?11 = 'history' THEN ?9 END)"
            ),
            params![
                key,
                s.name(),
                pos as i64,
                i.text,
                i.anchor,
                i.mine,
                w.author,
                w.run,
                w.now,
                if i.confirmed > 0 { i.confirmed } else { w.now },
                state,
                reason
            ],
        )
    };
    for &s in owner.shape().sections() {
        for (pos, i) in new.section(s).iter().enumerate() {
            match before.get(&i.id) {
                None => {
                    // A bullet brought back from its history keeps its id.
                    let revived = i.id != 0
                        && conn.execute(
                            &format!(
                                "UPDATE cairn_item SET state = 'current', reason = NULL, left_at = NULL, section = ?3,
                                                       pos = ?4
                                 WHERE id = ?1 AND {col} = ?2 AND state = 'history'"
                            ),
                            params![i.id, key, s.name(), pos as i64],
                        )? > 0;
                    if !revived {
                        insert(s, pos, i, "current", None)?;
                    }
                }
                Some(was) => {
                    if *was != i.text {
                        conn.execute(
                            "INSERT INTO cairn_item (task, project, section, pos, text, anchor, mine, author, run,
                                                     created, confirmed, state, reason, left_at, of)
                             SELECT task, project, section, pos, text, anchor, mine, author, run, created,
                                    confirmed, 'history', 'edited', ?2, id
                             FROM cairn_item WHERE id = ?1",
                            params![i.id, w.now],
                        )?;
                        conn.execute(
                            "UPDATE cairn_item SET text = ?2, anchor = ?3, stale = NULL, checked = NULL,
                                                   run = coalesce(?4, run)
                             WHERE id = ?1",
                            params![i.id, i.text, i.anchor, w.run],
                        )?;
                    }
                    conn.execute(
                        "UPDATE cairn_item SET section = ?2, pos = ?3, mine = ?4, confirmed = max(confirmed, ?5)
                         WHERE id = ?1",
                        params![i.id, s.name(), pos as i64, i.mine, i.confirmed],
                    )?;
                }
            }
        }
    }
    for (s, i) in w.capped {
        insert(*s, 0, i, "history", Some("cap"))?;
    }
    let snapshot = document(conn, owner)?.map(|(c, _)| c).unwrap_or_default();
    conn.execute(
        &format!(
            "INSERT INTO {} ({col}, doc, author, reason, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            owner.history()
        ),
        params![key, serde_json::to_string(&snapshot)?, w.author, w.reason, w.now],
    )?;
    match owner {
        Owner::Task(task) => reindex(conn, task)?,
        Owner::Project(project) => reindex_project(conn, project)?,
    }
    Ok(true)
}

/// Replace a task's cairn with `cairn`: what it leaves out goes to
/// history as replaced. For callers that hold a whole cairn, such as a
/// person's edit.
pub fn put_cairn(
    conn: &Connection,
    task: &str,
    cairn: &Cairn,
    expected: i64,
    author: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<bool> {
    let w =
        Write { author, run: None, reason, gone: &[], default_gone: "removed by an edit", capped: &[], now };
    save_doc(conn, Owner::Task(task), expected, cairn, &w)
}

/// Bullets said still to hold, without a new version.
pub fn confirm(conn: &Connection, ids: &[i64], now: i64) -> Result<()> {
    for id in ids {
        conn.execute(
            "UPDATE cairn_item SET confirmed = max(confirmed, ?2) WHERE id = ?1 AND state = 'current'",
            params![id, now],
        )?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Version {
    pub ts: i64,
    pub author: String,
    pub reason: Option<String>,
    pub cairn: Cairn,
}

pub fn history(conn: &Connection, task: &str) -> Result<Vec<Version>> {
    history_of(conn, Owner::Task(task))
}

pub fn history_of(conn: &Connection, owner: Owner) -> Result<Vec<Version>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT ts, author, reason, doc FROM {} WHERE {} = ?1 ORDER BY ts, id",
        owner.history(),
        owner.column()
    ))?;
    let rows =
        stmt.query_map([owner.key()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, author, reason, doc) = row?;
        out.push(Version { ts, author, reason, cairn: serde_json::from_str(&doc)? });
    }
    Ok(out)
}

/// How a cairn is used: who last changed it and when, how many sessions
/// were handed it, and what waits for its next update.
#[derive(Debug, Default, Serialize)]
pub struct Usage {
    /// When it last changed, and by whom: `user`, or `job` for an update.
    pub updated: Option<i64>,
    pub author: Option<String>,
    /// The run whose update last changed it; for a project's cairn, the
    /// run whose task's promotion did.
    pub run: Option<String>,
    /// For a project's cairn, the task that promotion was of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Sessions handed it whole (`shown` keeps 30 days), and the last time.
    pub sessions: i64,
    pub handed: Option<i64>,
    /// Notes waiting for the next update; a task's only.
    pub notes: usize,
    /// Bullets whose code is gone.
    pub stale: usize,
}

/// How `owner`'s document is used.
pub fn usage(conn: &Connection, owner: Owner) -> Result<Usage> {
    let mut u = Usage::default();
    let last: Option<(i64, String)> = conn
        .query_row(
            &format!(
                "SELECT ts, author FROM {} WHERE {} = ?1 ORDER BY ts DESC, id DESC LIMIT 1",
                owner.history(),
                owner.column()
            ),
            [owner.key()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((ts, author)) = last {
        u.updated = Some(ts);
        u.author = Some(author);
    }
    let (handed, by) = match owner {
        Owner::Task(_) => (
            "FROM run r JOIN shown s ON s.run = r.id WHERE r.task = ?1 AND s.kind = 'cairn' AND s.task = ?1",
            "SELECT j.run, r.task FROM job j JOIN run r ON r.id = j.run
             WHERE r.task = ?1 AND j.kind IN ('fold', 'close') AND j.state = 'done'
             ORDER BY j.finished DESC LIMIT 1",
        ),
        Owner::Project(_) => (
            "FROM task t JOIN run r ON r.task = t.id JOIN shown s ON s.run = r.id
             WHERE t.project = ?1 AND s.kind = 'project'",
            "SELECT j.run, r.task FROM job j JOIN run r ON r.id = j.run JOIN task t ON t.id = r.task
             WHERE t.project = ?1 AND j.kind = 'promote' AND j.state = 'done'
             ORDER BY j.finished DESC LIMIT 1",
        ),
    };
    (u.sessions, u.handed) =
        conn.query_row(&format!("SELECT count(DISTINCT s.run), max(s.ts) {handed}"), [owner.key()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if u.author.as_deref() == Some("job") {
        let found: Option<(String, Option<String>)> =
            conn.query_row(by, [owner.key()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some((run, task)) = found {
            u.run = Some(run);
            u.task = task.filter(|_| matches!(owner, Owner::Project(_)));
        }
    }
    if let Owner::Task(task) = owner {
        u.notes = pending_notes(conn, task)?.len();
    }
    u.stale = document(conn, owner)?.map_or(0, |(c, _)| c.items().filter(|(_, i)| i.stale).count());
    Ok(u)
}

/// A bullet that has left a cairn, and why.
#[derive(Debug, Serialize)]
pub struct Former {
    pub id: i64,
    pub section: String,
    pub text: String,
    pub reason: Option<String>,
    pub left: Option<i64>,
}

/// What has left `task`'s cairn, newest first.
pub fn former(conn: &Connection, task: &str) -> Result<Vec<Former>> {
    let mut stmt = conn.prepare(
        "SELECT id, section, text, reason, left_at FROM cairn_item WHERE task = ?1 AND state = 'history'
         ORDER BY left_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([task], |r| {
        Ok(Former { id: r.get(0)?, section: r.get(1)?, text: r.get(2)?, reason: r.get(3)?, left: r.get(4)? })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Pin a note on `run`, with anything that reads like a secret replaced
/// by `[redacted]` before it is stored: a note is indexed and may be shown
/// to later sessions. Returns its id, and how many lines were redacted.
pub fn add_note(store: &Store, run: &str, text: &str, now: i64) -> Result<(String, usize)> {
    let (text, redacted) = crate::cairn::redact(&crate::cairn::clean(text.trim()));
    let text = text.trim();
    if text.is_empty() {
        bail!("a note needs text");
    }
    let id = ulid::Ulid::new().to_string();
    store.conn.execute(
        "INSERT INTO note (id, run, text, ts) VALUES (?1, ?2, ?3, ?4)",
        params![id, run, text, now],
    )?;
    // Findable at once, not only after the next update.
    let task: Option<String> =
        store.conn.query_row("SELECT task FROM run WHERE id = ?1", [run], |r| r.get(0)).optional()?.flatten();
    if let Some(task) = task {
        reindex(&store.conn, &task)?;
    }
    Ok((id, redacted))
}

/// Notes on any of `task`'s runs not yet folded into its cairn.
pub fn pending_notes(conn: &Connection, task: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, n.text FROM note n JOIN run r ON r.id = n.run
         WHERE r.task = ?1 AND n.folded = 0 ORDER BY n.ts",
    )?;
    let notes = stmt.query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    Ok(notes)
}

/// A bullet whose anchor is to be checked, and the checkout it is checked
/// in: its task's active worktree, else its project; for a bullet of a
/// project's cairn, the project.
pub struct Anchored {
    pub id: i64,
    pub anchor: String,
    pub dir: String,
}

/// A current anchored bullet with the checkout it is checked in; one with
/// no checkout to check in is left out.
const ANCHORED: &str = "SELECT i.id, i.anchor,
        coalesce((SELECT w.path FROM worktree w WHERE w.task = i.task AND w.state = 'active'
                  ORDER BY w.created DESC LIMIT 1), t.project, i.project) AS dir
 FROM cairn_item i LEFT JOIN task t ON t.id = i.task
 WHERE i.state = 'current' AND i.anchor IS NOT NULL";

fn anchored_rows(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<Anchored>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(args, |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        if let (id, anchor, Some(dir)) = row? {
            out.push(Anchored { id, anchor, dir });
        }
    }
    Ok(out)
}

/// The anchored bullets of one cairn, or of one project's cairn: what an
/// update just wrote, to be checked at once.
pub fn anchored(conn: &Connection, owner: Owner) -> Result<Vec<Anchored>> {
    anchored_rows(conn, &format!("{ANCHORED} AND i.{} = ?1", owner.column()), [owner.key()])
}

/// At most `limit` anchored bullets not checked since `before`, the least
/// recently checked first (never checked before all): what one
/// `git-refresh` checks, so its cost does not grow with the store.
pub fn anchored_due(conn: &Connection, before: i64, limit: usize) -> Result<Vec<Anchored>> {
    anchored_rows(
        conn,
        &format!(
            "{ANCHORED} AND coalesce(i.checked, 0) < ?1 AND dir IS NOT NULL
             ORDER BY coalesce(i.checked, 0), i.id LIMIT ?2"
        ),
        params![before, limit as i64],
    )
}

/// Record whether each bullet's anchor resolved, and when it was checked.
pub fn set_stale(conn: &Connection, results: &[(i64, bool)], now: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (id, stale) in results {
        tx.execute("UPDATE cairn_item SET stale = ?2, checked = ?3 WHERE id = ?1", params![id, stale, now])?;
    }
    tx.commit()?;
    Ok(())
}

/// How many bullets now shown are marked stale, and in how many tasks.
pub fn stale_count(conn: &Connection) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        "SELECT count(*), count(DISTINCT task) FROM cairn_item WHERE state = 'current' AND stale = 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Memory a run was shown: a task's cairn (`cairn`, or `origin` for the
/// task it was deferred from), `related` hits, or `project` memory; or
/// `holdout` when it was held out of memory, and shown none.
#[derive(Debug, Clone, Serialize)]
pub struct Shown {
    pub kind: &'static str,
    pub task: Option<String>,
    /// The cairn's version, for a cairn.
    pub version: Option<i64>,
    /// The bullets shown, by id, or the notes'.
    pub items: Vec<String>,
}

/// Record what `run` was shown, and how (`preamble` or `hook`).
pub fn record_shown(conn: &Connection, run: &str, via: &str, shown: &[Shown], now: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO shown (run, ts, via, kind, task, version, items) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for s in shown {
        stmt.execute(params![run, now, via, s.kind, s.task, s.version, serde_json::to_string(&s.items)?])?;
    }
    Ok(())
}

/// What a conversation has been shown, and whether it was held out of
/// memory.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Saw {
    /// The newest version of the task's cairn it was shown whole.
    pub cairn: Option<i64>,
    /// The newest version of its project's cairn it was shown whole.
    pub project: Option<i64>,
    pub held: bool,
}

/// What the conversation `run` belongs to has been shown of `task`'s
/// cairn and its project's cairn: by this run, or by the runs it carries
/// on, through resumes and forks back to its fresh start, or its last 32,
/// which keeps a hook's cost flat however often a conversation goes on.
pub fn conversation_saw(conn: &Connection, run: &str, task: &str) -> Result<Saw> {
    // Walked a run at a time: two indexed lookups each, where one
    // recursive query cost a hook half a millisecond to plan.
    let mut step =
        conn.prepare_cached("SELECT parent_run, relation, coalesce(holdout, 0) FROM run WHERE id = ?1")?;
    let mut saw = conn.prepare_cached(
        "SELECT max(CASE WHEN kind = 'cairn' AND task = ?2 THEN version END),
                max(CASE WHEN kind = 'project' THEN version END)
         FROM shown WHERE run = ?1 AND kind IN ('cairn', 'project')",
    )?;
    let mut out = Saw::default();
    let mut at = run.to_string();
    for _ in 0..32 {
        let Some((parent, relation, holdout)) = step
            .query_row([&at], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))
            })
            .optional()?
        else {
            break;
        };
        out.held |= holdout;
        let (c, p): (Option<i64>, Option<i64>) =
            saw.query_row(params![at, task], |r| Ok((r.get(0)?, r.get(1)?)))?;
        out.cairn = out.cairn.max(c);
        out.project = out.project.max(p);
        match parent {
            Some(p) if matches!(relation.as_str(), "resume" | "fork") => at = p,
            _ => break,
        }
    }
    Ok(out)
}

/// Whether a fold of `run` is queued or under way: what a session
/// compacted just now may not have been given yet.
pub fn fold_pending(conn: &Connection, run: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM job WHERE run = ?1 AND kind = 'fold' AND state IN ('queued', 'running'))",
        )?
        .query_row([run], |r| r.get(0))?)
}

/// The text of every bullet and note the conversation `run` belongs to
/// was shown, through resumes and forks as `conversation_saw` walks them:
/// what an update can tell it re-discovered.
pub fn conversation_shown(conn: &Connection, run: &str) -> Result<Vec<String>> {
    let mut step = conn.prepare_cached("SELECT parent_run, relation FROM run WHERE id = ?1")?;
    let mut items = conn.prepare_cached(
        "SELECT items FROM shown WHERE run = ?1 AND kind IN ('cairn', 'project', 'related', 'origin')",
    )?;
    let mut ids: Vec<String> = Vec::new();
    let mut at = run.to_string();
    for _ in 0..32 {
        let Some((parent, relation)) = step
            .query_row([&at], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)))
            .optional()?
        else {
            break;
        };
        for list in items.query_map([&at], |r| r.get::<_, String>(0))? {
            ids.extend(serde_json::from_str::<Vec<String>>(&list?).unwrap_or_default());
        }
        match parent {
            Some(p) if matches!(relation.as_str(), "resume" | "fork") => at = p,
            _ => break,
        }
    }
    ids.sort();
    ids.dedup();
    let mut bullet = conn.prepare_cached("SELECT text FROM cairn_item WHERE id = ?1")?;
    let mut note = conn.prepare_cached("SELECT text FROM note WHERE id = ?1")?;
    let mut out = Vec::new();
    for id in ids {
        let text: Option<String> = match id.parse::<i64>() {
            Ok(n) => bullet.query_row([n], |r| r.get(0)).optional()?,
            Err(_) => note.query_row([&id], |r| r.get(0)).optional()?,
        };
        out.extend(text);
    }
    Ok(out)
}

/// Whether a fresh session is held out of memory, for `share` (0 to 1)
/// of them: decided by its run id, so the same run always gets the same
/// answer, and no hook needs the config to know it.
pub fn held_out(run: &str, share: f64) -> bool {
    // FNV-1a: stable across builds and platforms, unlike the std hasher.
    let hash =
        run.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    ((hash % 10_000) as f64) < share.clamp(0.0, 1.0) * 10_000.0
}

#[cfg(test)]
mod tests;
