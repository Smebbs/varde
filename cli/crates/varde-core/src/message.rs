//! Messages `varde say` sends a task whose session is open (§6.6): kept
//! here first, so every agent gets them the same way, by its own way in
//! or with its next hook.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// A message for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub text: String,
    pub sender: Option<String>,
    /// The run that wants its answer, if any.
    pub reply_to: Option<String>,
}

/// Keep `text` for `task`; its id. `reply_to`, the run that sent it,
/// gets the answer back once the turn that takes it ends.
pub fn send(
    conn: &Connection,
    task: &str,
    text: &str,
    sender: Option<&str>,
    reply_to: Option<&str>,
    now: i64,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO message (id, task, text, sender, reply_to, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, task, text, sender, reply_to, now],
    )?;
    Ok(id)
}

/// Whether `run` owes an answer: a message that asked for one went in
/// before `now`. One indexed read, for every turn end.
pub fn owed(conn: &Connection, run: &str, now: i64) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM message WHERE run = ?1 AND reply_to IS NOT NULL
               AND replied IS NULL AND delivered <= ?2)",
        )?
        .query_row(params![run, now], |r| r.get(0))?)
}

/// A message `run` owes an answer to.
#[derive(Debug, Clone)]
pub struct Owed {
    pub id: String,
    pub text: String,
    /// The run that wants it.
    pub reply_to: String,
    /// Where the transcript stood when it went in.
    pub mark: Option<u64>,
    /// How it went in: `push`, `hook` or `resume`.
    pub via: Option<String>,
}

/// What `run` owes, oldest first.
pub fn owed_by(conn: &Connection, run: &str, now: i64) -> Result<Vec<Owed>> {
    Ok(conn
        .prepare_cached(
            "SELECT id, text, reply_to, mark, via FROM message WHERE run = ?1 AND reply_to IS NOT NULL
               AND replied IS NULL AND delivered <= ?2 ORDER BY delivered",
        )?
        .query_map(params![run, now], |r| {
            Ok(Owed {
                id: r.get(0)?,
                text: r.get(1)?,
                reply_to: r.get(2)?,
                mark: r.get::<_, Option<i64>>(3)?.map(|m| m as u64),
                via: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Mark `id` answered, once: says whether this did.
pub fn answered(conn: &Connection, id: &str, now: i64) -> Result<bool> {
    Ok(conn.execute("UPDATE message SET replied = ?2 WHERE id = ?1 AND replied IS NULL", params![id, now])?
        > 0)
}

/// Record message `id` as in `run`'s session, by `via`, with its
/// transcript at `mark`. Only once: says whether this did.
pub fn delivered(
    conn: &Connection,
    id: &str,
    run: &str,
    via: &str,
    mark: Option<u64>,
    now: i64,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE message SET delivered = ?2, via = ?3, run = ?4, mark = ?5 WHERE id = ?1 AND delivered IS NULL",
        params![id, now, via, run, mark.map(|m| m as i64)],
    )?;
    Ok(n > 0)
}

/// Give message `id` back, not delivered after all: a push that did not
/// go in leaves it for the session's next hook.
pub fn undeliver(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE message SET delivered = NULL, via = NULL, run = NULL, mark = NULL WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Whether any message waits for a task that may still run: one read, so
/// a hook with nothing to hand over stays as cheap as it was. A closed
/// task's are left out, or one never collected would cost every hook.
pub fn any_waiting(conn: &Connection) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM message m JOIN task t ON t.id = m.task
               WHERE m.delivered IS NULL AND t.state IN ('open', 'parked'))",
        )?
        .query_row([], |r| r.get(0))?)
}

/// Whether a message waits for `run`'s task.
pub fn waiting_for(conn: &Connection, run: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM message WHERE delivered IS NULL
               AND task = (SELECT task FROM run WHERE id = ?1))",
        )?
        .query_row([run], |r| r.get(0))?)
}

/// Take what waits for `run`'s task: marked delivered to `run` by its
/// hook, oldest first.
pub fn take(conn: &Connection, run: &str, mark: Option<u64>, now: i64) -> Result<Vec<Message>> {
    let Some(task) = conn
        .prepare_cached("SELECT task FROM run WHERE id = ?1")?
        .query_row([run], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten()
    else {
        return Ok(vec![]);
    };
    let waiting: Vec<Message> = conn
        .prepare_cached(
            "SELECT id, text, sender, reply_to FROM message WHERE task = ?1 AND delivered IS NULL
             ORDER BY created, id",
        )?
        .query_map([&task], |r| {
            Ok(Message { id: r.get(0)?, text: r.get(1)?, sender: r.get(2)?, reply_to: r.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut taken = Vec::new();
    for m in waiting {
        if delivered(conn, &m.id, run, "hook", mark, now)? {
            taken.push(m);
        }
    }
    Ok(taken)
}

/// Message `id` once delivered: the run, and where its transcript stood.
pub fn landed(conn: &Connection, id: &str) -> Result<Option<(String, Option<u64>, i64)>> {
    Ok(conn
        .prepare_cached("SELECT run, mark, delivered FROM message WHERE id = ?1 AND delivered IS NOT NULL")?
        .query_row([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?.map(|m| m as u64), r.get(2)?))
        })
        .optional()?)
}
