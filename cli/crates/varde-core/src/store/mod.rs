//! The local store: one SQLite database per machine, the source of truth
//! for everything varde knows.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::event::{Kind, Norm, Source};
use crate::status::{self, Status};
use crate::{project, title};

mod migrations;

use migrations::MIGRATIONS;

/// The store was migrated by a newer varde: this one is out of date.
#[derive(Debug)]
pub struct NewerSchema {
    pub have: usize,
    pub known: usize,
}

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "database schema {} is newer than this varde understands ({}); upgrade varde",
            self.have, self.known
        )
    }
}

impl std::error::Error for NewerSchema {}

/// A run with no task after this long never got a prompt: a pre-warmed
/// worker the agent CLI spawned and killed unused.
const SPARE_TTL_MS: i64 = 24 * 3600 * 1000;

/// The run a claim names, as `ingest` sees it: its id, task, and process
/// (pid and start time) if known.
type Claimed = (String, Option<String>, Option<(i64, i64)>);

pub struct Store {
    pub conn: Connection,
}

impl Store {
    pub fn open(path: &Path, busy: Duration) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(busy)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Every command is a new process: reading pages from the mapping
        // beats a system call per page.
        conn.pragma_update(None, "mmap_size", 256i64 << 20)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<usize> {
        let v: i64 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(v as usize)
    }

