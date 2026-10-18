//! The jobs that update memory: a fold of a run's transcript into its
//! task's cairn, and a task's close-out. Queued from hooks and commands,
//! run detached.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{cairn, confirm, pending_notes, save_doc, Owner, Write};
use crate::cairn::{Cairn, Item, Section};
use crate::store::Store;

/// A cairn is folded on a `turn_end` at most this often; run end and
/// compaction always fold. That keeps a session left open for days in
/// memory without an update per turn.
pub const FOLD_EVERY_MS: i64 = 30 * 60 * 1000;

/// How far run `r`'s transcript has been folded. Every resume of a
/// conversation is a run of its own on the same transcript, and a fold of
/// the previous one may still be running when the next starts: the
/// furthest any of them got is where the next fold begins.
const CURSOR: &str = "(SELECT max(o.cairn_cursor) FROM run o
   WHERE o.id = r.id OR (o.task = r.task AND o.transcript = r.transcript))";

/// Queue a fold of `run` up to transcript position `upto`, unless one is
/// already queued there or the run has nothing new. Returns the job id.
pub fn enqueue(store: &Store, run: &str, upto: i64, now: i64) -> Result<Option<String>> {
    let cursor: Option<i64> = store
        .conn
        .query_row(
            &format!("SELECT {CURSOR} FROM run r WHERE r.id = ?1 AND r.task IS NOT NULL"),
            [run],
            |r| r.get(0),
        )
        .optional()?;
    match cursor {
        Some(c) if upto > c => {}
        _ => return Ok(None),
    }
    // A close-out of this run, waiting or under way, reads the same
    // material: one waiting is made to read this far too, rather than
    // two calls on it, the second thrown away.
    let close: Option<(String, i64, String)> = store
        .conn
        .query_row(
            "SELECT id, upto, state FROM job WHERE run = ?1 AND kind = 'close' AND state IN ('queued', 'running')
             ORDER BY created DESC LIMIT 1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match close {
        Some((_, reads, _)) if reads >= upto => return Ok(None),
        Some((id, _, state)) if state == "queued" => {
            store.conn.execute(
                "UPDATE OR IGNORE job SET upto = ?2 WHERE id = ?1 AND state = 'queued'",
                params![id, upto],
            )?;
            return Ok(None);
        }
        _ => {}
    }
    insert_job(&store.conn, run, upto, "fold", now)
}

fn insert_job(conn: &Connection, run: &str, upto: i64, kind: &str, now: i64) -> Result<Option<String>> {
    let id = ulid::Ulid::new().to_string();
    // A fold that failed, or whose process died, is tried again rather
    // than blocking this stretch of transcript for good. A close-out is
    // done again whenever the task is closed again.
    let job: Option<String> = conn
        .query_row(
            "INSERT INTO job (id, run, upto, kind, state, created) VALUES (?1, ?2, ?3, ?4, 'queued', ?5)
             ON CONFLICT (run, upto, kind) DO UPDATE SET state = 'queued', error = NULL, created = ?5,
                                                         beat = NULL, attempts = 0, finished = NULL
               WHERE job.state = 'failed'
                  OR (job.state IN ('queued', 'running') AND coalesce(job.beat, job.created) < ?6)
                  OR (job.kind = 'close' AND job.state IN ('done', 'unchanged', 'skipped'))
             RETURNING id",
            params![id, run, upto, kind, now, now - STUCK_AFTER_MS],
            |r| r.get(0),
        )
        .optional()?;
    Ok(job)
}

/// Queue the last update of a task just closed, parked or dropped: on its
/// latest run, up to `size` of that run's transcript (its length now, if
/// it can be read). Nothing when the task has no memory to settle and no
/// run with anything new.
pub fn enqueue_close(
    store: &Store,
    task: &str,
    size: &dyn Fn(&str) -> Option<i64>,
    now: i64,
) -> Result<Option<String>> {
    let run: Option<(String, Option<String>, i64)> = store
        .conn
        .query_row(
            &format!(
                "SELECT r.id, r.transcript, {CURSOR} FROM run r WHERE r.task = ?1
                 ORDER BY r.started DESC, r.rowid DESC LIMIT 1"
            ),
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((run, transcript, cursor)) = run else { return Ok(None) };
    let upto = transcript.as_deref().and_then(size).unwrap_or(cursor).max(cursor);
    let has_memory: bool = store.conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM cairn WHERE task = ?1)
             OR EXISTS (SELECT 1 FROM note n JOIN run r ON r.id = n.run WHERE r.task = ?1 AND n.folded = 0)",
        [task],
        |r| r.get(0),
    )?;
    if !has_memory && upto <= cursor {
        return Ok(None);
    }
    let job = insert_job(&store.conn, &run, upto, "close", now)?;
    // A fold still waiting on what the close-out reads would only find
    // the cairn moved underneath it.
    if job.is_some() {
        store.conn.execute(
            "UPDATE job SET state = 'skipped', finished = ?3, error = 'the close-out read it'
             WHERE run = ?1 AND kind = 'fold' AND state = 'queued' AND upto <= ?2",
            params![run, upto, now],
        )?;
    }
    Ok(job)
}

