//! Tasks: creating, accepting, linking, finding and describing them.

use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::store::{self, Store};

/// The part of a task id people type and see: its last six characters,
/// which are random. The first ten are a timestamp, shared by every task
/// made the same day.
pub fn short(id: &str) -> String {
    id[id.len().saturating_sub(6)..].to_ascii_lowercase()
}

/// The title as stored: one line, with nothing in it a terminal acts on.
fn need_title(title: &str) -> Result<String> {
    let title = crate::title::clean(title).trim().to_string();
    if title.is_empty() {
        bail!("a task needs a title");
    }
    Ok(title)
}

/// The title of a task made with `title` and context `body`: the one
/// typed, else one cut from the context, as an ad-hoc session's is from
/// its first prompt. The second is the cut one, which a model may name
/// the task after (§6.2).
fn titled(title: &str, body: Option<&str>) -> Result<(String, Option<String>)> {
    if !crate::title::clean(title).trim().is_empty() {
        return Ok((need_title(title)?, None));
    }
    let Some(cut) = body.and_then(crate::title::from_prompt) else {
        bail!("a task needs a title or a context");
    };
    Ok((cut.clone(), Some(cut)))
}

/// Make a task. One given only its context is titled from it, and a
/// model names it (§6.2).
pub fn create(
    store: &Store,
    title: &str,
    body: Option<&str>,
    project: Option<&str>,
    external: Option<&str>,
    now: i64,
) -> Result<String> {
    let (title, prompt_title) = titled(title, body)?;
    let id = ulid::Ulid::new().to_string();
    store.conn.execute(
        "INSERT INTO task (id, title, prompt_title, body, project, external, created)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![id, title, prompt_title, body, project, external, now],
    )?;
    crate::memory::reindex(&store.conn, &id)?;
    Ok(id)
}

/// Split work off the task of run `from` into a task of its own, in
/// `project`, else in the same project. It waits in the Inbox until
/// someone picks it up or drops it. Its title is the agent's, or cut
/// from its context, never yours: a model names it (§6.2).
pub fn defer(
    store: &Store,
    title: &str,
    body: Option<&str>,
    from: &str,
    project: Option<&str>,
    now: i64,
) -> Result<String> {
    let (title, _) = titled(title, body)?;
    let origin: Option<(String, Option<String>)> = store
        .conn
        .query_row(
            "SELECT t.id, t.project FROM run r JOIN task t ON t.id = r.task WHERE r.id = ?1",
            [from],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((origin, theirs)) = origin else { bail!("run {from} has no task to defer from") };
    let project = project.map(String::from).or(theirs);
    let id = ulid::Ulid::new().to_string();
    store.conn.execute(
        "INSERT INTO task (id, title, prompt_title, body, project, inbox, origin, created)
         VALUES (?1,?2,?2,?3,?4,1,?5,?6)",
        params![id, title, body, project, origin, now],
    )?;
    crate::memory::reindex(&store.conn, &id)?;
    Ok(id)
}

/// Refresh a task's title and body from where it is written, e.g. an org
/// heading. The task's identity never depends on them.
pub fn set(store: &Store, id: &str, title: Option<&str>, body: Option<&str>) -> Result<Set> {
    let mut set = Set::default();
    if let Some(b) = body {
        let b = b.trim();
        let b = (!b.is_empty()).then_some(b);
        set.body = store
            .conn
            .execute("UPDATE task SET body = ?2 WHERE id = ?1 AND body IS NOT ?2", params![id, b])?
            > 0;
    }
    if let Some(t) = title {
        set.title = if crate::title::clean(t).trim().is_empty() {
            // No title: cut from the context, as `create` does, and a
            // model names the task after it, once more.
            let context: Option<String> =
                store.conn.query_row("SELECT body FROM task WHERE id = ?1", [id], |r| r.get(0))?;
            let Some(cut) = context.as_deref().and_then(crate::title::from_prompt) else {
                bail!("a task needs a title or a context");
            };
            store.conn.execute(
                "UPDATE task SET title = ?2, prompt_title = ?2, named = NULL
                 WHERE id = ?1 AND (title IS NOT ?2 OR prompt_title IS NOT ?2 OR named IS NOT NULL)",
                params![id, cut],
            )? > 0
        } else {
            let t = need_title(t)?;
            // A title changed here is yours: no model names the task after it.
            store.conn.execute(
                "UPDATE task SET title = ?2, prompt_title = NULL WHERE id = ?1 AND title IS NOT ?2",
                params![id, t],
            )? > 0
        };
        if set.title {
            // Recall finds a task by its title too.
            crate::memory::reindex(&store.conn, id)?;
        }
    }
    Ok(set)
}

/// What `set` changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Set {
    pub title: bool,
    /// The context.
    pub body: bool,
}