    fn migrate(&mut self) -> Result<()> {
        let have = self.schema_version()?;
        if have > MIGRATIONS.len() {
            return Err(NewerSchema { have, known: MIGRATIONS.len() }.into());
        }
        if have == MIGRATIONS.len() {
            return Ok(());
        }
        // A migration may rebuild a table others refer to, which SQLite
        // allows only with foreign keys off; that cannot change inside a
        // transaction. They are checked before the migration commits.
        self.conn.pragma_update(None, "foreign_keys", false)?;
        let result = (|| -> Result<()> {
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Re-read under the write lock: another process may have migrated.
            let have: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            for sql in &MIGRATIONS[have as usize..] {
                tx.execute_batch(sql)?;
            }
            if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
                bail!("migrating the store would break a foreign key; nothing was changed");
            }
            tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
            tx.commit()?;
            Ok(())
        })();
        self.conn.pragma_update(None, "foreign_keys", true)?;
        result
    }

    /// Record one hook event. `now` is unix milliseconds; `raw` is the
    /// payload as it should be kept. `claim` is the run a session varde
    /// launched was told it belongs to (`VARDE_RUN`); it only counts for
    /// that session's own id, so an agent started from inside it — a
    /// `claude -p` run by a tool, say — is not mistaken for it, and never
    /// becomes a task. The one exception is the session itself moving to
    /// another conversation (`/clear`, `/resume`, or pi's `/new` or
    /// `/fork`): that is a new run, in the same process and terminal.
    pub fn ingest(
        &mut self,
        agent: &str,
        norm: &Norm,
        raw: &str,
        now: i64,
        claim: Option<&str>,
    ) -> Result<Option<String>> {
        self.ingest_from(agent, norm, raw, now, claim, &|| None)
    }

    /// `ingest`, told how to find the process the hook came from (pid and
    /// start time), which tells the claimed session itself from one started
    /// inside it, and is recorded for a session varde did not launch.
    pub fn ingest_from(
        &mut self,
        agent: &str,
        norm: &Norm,
        raw: &str,
        now: i64,
        claim: Option<&str>,
        process: &dyn Fn() -> Option<(i64, i64)>,
    ) -> Result<Option<String>> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // A run launched for an agent that picks its own id has none
        // (empty) until the first event that claims it gives it one.
        let claimed: Option<(String, Option<String>)> = match claim {
            Some(id) => tx
                .query_row(
                    "SELECT id, task FROM run WHERE id=?1 AND agent=?2 AND native_id IN (?3, '')",
                    params![id, agent, norm.native_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?,
            None => None,
        };
        if let Some((run, _)) = &claimed {
            tx.execute(
                "UPDATE run SET native_id=?2 WHERE id=?1 AND native_id=''",
                params![run, norm.native_id],
            )?;
        }

        let latest: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT id, task FROM run WHERE agent=?1 AND native_id=?2
                 ORDER BY started DESC, rowid DESC LIMIT 1",
                params![agent, norm.native_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        // A session varde launched that moves to another conversation
        // keeps its claim, under a new id. Any other session started under
        // a claim was started from inside the claimed one: told apart by
        // its process, when both are known.
        let other: Option<Claimed> = match (claim, &claimed) {
            (Some(id), None) => tx
                .query_row(
                    "SELECT id, task, CASE WHEN pid > 0 THEN pid END, coalesce(pid_start, 0) FROM run
                     WHERE id = ?1 AND agent = ?2",
                    params![id, agent],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<i64>>(2)?.zip(Some(r.get(3)?)))),
                )
                .optional()?,
            _ => None,
        };
        let cell = std::cell::OnceCell::new();
        let same = || {
            *cell.get_or_init(|| match (&other, process()) {
                (Some((_, _, Some(theirs))), Some(me)) => Some(*theirs == me),
                _ => None,
            })
        };
        // The new run's task, and the run it carries on from: a resumed
        // conversation varde knows goes on in its own task.
        let successor: Option<(String, Option<String>)> = match (&other, &latest, &norm.kind) {
            (Some((id, task, ..)), None, Kind::Started(Source::Clear | Source::Fork))
                if same() != Some(false) =>
            {
                Some((id.clone(), task.clone()))
            }
            (Some((id, task, ..)), _, Kind::Started(Source::Resume)) if same() == Some(true) => match &latest
            {
                Some((prev, theirs)) => Some((prev.clone(), theirs.clone().or(task.clone()))),
                None => Some((id.clone(), task.clone())),
            },
            _ => None,
        };
        let unclaimed = claim.is_some() && claimed.is_none();
        let refused = || unclaimed && same() != Some(true);

        let new_run = |task: Option<&str>, parent: Option<&str>, relation: &str| -> Result<String> {
            let id = ulid::Ulid::new().to_string();
            tx.execute(
                "INSERT INTO run (id, task, agent, native_id, parent_run, relation, cwd, transcript, started)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, task, agent, norm.native_id, parent, relation, norm.cwd, norm.transcript, now],
            )?;
            Ok(id)
        };

        let (run, task) = if let Some((run, task)) = claimed {
            if norm.kind == Kind::ToolRan && !tool_run_counts(&tx, &run, now)? {
                return Ok(None);
            }
            (run, task)
        } else if let (Some((prev, task)), Some((claimed_run, claimed_task, _))) = (successor, &other) {
            let relation = match norm.kind {
                Kind::Started(Source::Fork) => "fork",
                Kind::Started(Source::Resume) => "resume",
                _ => "fresh",
            };
            let id = new_run(task.as_deref(), Some(&prev), relation)?;
            // Another task's conversation resumed here picks that task up
            // again, as starting it would: a live session is never hidden
            // under Done or Parked.
            if let Some(t) = task.as_deref().filter(|t| Some(*t) != claimed_task.as_deref()) {
                reopen(&tx, t)?;
            }
            // The same process, in the same terminal.
            tx.execute(
                "UPDATE run SET (launch, pid, pid_start, tty, tty_ctime) =
                   (SELECT launch, pid, pid_start, tty, tty_ctime FROM run WHERE id = ?2)
                 WHERE id = ?1",
                params![id, claimed_run],
            )?;
            (id, task)
        } else {
            match (&norm.kind, latest) {
                // The same conversation carried on: a new run of the same
                // task. It shares the transcript, and with it the fold
                // cursor (`memory::enqueue`).
                // A closed task picked up again this way is open again.
                (Kind::Started(Source::Resume), Some((prev, task))) => {
                    if let Some(t) = &task {
                        reopen(&tx, t)?;
                    }
                    (new_run(task.as_deref(), Some(&prev), "resume")?, task)
                }
                // Compaction continues the run it happens in.
                (Kind::Started(Source::Compact), Some(run)) => run,
                // A fork is a new conversation. Its parent is found later, off
                // the hook path, by matching copied transcript messages.
                (Kind::Started(Source::Fork), _) => (new_run(None, None, "fork")?, None),
                (Kind::Started(_), Some((prev, task))) if !has_events(&tx, &prev)? => (prev, task),
                (Kind::Started(_), _) => (new_run(None, None, "fresh")?, None),
                (Kind::ToolRan, Some((prev, task))) => {
                    if !tool_run_counts(&tx, &prev, now)? {
                        return Ok(None);
                    }
                    (prev, task)
                }
                (Kind::ToolRan | Kind::Answered, None) => return Ok(None),
                (_, Some(run)) => run,
                // varde arrived mid-session: start tracking from here.
                (_, None) => (new_run(None, None, "fresh")?, None),
            }
        };

        // A fork's transcript starts as a copy of its parent's history,
        // which the parent's own folds cover: only what follows is new.
        if let (Kind::Started(Source::Fork), Some(t)) = (&norm.kind, &norm.transcript) {
            if let Ok(m) = std::fs::metadata(t) {
                tx.execute(
                    "UPDATE run SET cairn_cursor = ?2 WHERE id = ?1 AND cairn_cursor = 0",
                    params![run, m.len() as i64],
                )?;
            }
        }

        // A run varde launched learns where its transcript is from its
        // first event.
        if let Some(t) = &norm.transcript {
            tx.execute("UPDATE run SET transcript=?2 WHERE id=?1 AND transcript IS NULL", params![run, t])?;
        }

        let (agents, shells) = match norm.kind {
            Kind::TurnEnd { agents, shells } => (Some(agents), Some(shells)),
            _ => (None, None),
        };
        let kind = match norm.kind {
            Kind::Answered => match answered(&tx, &run, now)? {
                Some(kind) => kind,
                None => return Ok(None),
            },
            _ => norm.kind.name(),
        };
        tx.execute(
            "INSERT INTO event (run, kind, ts, raw, agents, shells) VALUES (?1,?2,?3,?4,?5,?6)",
            params![run, kind, now, raw, agents, shells],
        )?;

        // The process of a session varde did not launch: once it is gone
        // the session is over. Recorded here rather than by the hook, so
        // an event that waited in the pending file records it too.
        if matches!(norm.kind, Kind::Started(_) | Kind::Prompt) {
            if let Some((pid, start)) = process() {
                tx.execute(
                    "UPDATE run SET pid = ?2, pid_start = ?3 WHERE id = ?1 AND launch = 'adopted' AND pid IS NULL",
                    params![run, pid, start],
                )?;
            }
        }

        // Being asked something is what separates a session from a warm
        // process, so a task is born on the first prompt, never before.
        if task.is_none() && norm.kind == Kind::Prompt && !refused() {
            if let Some(t) = norm.prompt.as_deref().and_then(title::from_prompt) {
                let id = ulid::Ulid::new().to_string();
                let project = norm.cwd.as_deref().map(|c| project::of(&tx, Path::new(c)));
                tx.execute(
                    "INSERT INTO task (id, title, prompt_title, project, inbox, created) VALUES (?1,?2,?2,?3,1,?4)",
                    params![id, t, project.map(|p| p.display().to_string()), now],
                )?;
                tx.execute("UPDATE run SET task=?1 WHERE id=?2", params![id, run])?;
            }
        }

        if matches!(norm.kind, Kind::Started(_)) {
            tx.execute(
                "DELETE FROM run WHERE task IS NULL AND started < ?1
                 AND id NOT IN (SELECT parent_run FROM run WHERE parent_run IS NOT NULL)
                 AND id NOT IN (SELECT run FROM note)
                 AND id NOT IN (SELECT run FROM job)",
                params![now - SPARE_TTL_MS],
            )?;
        }
        tx.commit()?;
        Ok(Some(run))
    }
}

