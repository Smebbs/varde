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

pub fn create(
    store: &Store,
    title: &str,
    body: Option<&str>,
    project: Option<&str>,
    external: Option<&str>,
    now: i64,
) -> Result<String> {
    let title = need_title(title)?;
    let id = ulid::Ulid::new().to_string();
    store.conn.execute(
        "INSERT INTO task (id, title, body, project, external, created)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, title, body, project, external, now],
    )?;
    Ok(id)
}

/// Split work off the task of run `from` into a task of its own, in
/// `project`, else in the same project. It waits in the Inbox until
/// someone picks it up or drops it.
pub fn defer(
    store: &Store,
    title: &str,
    body: Option<&str>,
    from: &str,
    project: Option<&str>,
    now: i64,
) -> Result<String> {
    let title = need_title(title)?;
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
        "INSERT INTO task (id, title, body, project, inbox, origin, created)
         VALUES (?1,?2,?3,?4,1,?5,?6)",
        params![id, title, body, project, origin, now],
    )?;
    Ok(id)
}

/// Refresh a task's title and body from where it is written, e.g. an org
/// heading. The task's identity never depends on them.
pub fn set(store: &Store, id: &str, title: Option<&str>, body: Option<&str>) -> Result<()> {
    if let Some(t) = title {
        let t = need_title(t)?;
        store.conn.execute("UPDATE task SET title = ?2 WHERE id = ?1", params![id, t])?;
        // Recall finds a task by its title too.
        crate::memory::reindex(&store.conn, id)?;
    }
    if let Some(b) = body {
        let b = b.trim();
        store
            .conn
            .execute("UPDATE task SET body = ?2 WHERE id = ?1", params![id, (!b.is_empty()).then_some(b)])?;
    }
    Ok(())
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
    /// The task this one was deferred from.
    pub origin: Option<String>,
    pub created: i64,
    pub closed: Option<i64>,
    /// When its state, Inbox or link last changed, unix ms.
    pub changed: i64,
    pub runs: Vec<RunDetail>,
    pub worktree: Option<WorktreeDetail>,
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
                coalesce(changed, created)
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
                runs: Vec::new(),
                worktree: None,
            })
        },
    )?;
    let mut stmt = store.conn.prepare_cached(
        "SELECT id, native_id, relation, parent_run, launch, cwd, started, exit_code, pid, pid_start,
                coalesce(holdout, 0), tty, tty_ctime
         FROM run WHERE task = ?1 ORDER BY started, rowid",
    )?;
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
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::temp as store;

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