/// A job that has shown no sign of life for this long has died. A
/// running job beats before every model call, and one call is given at
/// most 3 minutes.
pub const STUCK_AFTER_MS: i64 = 10 * 60 * 1000;
/// How often a job is started before it is given up on, and how long
/// after a failure it is tried again.
pub const MAX_ATTEMPTS: i64 = 3;
pub const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;

/// Put every job that died, or failed fewer than `MAX_ATTEMPTS` times, back
/// in the queue, returning their ids for starting again. A run that has
/// ended never queues its last fold again, so without this a job killed or
/// failed then (by a shutdown, or a model having a bad minute) would leave
/// that stretch of the session out of memory for good. One that keeps
/// dying is failed instead.
pub fn requeue_stuck(conn: &Connection, now: i64) -> Result<Vec<String>> {
    conn.execute(
        "UPDATE job SET state = 'failed', error = 'it stopped without finishing, every time', finished = ?1
         WHERE state IN ('queued', 'running') AND coalesce(beat, created) < ?2 AND attempts >= ?3",
        params![now, now - STUCK_AFTER_MS, MAX_ATTEMPTS],
    )?;
    let mut stmt = conn.prepare(
        "UPDATE job SET state = 'queued', error = NULL, created = ?1, beat = NULL
         WHERE (state IN ('queued', 'running') AND coalesce(beat, created) < ?2)
            OR (state = 'failed' AND attempts < ?3 AND finished < ?4)
         RETURNING id",
    )?;
    let jobs = stmt
        .query_map(params![now, now - STUCK_AFTER_MS, MAX_ATTEMPTS, now - RETRY_AFTER_MS], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(jobs)
}

/// A running job is alive: it is about to wait on a model.
pub fn beat(conn: &Connection, job: &str, now: i64) -> Result<()> {
    conn.execute("UPDATE job SET beat = ?2 WHERE id = ?1 AND state = 'running'", params![job, now])?;
    Ok(())
}

/// One model call a job made, and the tokens it cost, when the agent
/// reports them.
pub fn record_call(conn: &Connection, job: &str, input: Option<i64>, output: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE job SET calls = coalesce(calls, 0) + 1,
                        input_tokens = CASE WHEN ?2 IS NULL THEN input_tokens ELSE coalesce(input_tokens, 0) + ?2 END,
                        output_tokens = CASE WHEN ?3 IS NULL THEN output_tokens ELSE coalesce(output_tokens, 0) + ?3 END
         WHERE id = ?1",
        params![job, input, output],
    )?;
    Ok(())
}

/// What a job's ADDs came to, for measuring re-discovery: how many
/// repeated a bullet word for word, and how many new bullets were close
/// to one its session had been shown.
pub fn record_adds(conn: &Connection, job: &str, repeated: usize, like_shown: usize) -> Result<()> {
    conn.execute(
        "UPDATE job SET repeated = ?2, like_shown = ?3 WHERE id = ?1",
        params![job, repeated as i64, like_shown as i64],
    )?;
    Ok(())
}

/// Should this event fold the run's transcript into its task's cairn?
pub fn fold_due(conn: &Connection, run: &str, kind: &str, now: i64) -> Result<bool> {
    Ok(match kind {
        "ended" | "compacting" => true,
        "turn_end" | "delegated" => {
            let since: Option<i64> = conn
                .query_row("SELECT coalesce(cairn_at, started) FROM run WHERE id = ?1", [run], |r| r.get(0))
                .optional()?;
            since.is_some_and(|s| now - s >= FOLD_EVERY_MS)
        }
        _ => false,
    })
}

/// Everything a job needs, read at the moment it starts.
pub struct JobInput {
    pub run: String,
    /// The run's agent: it reads the transcript and makes the update.
    pub agent: String,
    /// `fold`, or `close` for the last update of a task just closed.
    pub kind: String,
    pub task: String,
    pub title: String,
    pub body: Option<String>,
    /// The task's state now: `open`, `done`, `parked` or `dropped`.
    pub state: String,
    pub project: Option<String>,
    pub transcript: Option<String>,
    pub cursor: i64,
    pub cairn: Option<(Cairn, i64)>,
    pub notes: Vec<(String, String)>,
    /// Its schedule's rule, when the task starts again and again, each
    /// time a fresh session given only its cairn.
    pub repeats: Option<String>,
}