impl Store {
    /// Put a fork varde did not start under the task of the conversation
    /// it was forked from. `key` gives the fork's family key from its
    /// transcript; `in_family` checks another transcript against it. Does
    /// nothing unless the latest run for `native_id` is a fork with no
    /// parent and no task yet.
    pub fn link_fork(
        &mut self,
        agent: &str,
        native_id: &str,
        key: &dyn Fn(&Path) -> Option<String>,
        in_family: &dyn Fn(&Path, &str) -> bool,
    ) -> Result<()> {
        let fork: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, transcript FROM run WHERE agent=?1 AND native_id=?2
                   AND relation='fork' AND parent_run IS NULL AND task IS NULL
                 ORDER BY started DESC, rowid DESC LIMIT 1",
                params![agent, native_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((run, Some(transcript))) = fork else { return Ok(()) };
        let transcript = Path::new(&transcript);
        let Some(k) = key(transcript) else { return Ok(()) };
        // A fork's transcript sits beside its parent's: the agent files
        // them by the directory the session ran in.
        let dir = transcript.parent().map(|d| d.display().to_string()).unwrap_or_default();
        let mut stmt = self.conn.prepare(
            "SELECT id, task, transcript FROM run
             WHERE agent=?1 AND id != ?2 AND task IS NOT NULL
               AND substr(transcript, 1, length(?3) + 1) = ?3 || '/'
             ORDER BY started DESC, rowid DESC LIMIT 200",
        )?;
        // The newest runs in the directory, oldest first among them.
        let mut candidates: Vec<(String, String, String)> = stmt
            .query_map(params![agent, run, dir], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        candidates.reverse();
        drop(stmt);
        // The earliest member of the family is where its task began.
        if let Some((parent, task, _)) = candidates.into_iter().find(|(_, _, t)| in_family(Path::new(t), &k))
        {
            self.conn.execute(
                "UPDATE run SET parent_run=?2, task=?3 WHERE id=?1 AND task IS NULL",
                params![run, parent, task],
            )?;
        }
        Ok(())
    }

    /// A run `varde run` supervises has ended, cleanly or not, and with it
    /// every run its process moved on to (`/clear`, `/resume`) that has
    /// not ended yet. Returns them all.
    pub fn record_exit(&mut self, run: &str, code: i32, now: i64) -> Result<Vec<String>> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let runs: Vec<String> = tx
            .prepare(
                "SELECT id FROM run WHERE id = ?1
                 OR (exit_code IS NULL AND pid > 0
                     AND (pid, coalesce(pid_start, 0)) = (SELECT pid, coalesce(pid_start, 0) FROM run WHERE id = ?1))",
            )?
            .query_map([run], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let raw = serde_json::json!({"source": "varde run", "exit_code": code}).to_string();
        for r in &runs {
            tx.execute("UPDATE run SET exit_code = ?2 WHERE id = ?1", params![r, code])?;
            tx.execute(
                "INSERT INTO event (run, kind, ts, raw) VALUES (?1, 'ended', ?2, ?3)",
                params![r, now, raw],
            )?;
        }
        tx.commit()?;
        Ok(runs)
    }

    /// The agent sends no event when the user interrupts a turn; `varde
    /// run` records one when the transcript shows it.
    pub fn record_interrupt(&self, run: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO event (run, kind, ts, raw) VALUES (?1, 'interrupted', ?2, ?3)",
            params![run, now, serde_json::json!({"source": "varde run"}).to_string()],
        )?;
        Ok(())
    }
}

/// Is a run live? Its events say so, unless its process is gone: a
/// terminal closed under a session sends no `SessionEnd`. A session
/// whose process varde could not find is known only by its events and
/// its terminal: once that is gone, so is it. On Linux its `tty_ctime`,
/// when recorded, says too whether the terminal is still the same one,
/// since a closed terminal's number is soon another's. A `chmod` (`mesg`)
/// changes the ctime as well and ends such a session early, an accepted
/// limit. Elsewhere the ctime moves with every write (macOS devfs), so
/// only the path counts.
pub fn effective_status(
    status: Option<Status>,
    pid: Option<i64>,
    pid_start: Option<i64>,
    tty: Option<(&str, Option<i64>)>,
    alive: &dyn Fn(i64, i64) -> bool,
) -> Option<Status> {
    match (status, pid) {
        (Some(Status::Working | Status::Delegated | Status::Waiting | Status::Blocked), Some(pid))
            if !alive(pid, pid_start.unwrap_or(0)) =>
        {
            Some(Status::Exited)
        }
        (Some(Status::Working | Status::Delegated | Status::Waiting | Status::Blocked), None)
            if tty.is_some_and(|(t, made)| {
                let now = tty_ctime(t);
                match made {
                    Some(made) if cfg!(target_os = "linux") => now != Some(made),
                    _ => now.is_none(),
                }
            }) =>
        {
            Some(Status::Exited)
        }
        _ => status,
    }
}

/// When the terminal at `path` was made, or `None` if there is none.
pub fn tty_ctime(path: &str) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some(m.ctime() * 1_000_000_000 + m.ctime_nsec())
}