/// What a model reads to name a task (§6.2).
#[derive(Debug, PartialEq, Eq)]
pub struct ToName {
    /// Its title so far.
    pub title: String,
    /// What it was made from: its context, else its first prompt. `None`
    /// when it has neither, and there is nothing to name it from.
    pub text: Option<String>,
    /// The agent of its last run, if it has run.
    pub agent: Option<String>,
}

/// A task varde titled itself and has not yet asked a model to name:
/// its title is still the one it cut or the agent wrote, and the task is
/// not linked to where it is written.
const UNNAMED: &str = "named IS NULL AND title = prompt_title AND external IS NULL";

/// Tasks waiting to be named, newest first, at most `limit`.
pub fn unnamed(store: &Store, limit: usize) -> Result<Vec<String>> {
    let mut q =
        store.conn.prepare(&format!("SELECT id FROM task WHERE {UNNAMED} ORDER BY created DESC LIMIT ?1"))?;
    let ids = q.query_map([limit as i64], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Whether task `id` waits to be named.
pub fn waits_for_name(store: &Store, id: &str) -> Result<bool> {
    let q = format!("SELECT count(*) FROM task WHERE id = ?1 AND {UNNAMED}");
    Ok(store.conn.query_row(&q, [id], |r| r.get::<_, i64>(0))? > 0)
}

/// Take task `id`'s one attempt at being named, as of `now`: what to
/// name it from, or `None` when it is not waiting (already tried, named,
/// titled by you, or linked). Taken before the model is asked, so two
/// callers never both ask, and a failure is not tried again.
pub fn claim_naming(store: &Store, id: &str, now: i64) -> Result<Option<ToName>> {
    let n = store
        .conn
        .execute(&format!("UPDATE task SET named = ?2 WHERE id = ?1 AND {UNNAMED}"), params![id, now])?;
    if n == 0 {
        return Ok(None);
    }
    let (title, body): (String, Option<String>) =
        store
            .conn
            .query_row("SELECT title, body FROM task WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let first_prompt = || -> rusqlite::Result<Option<String>> {
        store
            .conn
            .query_row(
                "SELECT json_extract(e.raw, '$.prompt') FROM run r JOIN event e ON e.run = r.id
                 WHERE r.task = ?1 AND e.kind = 'prompt' AND json_valid(e.raw)
                   AND json_type(e.raw, '$.prompt') = 'text'
                 ORDER BY e.ts, e.seq LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map(Option::flatten)
    };
    let text = match body.filter(|b| !b.trim().is_empty()) {
        Some(b) => Some(b),
        None => first_prompt()?.filter(|p| !p.trim().is_empty()),
    };
    let agent = store
        .conn
        .query_row("SELECT agent FROM run WHERE task = ?1 ORDER BY started DESC LIMIT 1", [id], |r| r.get(0))
        .optional()?;
    Ok(Some(ToName { title, text, agent }))
}

/// Name a task as a model read it (§6.2): only while its title is still
/// the one varde cut or the agent wrote, and it is not linked to where it
/// is written, so a title you set meanwhile wins. Returns whether it was
/// renamed. `title` must already be one `title::from_model` gave.
pub fn name(store: &Store, id: &str, title: &str) -> Result<bool> {
    let n = store.conn.execute(
        "UPDATE task SET title = ?2
         WHERE id = ?1 AND title = prompt_title AND external IS NULL AND title IS NOT ?2",
        params![id, title],
    )?;
    if n > 0 {
        crate::memory::reindex(&store.conn, id)?;
    }
    Ok(n > 0)
}

/// Move a task to another project. Its runs keep the directories they
/// ran in, and its cairn stays with it. What git said of it was of the
/// old repo.
pub fn set_project(store: &Store, id: &str, project: &str) -> Result<()> {
    store.conn.execute("UPDATE task SET project = ?2 WHERE id = ?1", params![id, project])?;
    store.conn.execute("DELETE FROM git_state WHERE task = ?1", [id])?;
    Ok(())
}

/// Take a task out of the Inbox: you mean to keep it.
pub fn accept(store: &Store, id: &str) -> Result<()> {
    store.conn.execute("UPDATE task SET inbox = 0 WHERE id = ?1", [id])?;
    Ok(())
}

/// Record where the task also lives, e.g. `org`, which then owns its title
/// and body. Linking a task accepts it.
pub fn link(store: &Store, id: &str, external: &str) -> Result<()> {
    let external = external.trim();
    if external.is_empty() {
        bail!("a link needs a name, e.g. org");
    }
    store.conn.execute("UPDATE task SET external = ?2, inbox = 0 WHERE id = ?1", params![id, external])?;
    Ok(())
}

/// Forget where the task lives elsewhere. It stays where it is on the board.
pub fn unlink(store: &Store, id: &str) -> Result<()> {
    store.conn.execute("UPDATE task SET external = NULL WHERE id = ?1", [id])?;
    Ok(())
}

/// Find a task by its full id, by a unique ending of it of at least four
/// characters, or by a unique start of what `short` shows, like `b9ht` for
/// `b9htgs`. Case does not matter. Fewer than four characters name too
/// many tasks for a command that closes or drops one.
pub fn resolve(store: &Store, reference: &str) -> Result<String> {
    let r = reference.trim().to_ascii_uppercase();
    if r.len() < 4 {
        bail!("task reference '{reference}' is too short: use the six characters the board shows");
    }
    if !r.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("'{reference}' is not a task id");
    }
    let mut stmt = store.conn.prepare_cached(
        "SELECT id FROM task
         WHERE (length(?1) >= 4 AND substr(id, -length(?1)) = ?1)
            OR (length(?1) <= 6 AND substr(id, -6, length(?1)) = ?1)
         ORDER BY id = ?1 DESC LIMIT 3",
    )?;
    let ids: Vec<String> = stmt.query_map([&r], |row| row.get(0))?.collect::<Result<_, _>>()?;
    match ids.as_slice() {
        [] => bail!("no task matches '{reference}'"),
        [one] => Ok(one.clone()),
        [first, ..] if *first == r => Ok(first.clone()),
        _ => bail!("'{reference}' matches more than one task; use more characters"),
    }
}

#[derive(Debug, Serialize)]
pub struct Detail {
    pub v: u32,
    pub id: String,
    pub short: String,
    pub title: String,
    pub body: Option<String>,
    pub project: Option<String>,
    pub state: String,
    /// Arrived without you creating it, from a session or deferred from
    /// one, and not yet acted on.
    pub inbox: bool,
    pub external: Option<String>,
    /// The title varde cut (from its first prompt or its context) or the
    /// agent wrote, when a model has since named the task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_title: Option<String>,
    /// The task this one was deferred from.
    pub origin: Option<String>,
    pub created: i64,
    pub closed: Option<i64>,
    /// When its state, Inbox or link last changed, unix ms.
    pub changed: i64,
    pub runs: Vec<RunDetail>,
    /// Its runs as conversations, newest first, as the view lists its
    /// sessions.
    pub conversations: Vec<Conversation>,
    pub worktree: Option<WorktreeDetail>,
}

/// A conversation of the task: a run and every run that continued it,
/// resumed or forked.
#[derive(Debug, Serialize)]
pub struct Conversation {
    /// Its first run, which `start --run` opens.
    pub run: String,
    pub started: i64,
    /// The status of a live run of it: `working`, `delegated`, `waiting`
    /// or `blocked`.
    pub status: Option<&'static str>,
    /// A background run of it ended, and what it found is not yet opened.
    pub unread: bool,
    /// A background run of it ended with its own `varde done`: it found
    /// nothing to report.
    pub quiet: bool,
    /// False while `start` has prepared it and `varde run` not launched it.
    pub launched: bool,
    /// When it was last continued, and how: `resume` or `fork`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continued: Option<(i64, String)>,
}

#[derive(Debug, Serialize)]
pub struct RunDetail {
    pub id: String,
    pub native_id: String,
    pub relation: String,
    pub parent_run: Option<String>,
    pub launch: String,
    pub cwd: Option<String>,
    pub started: i64,
    pub status: Option<&'static str>,
    pub why: store::Why,
    pub exit_code: Option<i64>,
    /// False for a run `start` prepared that `varde run` never launched.
    pub launched: bool,
    /// Held out of memory (`memory.holdout`): given only the user's own
    /// lines.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub held_out: bool,
    /// Started by its schedule in the background, with no terminal.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// When its own session ran `varde done`: it needs nothing more of
    /// you, and a background run so ended found nothing to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<i64>,
    /// A background run that ended, with what it found not yet opened.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

#[derive(Debug, Serialize)]
pub struct WorktreeDetail {
    pub path: String,
    pub repo: String,
    pub branch: String,
    pub base: String,
    pub state: String,
}

pub fn detail(store: &Store, id: &str, alive: &dyn Fn(i64, i64) -> bool) -> Result<Detail> {
    let mut d = store.conn.query_row(
        "SELECT id, title, body, project, state, inbox, external, created, closed, origin,
                coalesce(changed, created), CASE WHEN title IS NOT prompt_title THEN prompt_title END
         FROM task WHERE id = ?1",
        [id],
        |r| {
            let id: String = r.get(0)?;
            Ok(Detail {
                v: 2,
                short: short(&id),
                id,
                title: r.get(1)?,
                body: r.get(2)?,
                project: r.get(3)?,
                state: r.get(4)?,
                inbox: r.get(5)?,
                external: r.get(6)?,
                created: r.get(7)?,
                closed: r.get(8)?,
                origin: r.get(9)?,
                changed: r.get(10)?,
                prompt_title: r.get(11)?,
                runs: Vec::new(),
                conversations: Vec::new(),
                worktree: None,
            })
        },
    )?;
    let mut stmt = store.conn.prepare_cached(&format!(
        "SELECT id, native_id, relation, parent_run, launch, cwd, started, exit_code, pid, pid_start,
                coalesce(holdout, 0), tty, tty_ctime, background, done, {}
         FROM run r WHERE task = ?1 ORDER BY started, rowid",
        store::UNREAD_RESULT
    ))?;
    let runs = stmt.query_map([id], |r| {
        let process: (Option<i64>, Option<i64>, Option<String>, Option<i64>) =
            (r.get(8)?, r.get(9)?, r.get(11)?, r.get(12)?);
        Ok((
            process,
            RunDetail {
                id: r.get(0)?,
                native_id: r.get(1)?,
                relation: r.get(2)?,
                parent_run: r.get(3)?,
                launch: r.get(4)?,
                cwd: r.get(5)?,
                started: r.get(6)?,
                status: None,
                why: store::Why::default(),
                exit_code: r.get(7)?,
                launched: true,
                held_out: r.get(10)?,
                background: r.get(13)?,
                done: r.get(14)?,
                unread: r.get(15)?,
            },
        ))
    })?;
    for run in runs {
        let ((pid, pid_start, tty, tty_ctime), mut run) = run?;
        let view = store::view(&store.conn, &run.id)?;
        run.status = store::effective_status(
            view.status,
            pid,
            pid_start,
            tty.as_deref().map(|t| (t, tty_ctime)),
            alive,
        )
        .map(|s| s.name());
        run.why = view.why;
        run.why.alive = pid.map(|p| alive(p, pid_start.unwrap_or(0)));
        run.launched =
            run.launch != "managed" || pid.is_some() || run.exit_code.is_some() || run.why.event.is_some();
        d.runs.push(run);
    }
    // A run prepared and never launched, as by a client that only asked,
    // says nothing once a later one exists. It is kept in the store.
    let last = d.runs.len().saturating_sub(1);
    let mut i = 0;
    d.runs.retain(|r| {
        i += 1;
        r.launched || i - 1 == last
    });
    d.worktree = store
        .conn
        .query_row(
            "SELECT path, repo, branch, base, state FROM worktree WHERE task = ?1
             ORDER BY created DESC LIMIT 1",
            [id],
            |r| {
                Ok(WorktreeDetail {
                    path: r.get(0)?,
                    repo: r.get(1)?,
                    branch: r.get(2)?,
                    base: r.get(3)?,
                    state: r.get(4)?,
                })
            },
        )
        .optional()?;
    d.conversations = conversations(&d.runs);
    Ok(d)
}

/// `runs`, oldest first, as conversations, newest first.
fn conversations(runs: &[RunDetail]) -> Vec<Conversation> {
    let mut out: Vec<Conversation> = Vec::new();
    // Each run's conversation, by its first run.
    let mut root: std::collections::HashMap<&str, usize> = Default::default();
    for r in runs {
        let i = match r.parent_run.as_deref().and_then(|p| root.get(p)) {
            Some(&i) => i,
            None => {
                out.push(Conversation {
                    run: r.id.clone(),
                    started: r.started,
                    status: None,
                    unread: false,
                    quiet: false,
                    launched: false,
                    continued: None,
                });
                out.len() - 1
            }
        };
        root.insert(&r.id, i);
        let c = &mut out[i];
        // One whose own session said it was done needs nothing of you.
        let done = r.done.is_some();
        if let Some(s) = r.status.filter(|s| !(*s == "exited" || done && matches!(*s, "waiting" | "blocked")))
        {
            c.status = Some(s);
        }
        c.unread |= r.unread;
        c.quiet |= r.background && r.done.is_some();
        c.launched |= r.launched;
        if c.run != r.id {
            c.continued = Some((r.started, r.relation.clone()));
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp as store;

    #[test]
    fn its_runs_are_listed_as_conversations_newest_first() {
        let (_d, s) = store();
        let id = create(&s, "Scan", None, Some("/p"), None, 1).unwrap();
        for (run, parent, relation, started, background) in [
            ("R1", None, "fresh", 10, 1),
            ("R2", Some("R1"), "resume", 20, 0),
            ("R3", None, "fresh", 30, 1),
            ("R4", Some("R3"), "fork", 40, 0),
            ("R5", None, "fresh", 50, 1),
        ] {
            s.conn
                .execute(
                    "INSERT INTO run (id, task, agent, native_id, parent_run, relation, started, launch, background, pid)
                     VALUES (?1, ?2, 'claude', ?1, ?3, ?4, ?5, 'managed', ?6, 1)",
                    rusqlite::params![run, id, parent, relation, started, background],
                )
                .unwrap();
        }
        for (run, kind, ts) in [
            ("R1", "ended", 11),
            ("R2", "ended", 21),
            ("R3", "ended", 31),
            ("R4", "prompt", 41),
            ("R5", "ended", 51),
        ] {
            s.conn
                .execute(
                    "INSERT INTO event (run, kind, ts, raw) VALUES (?1, ?2, ?3, '{}')",
                    rusqlite::params![run, kind, ts],
                )
                .unwrap();
        }
        s.conn.execute("UPDATE run SET done = 51 WHERE id = 'R5'", []).unwrap();
        let d = detail(&s, &id, &|_, _| true).unwrap();
        let got: Vec<_> =
            d.conversations.iter().map(|c| (c.run.as_str(), c.status, c.unread, c.quiet)).collect();
        assert_eq!(
            got,
            [("R5", None, false, true), ("R3", Some("working"), true, false), ("R1", None, true, false)]
        );
        let continued: Vec<_> = d.conversations.iter().map(|c| c.continued.clone()).collect();
        assert_eq!(continued, [None, Some((40, "fork".into())), Some((20, "resume".into()))]);
    }

    #[test]
    fn a_task_is_found_by_its_short_id() {
        let (_d, s) = store();
        let id = create(&s, "Fix it", None, Some("/p"), None, 1).unwrap();
        assert_eq!(resolve(&s, &short(&id)).unwrap(), id);
        assert_eq!(resolve(&s, &id.to_lowercase()).unwrap(), id);
        assert_eq!(resolve(&s, &short(&id)[..4]).unwrap(), id);
        assert!(resolve(&s, "zzzzzz").is_err());
        assert!(resolve(&s, "a").is_err());
        // Too few characters to close or drop a task by.
        assert!(resolve(&s, &short(&id)[..3]).unwrap_err().to_string().contains("too short"));
        // Not a pattern: as LIKE wildcards these would match any task.
        assert!(resolve(&s, "____").is_err());
        assert!(resolve(&s, "%%%%").is_err());
    }

    #[test]
    fn an_ambiguous_reference_is_refused() {
        let (_d, s) = store();
        for (id, t) in [("01AAAAAAAAAAAAAAAAAAAXYZ12", "a"), ("01BBBBBBBBBBBBBBBBBBBXYZ12", "b")] {
            s.conn.execute("INSERT INTO task (id, title, created) VALUES (?1, ?2, 1)", [id, t]).unwrap();
        }
        assert!(resolve(&s, "xyz12").unwrap_err().to_string().contains("more than one"));
        assert_eq!(resolve(&s, "Axyz12").unwrap(), "01AAAAAAAAAAAAAAAAAAAXYZ12");
    }

    #[test]
    fn a_task_is_found_by_the_start_of_its_short_id() {
        let (_d, s) = store();
        for (id, t) in [("01AAAAAAAAAAAAAAAAAAB9HTGS", "a"), ("01BBBBBBBBBBBBBBBBBBB9XQ4K", "b")] {
            s.conn.execute("INSERT INTO task (id, title, created) VALUES (?1, ?2, 1)", [id, t]).unwrap();
        }
        assert_eq!(resolve(&s, "b9ht").unwrap(), "01AAAAAAAAAAAAAAAAAAB9HTGS");
        assert_eq!(resolve(&s, "B9XQ").unwrap(), "01BBBBBBBBBBBBBBBBBBB9XQ4K");
        // The start of the short id, not of the full one.
        assert!(resolve(&s, "01AA").is_err());
    }

    #[test]
    fn accepting_or_linking_takes_a_task_out_of_the_inbox() {
        let (_d, s) = store();
        s.conn
            .execute(
                "INSERT INTO task (id, title, inbox, created)
                 VALUES ('T1XXXXXX','a',1,1), ('T2XXXXXX','b',1,1)",
                [],
            )
            .unwrap();
        let get = |id| detail(&s, id, &|_, _| true).unwrap();
        accept(&s, "T1XXXXXX").unwrap();
        assert!(!get("T1XXXXXX").inbox);
        assert_eq!(get("T1XXXXXX").external, None);
        link(&s, "T2XXXXXX", "org").unwrap();
        assert!(!get("T2XXXXXX").inbox);
        assert_eq!(get("T2XXXXXX").external.as_deref(), Some("org"));
        assert!(link(&s, "T2XXXXXX", " ").is_err());
        unlink(&s, "T2XXXXXX").unwrap();
        let d = get("T2XXXXXX");
        assert_eq!((d.inbox, d.external), (false, None));
    }

    #[test]
    fn only_a_title_cut_from_a_prompt_is_named_and_only_once() {
        let (_d, s) = store();
        s.conn
            .execute(
                "INSERT INTO task (id, title, prompt_title, created)
                 VALUES ('T1XXXXXX','fix the','fix the',1), ('T2XXXXXX','b','b',1), ('T3XXXXXX','c','c',1)",
                [],
            )
            .unwrap();
        let get = |id| detail(&s, id, &|_, _| true).unwrap();
        let mine = create(&s, "Typed by you", None, None, None, 1).unwrap();
        assert!(!name(&s, &mine, "Renamed").unwrap());
        assert_eq!(get(&mine).title, "Typed by you");
        // Named once, and the first prompt's words are kept.
        assert!(name(&s, "T1XXXXXX", "Fix the login").unwrap());
        assert!(!name(&s, "T1XXXXXX", "Again").unwrap());
        let d = get("T1XXXXXX");
        assert_eq!((d.title.as_str(), d.prompt_title.as_deref()), ("Fix the login", Some("fix the")));
        // A title you set, or a link, wins.
        set(&s, "T2XXXXXX", Some("Yours"), None).unwrap();
        assert!(!name(&s, "T2XXXXXX", "Renamed").unwrap());
        assert_eq!(get("T2XXXXXX").prompt_title, None);
        link(&s, "T3XXXXXX", "org").unwrap();
        assert!(!name(&s, "T3XXXXXX", "Renamed").unwrap());
        // Saving the same words is not setting a title.
        set(&s, "T1XXXXXX", Some("Fix the login"), None).unwrap();
        assert_eq!(get("T1XXXXXX").prompt_title.as_deref(), Some("fix the"));
        assert_eq!(get("T3XXXXXX").prompt_title, None, "shown only once named");
    }

    #[test]
    fn a_task_made_on_purpose_is_never_in_the_inbox() {
        let (_d, s) = store();
        let id = create(&s, "Fix it", None, None, None, 1).unwrap();
        assert!(!detail(&s, &id, &|_, _| true).unwrap().inbox);
    }

    #[test]
    fn deferred_work_waits_in_the_inbox_in_its_origin_project() {
        let (_d, s) = store();
        let origin = create(&s, "Big job", None, Some("/p"), Some("org"), 1).unwrap();
        s.conn
            .execute(
                "INSERT INTO run (id, task, agent, native_id, relation, started)
                 VALUES ('R1', ?1, 'claude', 'n', 'fresh', 1)",
                [&origin],
            )
            .unwrap();
        let id = defer(&s, "Later bit", Some("why"), "R1", None, 2).unwrap();
        let d = detail(&s, &id, &|_, _| true).unwrap();
        assert!(d.inbox);
        assert_eq!(d.origin.as_deref(), Some(origin.as_str()));
        assert_eq!((d.project.as_deref(), d.external), (Some("/p"), None));
        assert!(defer(&s, "x", None, "nope", None, 3).is_err());
        let other = defer(&s, "Elsewhere", None, "R1", Some("/q"), 4).unwrap();
        let d = detail(&s, &other, &|_, _| true).unwrap();
        assert_eq!((d.project.as_deref(), d.origin.as_deref()), (Some("/q"), Some(origin.as_str())));
    }

    #[test]
    fn title_and_body_follow_their_source() {
        let (_d, s) = store();
        let id = create(&s, "Old", Some("old body"), None, None, 1).unwrap();
        set(&s, &id, Some("New"), Some("  ")).unwrap();
        let d = detail(&s, &id, &|_, _| true).unwrap();
        assert_eq!((d.title.as_str(), d.body), ("New", None));
        assert!(set(&s, &id, Some(" "), None).is_err());
    }

    #[test]
    fn a_title_is_required() {
        let (_d, s) = store();
        assert!(create(&s, "  ", None, None, None, 1).is_err());
        assert!(create(&s, "\x1b\x07", None, None, None, 1).is_err());
        assert!(create(&s, " ", Some("?!"), None, None, 1).is_err(), "nothing in the context to title");
    }

    #[test]
    fn a_task_given_only_its_context_is_titled_from_it_and_named_once() {
        let (_d, s) = store();
        let body = "\nhey so the login page is in english for everyone\nmore";
        let id = create(&s, " ", Some(body), None, None, 1).unwrap();
        let d = detail(&s, &id, &|_, _| true).unwrap();
        assert_eq!(
            (d.title.as_str(), d.body.as_deref()),
            ("hey so the login page is in english…", Some(body))
        );
        assert!(name(&s, &id, "Login page shown in English").unwrap());
        assert_eq!(detail(&s, &id, &|_, _| true).unwrap().prompt_title.as_deref(), Some(d.title.as_str()));
        assert!(!name(&s, &id, "Again").unwrap());
        // A title typed, even beside a context, is yours.
        let typed = create(&s, "Fix it", Some("the login page"), None, None, 1).unwrap();
        assert!(!name(&s, &typed, "Renamed").unwrap());
    }

    #[test]
    fn a_task_varde_titled_is_claimed_for_naming_once() {
        let (_d, s) = store();
        let ctx = create(&s, "", Some("why is the cart total off by a cent"), None, None, 1).unwrap();
        let typed = create(&s, "Typed", Some("context"), None, None, 2).unwrap();
        let linked = create(&s, "", Some("from org"), None, Some("org"), 3).unwrap();
        s.conn
            .execute_batch(
                "INSERT INTO task (id, title, prompt_title, created) VALUES ('ADHOC', 'hey so the', 'hey so the', 4);
                 INSERT INTO run (id, task, agent, native_id, relation, started) VALUES ('R1', 'ADHOC', 'pi', 'n', 'fresh', 5);
                 INSERT INTO event (run, kind, ts, raw) VALUES
                   ('R1', 'started', 5, '{}'),
                   ('R1', 'prompt', 6, '{\"prompt\": \"hey so the cart rounds wrong\"}'),
                   ('R1', 'prompt', 7, '{\"prompt\": \"later\"}');",
            )
            .unwrap();
        let deferred = defer(&s, "Agent's words", None, "R1", None, 8).unwrap();
        assert_eq!(unnamed(&s, 10).unwrap(), [deferred.as_str(), "ADHOC", ctx.as_str()]);
        assert_eq!(unnamed(&s, 1).unwrap(), [deferred.as_str()]);
        let got = claim_naming(&s, "ADHOC", 9).unwrap().unwrap();
        assert_eq!(
            got,
            ToName {
                title: "hey so the".into(),
                text: Some("hey so the cart rounds wrong".into()),
                agent: Some("pi".into())
            }
        );
        assert_eq!(claim_naming(&s, "ADHOC", 10).unwrap(), None, "once");
        let got = claim_naming(&s, &ctx, 11).unwrap().unwrap();
        assert_eq!((got.text.as_deref(), got.agent), (Some("why is the cart total off by a cent"), None));
        assert_eq!(claim_naming(&s, &typed, 12).unwrap(), None);
        assert_eq!(claim_naming(&s, &linked, 12).unwrap(), None);
        assert_eq!(claim_naming(&s, &deferred, 12).unwrap().unwrap().text, None, "nothing to name it from");
        assert!(unnamed(&s, 10).unwrap().is_empty());
        // Tried, a cleared title asks once more.
        set(&s, &ctx, Some(" "), None).unwrap();
        assert!(claim_naming(&s, &ctx, 13).unwrap().is_some());
    }

    #[test]
    fn an_edit_without_a_title_takes_one_from_the_context_and_can_be_named() {
        let (_d, s) = store();
        let id = create(&s, "Typed by hand", None, None, None, 1).unwrap();
        let changed = set(&s, &id, Some(" "), Some("the cart total rounds the wrong way")).unwrap();
        assert!(changed.title && changed.body);
        let d = detail(&s, &id, &|_, _| true).unwrap();
        assert_eq!(d.title, "the cart total rounds the wrong way");
        assert!(name(&s, &id, "Cart total rounding").unwrap(), "nameable again");
        // With neither, nothing is saved.
        let bare = create(&s, "Bare", None, None, None, 1).unwrap();
        assert!(set(&s, &bare, Some(""), Some(" ")).is_err());
        assert_eq!(detail(&s, &bare, &|_, _| true).unwrap().title, "Bare");
    }

    #[test]
    fn a_title_never_carries_control_characters() {
        let (_d, s) = store();
        let id = create(&s, "\x1b]0;PWNED\x07Fix\tit\n", None, None, None, 1).unwrap();
        assert_eq!(detail(&s, &id, &|_, _| true).unwrap().title, "]0;PWNEDFix it");
        set(&s, &id, Some("New\x1b[2J"), None).unwrap();
        assert_eq!(detail(&s, &id, &|_, _| true).unwrap().title, "New[2J");
    }
}