/// Take a queued job for running; `None` if it is not queued any more.
pub fn claim_job(store: &mut Store, job: &str, now: i64) -> Result<Option<JobInput>> {
    let tx = store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let n = tx.execute(
        "UPDATE job SET state = 'running', attempts = attempts + 1, beat = ?2 WHERE id = ?1 AND state = 'queued'",
        params![job, now],
    )?;
    if n == 0 {
        return Ok(None);
    }
    type Row =
        (String, String, String, String, String, Option<String>, String, Option<String>, Option<String>, i64);
    let (run, agent, kind, task, title, body, state, project, transcript, cursor): Row = tx.query_row(
        &format!(
            "SELECT r.id, r.agent, j.kind, t.id, t.title, t.body, t.state, t.project, r.transcript, {CURSOR}
             FROM job j JOIN run r ON r.id = j.run JOIN task t ON t.id = r.task WHERE j.id = ?1"
        ),
        [job],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        },
    )?;
    let repeats = tx
        .query_row("SELECT rule FROM schedule WHERE task = ?1", [&task], |r| r.get::<_, String>(0))
        .optional()?
        .filter(|rule| {
            crate::schedule::parse(rule, chrono::NaiveDateTime::default()).is_ok_and(|r| r.repeats())
        });
    let input = JobInput {
        repeats,
        cairn: cairn(&tx, &task)?,
        notes: pending_notes(&tx, &task)?,
        run,
        agent,
        kind,
        task,
        title,
        body,
        state,
        project,
        transcript,
        cursor,
    };
    tx.commit()?;
    Ok(Some(input))
}

pub enum Outcome<'a> {
    /// Nothing to read, so no model was asked: move the cursor, keep the
    /// cairn. The job is `skipped`.
    Skipped,
    /// The model found nothing worth keeping: move the cursor, keep the
    /// cairn, note the bullets said still to hold. The job is `unchanged`.
    Unchanged { confirmed: &'a [i64] },
    New {
        cairn: &'a Cairn,
        gone: &'a [(i64, String)],
        capped: &'a [(Section, Item)],
        confirmed: &'a [i64],
        reason: Option<&'a str>,
        /// Whether the edits took up the pinned notes (added, edited or
        /// confirmed a bullet), so they are folded; a Status alone is not.
        took_notes: bool,
    },
}

/// Record a job's result: the cairn (if still at the version the job
/// read), the run's new cursor, and the notes it folded. Returns false if
/// the cairn moved underneath it, in which case nothing is written and
/// the job should run again.
pub fn finish_job(
    store: &mut Store,
    job: &str,
    input: &JobInput,
    upto: i64,
    outcome: Outcome,
    now: i64,
) -> Result<bool> {
    let tx = store.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let expected = input.cairn.as_ref().map_or(0, |(_, v)| *v);
    let current = cairn(&tx, &input.task)?.map_or(0, |(_, v)| v);
    let cursor: i64 =
        tx.query_row(&format!("SELECT {CURSOR} FROM run r WHERE r.id = ?1"), [&input.run], |r| r.get(0))?;
    if current != expected || cursor != input.cursor {
        return Ok(false);
    }
    let took_notes = matches!(outcome, Outcome::New { took_notes: true, .. });
    let (confirmed, state) = match outcome {
        Outcome::Skipped => (&[][..], "skipped"),
        Outcome::Unchanged { confirmed } => (confirmed, "unchanged"),
        Outcome::New { cairn, gone, capped, confirmed, reason, .. } => {
            let default_gone = "left out by an update";
            let w = Write { author: "job", run: Some(&input.run), reason, gone, default_gone, capped, now };
            if !save_doc(&tx, Owner::Task(&input.task), expected, cairn, &w)? {
                return Ok(false);
            }
            (confirmed, "done")
        }
    };
    confirm(&tx, confirmed, now)?;
    // Only a written cairn whose edits took them up holds the notes;
    // otherwise they wait for the next update.
    if state == "done" && took_notes {
        for (id, _) in &input.notes {
            tx.execute("UPDATE note SET folded = 1 WHERE id = ?1", [id])?;
        }
    }
    tx.execute(
        "UPDATE run SET cairn_cursor = max(cairn_cursor, ?2), cairn_at = ?3 WHERE id = ?1",
        params![input.run, upto, now],
    )?;
    tx.execute("UPDATE job SET state = ?2, finished = ?3 WHERE id = ?1", params![job, state, now])?;
    tx.commit()?;
    Ok(true)
}

/// A running job failed. Only a running one: a stale process, woken from
/// sleep after its job was started again, must not undo what that did.
pub fn fail_job(conn: &Connection, job: &str, error: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE job SET state = 'failed', error = ?2, finished = ?3 WHERE id = ?1 AND state = 'running'",
        params![job, error, now],
    )?;
    Ok(())
}

/// Put a running job back in the queue, for another go.
pub fn requeue(conn: &Connection, job: &str) -> Result<()> {
    conn.execute("UPDATE job SET state = 'queued', error = NULL WHERE id = ?1 AND state = 'running'", [job])?;
    Ok(())
}

/// Cairn updates are off: the job is set aside without a model call.
pub fn skip_job(conn: &Connection, job: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE job SET state = 'skipped', finished = ?2 WHERE id = ?1 AND state = 'queued'",
        params![job, now],
    )?;
    Ok(())
}