/// Whether `run`'s process has since moved on to a later run (`/clear`,
/// `/resume`), perhaps of another task: the session is no longer `run`'s,
/// however alive its process.
pub fn moved_on(conn: &Connection, run: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM run r JOIN run n ON n.pid = r.pid
           AND coalesce(n.pid_start, 0) = coalesce(r.pid_start, 0) AND n.id != r.id
           AND (n.started > r.started OR (n.started = r.started AND n.rowid > r.rowid))
         WHERE r.id = ?1 AND r.pid > 0)",
        [run],
        |r| r.get(0),
    )?)
}

/// A live session is never hidden under Done or Parked: a task whose
/// conversation goes on is back to Open, as starting it would be.
fn reopen(conn: &Connection, task: &str) -> Result<()> {
    conn.execute(
        "UPDATE task SET state = 'open', closed = NULL WHERE id = ?1 AND state IN ('done', 'parked', 'dropped')",
        [task],
    )?;
    Ok(())
}

fn has_events(conn: &Connection, run: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM event WHERE run=?1 LIMIT 1", [run], |_| Ok(())).optional()?.is_some())
}

/// What a run's events say now. Read from its newest event back to the
/// last one that set its status outright, in the order they happened:
/// everything before that changes nothing, so the cost does not grow
/// with the run's history.
#[derive(Debug, Default)]
pub struct View {
    pub status: Option<Status>,
    /// What the run's last turn left running, if its end set the status:
    /// once the session is at work again the counts are stale.
    pub agents: u32,
    pub shells: u32,
    /// Its last turn ended, or it asked for a decision, and nobody has
    /// looked since. A session you started or interrupted is at its
    /// prompt too, but you were there.
    pub unread: bool,
    /// When the newest event that is not someone looking happened.
    pub last: Option<i64>,
    pub why: Why,
}

pub fn view(conn: &Connection, run: &str) -> Result<View> {
    let mut stmt = conn.prepare_cached(
        "SELECT kind, ts, agents, shells FROM event WHERE run = ?1 ORDER BY ts DESC, seq DESC",
    )?;
    let mut rows = stmt.query([run])?;
    let mut v = View::default();
    let (mut idle_at, mut seen_at) = (None, None);
    // Newest first.
    let mut tail: Vec<String> = Vec::new();
    while let Some(r) = rows.next()? {
        let kind: String = r.get(0)?;
        let ts: i64 = r.get(1)?;
        if kind == "seen" {
            seen_at.get_or_insert(ts);
            continue;
        }
        v.last.get_or_insert(ts);
        if kind == "idle" {
            idle_at.get_or_insert(ts);
        }
        if kind != "idle" && status::STATUS_KINDS.contains(&kind.as_str()) {
            if matches!(kind.as_str(), "turn_end" | "delegated") {
                let n = |i| r.get::<_, Option<i64>>(i).map(|n| n.unwrap_or(0) as u32);
                (v.agents, v.shells) = (n(2)?, n(3)?);
            }
            v.unread = matches!(kind.as_str(), "turn_end" | "blocked") && seen_at.is_none();
            v.why = Why { event: Some(kind.clone()), at: Some(ts), idle_at, seen_at, alive: None };
            tail.push(kind);
            break;
        }
        tail.push(kind);
    }
    v.status = status::fold(tail.iter().rev().map(String::as_str));
    Ok(v)
}

pub fn run_status(conn: &Connection, run: &str) -> Result<Option<Status>> {
    Ok(view(conn, run)?.status)
}

/// Whether a tool run at `ts` says anything about `run`. After a
/// permission dialog it is the proof the dialog was answered. After a
/// turn end it is the next turn starting with no prompt, as `/goal` does.
/// Anywhere else it is one of dozens of tool runs per turn, all meaning
/// "busy", or one you just interrupted reporting late. What came before
/// is judged by when it happened, so a tool run replayed late from the
/// pending file is judged where it belongs.
fn tool_run_counts(conn: &Connection, run: &str, ts: i64) -> Result<bool> {
    Ok(matches!(last_kinds(conn, run, ts, 1)?.first().map(String::as_str), Some("blocked" | "turn_end")))
}

/// A dialog closed while another is still open: the session is still
/// blocked. Not a status kind, so it sets no status, but `answered`
/// counts it.
const DIALOG_CLOSED: &str = "dialog_closed";

/// What an answered dialog at `ts` means for `run`: the turn it came in
/// going on (`unblocked`), the session back at its prompt (`answered`),
/// or, with another dialog still open, nothing yet (`DIALOG_CLOSED`).
/// `None` if no dialog was open.
fn answered(conn: &Connection, run: &str, ts: i64) -> Result<Option<&'static str>> {
    let kinds = last_kinds(conn, run, ts, 16)?;
    let dialogs = kinds.iter().take_while(|k| matches!(k.as_str(), "blocked" | DIALOG_CLOSED)).count();
    let opened = kinds[..dialogs].iter().filter(|k| *k == "blocked").count();
    Ok(match opened.saturating_sub(dialogs - opened) {
        0 => None,
        1 if matches!(kinds.get(dialogs).map(String::as_str), Some("prompt" | "unblocked")) => {
            Some("unblocked")
        }
        1 => Some("answered"),
        _ => Some(DIALOG_CLOSED),
    })
}

/// The last `n` kinds of `run`'s events at or before `ts` that bear on
/// its status, apart from `idle`, newest first.
fn last_kinds(conn: &Connection, run: &str, ts: i64, n: usize) -> Result<Vec<String>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT kind FROM event WHERE run = ?1 AND ts <= ?2 AND kind IN ({}, '{DIALOG_CLOSED}')
             ORDER BY ts DESC, seq DESC LIMIT ?3",
            non_idle_kinds()
        ))?
        .query_map(params![run, ts, n as i64], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

fn non_idle_kinds() -> String {
    status::STATUS_KINDS
        .iter()
        .filter(|k| **k != "idle")
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Someone looked at `task`'s sessions: what they finished is read.
/// Returns how many runs that changed.
/// Whether `run` is one a schedule started in the background that has
/// ended with nobody looking since: what it did is yours to read, as a
/// turn that ended is.
pub fn ended_unseen(conn: &Connection, run: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT background AND coalesce((SELECT max(ts) FROM event WHERE run = r.id AND kind = 'ended'), 0)
                 > coalesce((SELECT max(ts) FROM event WHERE run = r.id AND kind = 'seen'), 0)
             FROM run r WHERE id = ?1",
        )?
        .query_row([run], |r| r.get(0))
        .optional()?
        .unwrap_or(false))
}

pub fn mark_seen(conn: &Connection, task: &str, now: i64) -> Result<usize> {
    let runs: Vec<String> = conn
        .prepare_cached("SELECT id FROM run WHERE task = ?1")?
        .query_map([task], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut n = 0;
    for run in runs {
        if view(conn, &run)?.unread || ended_unseen(conn, &run)? {
            conn.execute(
                "INSERT INTO event (run, kind, ts, raw) VALUES (?1, 'seen', ?2, '{\"source\":\"varde seen\"}')",
                params![run, now],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

/// The events behind a run's status, for when it looks wrong.
#[derive(Debug, Default, serde::Serialize)]
pub struct Why {
    /// The last event that set the status outright, and when.
    pub event: Option<String>,
    pub at: Option<i64>,
    /// An `idle` after it, which moves `working` to `waiting`.
    pub idle_at: Option<i64>,
    /// When someone last looked, if after `event`.
    pub seen_at: Option<i64>,
    /// Whether the agent's process is still running; `None` when varde
    /// does not know its process, so only its events count.
    pub alive: Option<bool>,
}

/// Events older than this are dropped, apart from those a run's status
/// still rests on.
pub const KEEP_EVENTS_MS: i64 = 30 * 24 * 3600 * 1000;
/// Finished cairn jobs are kept as long, for looking back, and their
/// cost with them.
const KEEP_JOBS_MS: i64 = KEEP_EVENTS_MS;
/// What runs were shown and every recall are kept as long: `varde eval`
/// looks back no further, and a recall's query may be a prompt.
pub const KEEP_LOG_MS: i64 = KEEP_EVENTS_MS;

/// How many events one `tidy` deletes at most.
pub const TIDY_BATCH: usize = 5000;

/// Age out old events, finished jobs, what runs were shown and the recall
/// log, so the store does not grow with every hook ever fired, and merge
/// recall's index a step (`memory::merge_index`). A run keeps every event from the last one that
/// set its status on, which is all its status, unread mark and `why` are
/// read from. Called from detached work, never from a hook. One bounded
/// batch per call, so the write lock is never held for long and a hook
/// never waits on it: a large backlog clears over the next few jobs.
/// Returns how many events went.
pub fn tidy(conn: &Connection, now: i64) -> Result<usize> {
    let events = conn.execute(
        &format!(
            "DELETE FROM event WHERE seq IN (
               SELECT e.seq FROM run r JOIN event e ON e.run = r.id AND e.ts < ?1
               WHERE (e.ts, e.seq) < (SELECT ts, seq FROM event l WHERE l.run = r.id AND l.kind IN ({})
                                      ORDER BY ts DESC, seq DESC LIMIT 1)
               LIMIT {TIDY_BATCH})",
            non_idle_kinds()
        ),
        [now - KEEP_EVENTS_MS],
    )?;
    // A failed job is kept while it may still be retried.
    conn.execute(
        "DELETE FROM job WHERE finished < ?1
           AND (state IN ('done', 'unchanged', 'skipped') OR (state = 'failed' AND attempts >= ?2))",
        params![now - KEEP_JOBS_MS, crate::memory::MAX_ATTEMPTS],
    )?;
    // Rows go in about the order of their `ts`, so the oldest are found
    // first, without an index every hook would pay for.
    conn.execute(
        &format!("DELETE FROM shown WHERE id IN (SELECT id FROM shown WHERE ts < ?1 LIMIT {TIDY_BATCH})"),
        [now - KEEP_LOG_MS],
    )?;
    conn.execute(
        &format!(
            "DELETE FROM recall_log WHERE rowid IN (SELECT rowid FROM recall_log WHERE ts < ?1 LIMIT {TIDY_BATCH})"
        ),
        [now - KEEP_LOG_MS],
    )?;
    // And keep recall's index from fragmenting, which slows every hook
    // that offers related work.
    crate::memory::merge_index(conn)?;
    Ok(events)
}

/// A store of its own, in a directory that goes when it is dropped.
#[cfg(test)]
pub(crate) fn temp() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("varde.db"), Duration::from_millis(200)).unwrap();
    (dir, s)
}

#[cfg(test)]
mod tests;
