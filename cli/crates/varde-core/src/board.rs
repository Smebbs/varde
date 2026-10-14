//! The board as data: rows already grouped, ordered and labelled, so no
//! client decides what is shown or in what order.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

use crate::status::Status;
use crate::store::{self, Store};

/// How far back an older run of a task is looked at for a live session.
const RECENT_MS: i64 = 7 * 24 * 3600 * 1000;

#[derive(Debug, Serialize)]
pub struct Board {
    pub v: u32,
    pub groups: Vec<Group>,
}

#[derive(Debug, Serialize)]
pub struct Group {
    pub key: &'static str,
    pub label: &'static str,
    /// Shown as its heading and count only, until someone opens it.
    pub collapsed: bool,
    pub rows: Vec<Row>,
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub task: String,
    /// What people type to refer to the task.
    pub short: String,
    pub title: String,
    pub project: Option<String>,
    /// `working`, `delegated`, `waiting`, `blocked`, `exited`, `open`,
    /// `done`, `parked` or `dropped`.
    pub status: &'static str,
    /// The task's own state: `open`, `done`, `parked` or `dropped`.
    pub state: String,
    /// Unix ms of the last change to the task's state, Inbox or link,
    /// however it was made.
    pub changed: i64,
    /// Sub-agents and background shells the session's last turn left
    /// running; 0 unless it is `delegated` or `waiting`.
    pub agents: u32,
    pub shells: u32,
    /// A live session of the task is at work: mid-turn (`working`,
    /// `delegated` or `blocked`), or waiting with sub-agents or background
    /// shells its last turn left running, which done or park would stop.
    pub busy: bool,
    /// In Needs you: its session finished a turn, or asked for a decision,
    /// that nobody has looked at yet. These come first.
    pub unread: bool,
    /// Arrived without you creating it, from a session or deferred from
    /// one, and not yet acted on.
    pub inbox: bool,
    /// Where the task lives elsewhere, e.g. `org`.
    pub external: Option<String>,
    /// The task this one was deferred from.
    pub origin: Option<Origin>,
    /// Unix ms of the latest thing that happened to the task.
    pub last: i64,
    /// Unix ms of when it was done, parked or dropped, while it is.
    pub closed: Option<i64>,
    /// Its cairn's Status line, when it has one; only with `all`, for a
    /// review: the board does without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cairn_status: Option<String>,
    pub run: Option<RunRef>,
    /// Where its git work stands, as last checked; `null` before the first
    /// check or outside a repository.
    pub git: Option<Git>,
    /// Whether its git work is checked: it has a worktree, or it is open
    /// in a project checkout and has been started.
    #[serde(skip)]
    pub tracked: bool,
    /// Whether it has a worktree of its own, whose work dropping it keeps
    /// under `refs/varde/discarded/`.
    #[serde(skip)]
    pub worktree: bool,
    /// When its git work was last checked, unix ms.
    #[serde(skip)]
    pub git_checked: Option<i64>,
    /// When varde starts it next, if it is scheduled.
    pub schedule: Option<Schedule>,
}

#[derive(Debug, Serialize)]
pub struct Schedule {
    /// As typed: `every weekday 09:00`, `at mon 09:00`, an org timestamp.
    pub rule: String,
    /// Unix ms of when it is next due.
    pub due: i64,
    /// `◷ Mon 09:00` once, `↻ Mon 09:00` again and again; with `late`
    /// when it should have started by now.
    pub label: String,
    pub repeats: bool,
    /// It is past due and has not started: no heartbeat ran it, or it
    /// failed to (`error`).
    pub late: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Git {
    /// `clean`, `edited`, `committed`, `pushed`, `pr` or `landed`.
    pub stage: String,
    /// What to show, e.g. `committed` or `PR #12 ✓`; empty when clean.
    pub label: String,
    /// The PR's checks: `passing`, `failing` or `pending`.
    pub checks: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Origin {
    pub task: String,
    pub short: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunRef {
    pub id: String,
    pub native_id: String,
    pub cwd: Option<String>,
}

const GROUPS: [(&str, &str); 8] = [
    ("needs_you", "Needs you"),
    ("working", "Working"),
    ("open", "Open"),
    // Open tasks with a schedule, until it starts them: soonest first.
    ("scheduled", "Scheduled"),
    ("inbox", "Inbox"),
    ("done", "Done"),
    ("parked", "Parked"),
    // Only with `all`.
    ("dropped", "Dropped"),
];

/// `now` and `day_start` are unix ms; `day_start` is local midnight.
/// `all` keeps every task, including old done ones and, in a group of
/// their own, dropped ones; a done task whose worktree is still there
/// stays until its work lands. Nothing else ever leaves the board by
/// itself. `alive` says whether an agent process
/// varde knows is still running.
pub fn build(
    store: &Store,
    now: i64,
    day_start: i64,
    all: bool,
    alive: &dyn Fn(i64, i64) -> bool,
) -> Result<Board> {
    // Old done tasks are most of a store, so they are left out here
    // rather than read and thrown away.
    let sql = "SELECT t.id, t.title, t.project, t.state, t.inbox, t.created, t.closed,
                r.id, r.native_id, r.cwd, r.started, r.pid, r.pid_start, t.external,
                EXISTS (SELECT 1 FROM worktree w WHERE w.task = t.id AND w.state = 'active'),
                g.stage, g.label, g.checks, g.checked, o.id, o.title,
                coalesce(t.changed, t.created), r.tty, r.tty_ctime,
                CASE WHEN ?1 THEN (SELECT nullif(status, '') FROM cairn WHERE task = t.id) END,
                s.rule, s.next_due, s.error, coalesce(r.background, 0)
         FROM task t
         LEFT JOIN task o ON o.id = t.origin
         LEFT JOIN schedule s ON s.task = t.id
         LEFT JOIN git_state g ON g.task = t.id
         LEFT JOIN run r ON r.id = (SELECT id FROM run WHERE task = t.id
                                    ORDER BY started DESC, rowid DESC LIMIT 1)
         WHERE (?1 OR t.state != 'dropped')
           AND (?1 OR t.state != 'done' OR coalesce(t.closed, 0) >= ?2
                OR EXISTS (SELECT 1 FROM worktree w WHERE w.task = t.id AND w.state = 'active'))";
    // A task can have more than one live run: a session varde did not
    // start keeps running beside the fork `start` made of it. The row
    // shows whichever run is most urgent, not just the newest.
    let (others, busy_older) = urgent_older_runs(store, now, alive)?;

    let mut stmt = store.conn.prepare_cached(sql)?;
    let mut rows = stmt.query(params![all, day_start])?;
    let mut groups: Vec<Group> = GROUPS
        .iter()
        .map(|&(key, label)| Group {
            key,
            label,
            collapsed: matches!(key, "parked" | "dropped"),
            rows: Vec::new(),
        })
        .collect();

    while let Some(r) = rows.next()? {
        let state: String = r.get(3)?;
        let inbox: bool = r.get(4)?;
        let created: i64 = r.get(5)?;
        let closed: Option<i64> = r.get(6)?;
        let run_id: Option<String> = r.get(7)?;
        let run_started: Option<i64> = r.get(10)?;
        let view = match &run_id {
            Some(id) => store::view(&store.conn, id)?,
            None => store::View::default(),
        };
        let last =
            [Some(created), closed, run_started, view.last].into_iter().flatten().max().unwrap_or(created);

        let task: String = r.get(0)?;
        let tty: Option<String> = r.get(22)?;
        let tty_ctime: Option<i64> = r.get(23)?;
        let tty = tty.as_deref().map(|t| (t, tty_ctime));
        let mut status = store::effective_status(view.status, r.get(11)?, r.get(12)?, tty, alive);
        let mut counts = (view.agents, view.shells);
        let mut unread = view.unread;
        // A run its schedule started in the background, ended and not
        // looked at: your turn, as a turn that ended in a terminal is.
        let background: bool = r.get(28)?;
        if background
            && status == Some(Status::Exited)
            && store::ended_unseen(&store.conn, run_id.as_deref().unwrap_or(""))?
        {
            status = Some(Status::Waiting);
            unread = true;
        }
        let mut run = match run_id {
            Some(id) => Some(RunRef { id, native_id: r.get(8)?, cwd: r.get(9)? }),
            None => None,
        };
        if let Some(other) = others.get(&task) {
            if urgency(Some(other.status)) < urgency(status) {
                status = Some(other.status);
                counts = other.counts;
                unread = other.unread;
                run = Some(other.run.clone());
            }
        }
        let busy = at_work(status, counts) || busy_older.contains(&task);
        let worktree: bool = r.get(14)?;
        let schedule = match r.get::<_, Option<String>>(25)? {
            Some(rule) if state == "open" => Some(schedule_of(rule, r.get(26)?, r.get(27)?, now)),
            _ => None,
        };
        let (group, label) = if state == "done" {
            (5, "done")
        } else if state == "parked" {
            (6, "parked")
        } else if state == "dropped" {
            (7, "dropped")
        } else {
            match status {
                Some(s) if s.needs_you() => (0, s.name()),
                Some(s @ (Status::Working | Status::Delegated)) => (1, s.name()),
                other => {
                    let label = if other == Some(Status::Exited) { "exited" } else { "open" };
                    (
                        if schedule.is_some() {
                            3
                        } else if inbox {
                            4
                        } else {
                            2
                        },
                        label,
                    )
                }
            }
        };
        let (agents, shells) = if group < 2 { counts } else { (0, 0) };
        let project: Option<String> = r.get(2)?;
        let tracked = worktree || (state == "open" && project.is_some() && run.is_some());
        let git = match r.get::<_, Option<String>>(15)? {
            Some(stage) if tracked => Some(Git { stage, label: r.get(16)?, checks: r.get(17)? }),
            _ => None,
        };
        let origin = match r.get::<_, Option<String>>(19)? {
            Some(task) => Some(Origin { short: crate::task::short(&task), task, title: r.get(20)? }),
            None => None,
        };
        groups[group].rows.push(Row {
            short: crate::task::short(&task),
            task,
            title: r.get(1)?,
            project,
            status: label,
            changed: r.get(21)?,
            state,
            agents,
            shells,
            busy,
            unread: unread && group == 0,
            inbox,
            external: r.get(13)?,
            origin,
            last,
            closed,
            cairn_status: r.get(24)?,
            run,
            git,
            tracked,
            worktree,
            git_checked: r.get(18)?,
            schedule,
        });
    }
    if !all {
        groups.pop();
    }
    for g in &mut groups {
        // Unread first, then newest first, with the task id breaking ties
        // so order is stable; what is scheduled, by when.
        if g.key == "scheduled" {
            g.rows.sort_by_key(|r| (r.schedule.as_ref().map_or(0, |s| s.due), r.task.clone()));
            continue;
        }
        g.rows.sort_by(|a, b| {
            b.unread.cmp(&a.unread).then(b.last.cmp(&a.last)).then_with(|| a.task.cmp(&b.task))
        });
    }
    Ok(Board { v: 2, groups })
}

/// Past due by this much, a schedule should have started: the heartbeat
/// runs every minute.
const LATE_MS: i64 = 5 * 60 * 1000;

fn schedule_of(rule: String, due: i64, error: Option<String>, now: i64) -> Schedule {
    use crate::schedule;
    let repeats = schedule::parse(&rule, schedule::from_ms(now)).is_ok_and(|r| r.repeats());
    let late = error.is_some() || due < now - LATE_MS;
    let when = schedule::when(schedule::from_ms(due), schedule::from_ms(now));
    let label = format!("{} {when}{}", if repeats { "↻" } else { "◷" }, if late { " late" } else { "" });
    Schedule { rule, due, label, repeats, late, error }
}

/// What a board kept to one project leaves out.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Elsewhere {
    pub tasks: usize,
    /// Of those, the ones in Needs you.
    pub need_you: usize,
}

impl Board {
    /// Whether any row is in `project`.
    pub fn has(&self, project: &str) -> bool {
        self.groups.iter().flat_map(|g| &g.rows).any(|r| r.project.as_deref() == Some(project))
    }

    /// Keeps only the rows of `project`, and says what went.
    pub fn only(&mut self, project: &str) -> Elsewhere {
        let mut gone = Elsewhere::default();
        for g in &mut self.groups {
            let before = g.rows.len();
            g.rows.retain(|r| r.project.as_deref() == Some(project));
            gone.tasks += before - g.rows.len();
            if g.key == "needs_you" {
                gone.need_you += before - g.rows.len();
            }
        }
        gone
    }
}

/// Lower is more urgent: needing you, then working, then anything else.
fn urgency(status: Option<Status>) -> u8 {
    match status {
        Some(s) if s.needs_you() => 0,
        Some(Status::Working | Status::Delegated) => 1,
        _ => 2,
    }
}

/// Whether a session is at work: mid-turn, or waiting with what its last
/// turn left running in the background.
fn at_work(status: Option<Status>, (agents, shells): (u32, u32)) -> bool {
    match status {
        Some(Status::Waiting) => agents > 0 || shells > 0,
        Some(s) => s.mid_turn(),
        None => false,
    }
}

struct Other {
    status: Status,
    counts: (u32, u32),
    unread: bool,
    run: RunRef,
}

/// For each open task, its most urgent live run other than the newest,
/// among runs active in the last week; and the tasks one of those runs
/// is mid-turn in.
#[allow(clippy::type_complexity)]
fn urgent_older_runs(
    store: &Store,
    now: i64,
    alive: &dyn Fn(i64, i64) -> bool,
) -> Result<(std::collections::HashMap<String, Other>, std::collections::HashSet<String>)> {
    // From the few open tasks to their runs (CROSS JOIN keeps that order),
    // and a run's newest event straight from the index.
    let mut stmt = store.conn.prepare_cached(
        "SELECT r.task, r.id, r.native_id, r.cwd, r.pid, r.pid_start, r.tty, r.tty_ctime
         FROM task t CROSS JOIN run r ON r.task = t.id
         WHERE t.state = 'open'
           AND r.id != (SELECT id FROM run WHERE task = t.id ORDER BY started DESC, rowid DESC LIMIT 1)
           AND (SELECT ts FROM event WHERE run = r.id ORDER BY ts DESC LIMIT 1) > ?1",
    )?;
    let mut rows = stmt.query([now - RECENT_MS])?;
    let mut out: std::collections::HashMap<String, Other> = Default::default();
    let mut busy = std::collections::HashSet::new();
    while let Some(r) = rows.next()? {
        let id: String = r.get(1)?;
        let view = store::view(&store.conn, &id)?;
        if view.last.is_none_or(|t| t <= now - RECENT_MS) {
            continue;
        }
        let tty: Option<String> = r.get(6)?;
        let tty_ctime: Option<i64> = r.get(7)?;
        let tty = tty.as_deref().map(|t| (t, tty_ctime));
        let status = store::effective_status(view.status, r.get(4)?, r.get(5)?, tty, alive);
        let Some(status) = status.filter(|s| urgency(Some(*s)) < 2) else { continue };
        let task: String = r.get(0)?;
        if at_work(Some(status), (view.agents, view.shells)) {
            busy.insert(task.clone());
        }
        if out.get(&task).is_none_or(|o| urgency(Some(status)) < urgency(Some(o.status))) {
            let run = RunRef { id, native_id: r.get(2)?, cwd: r.get(3)? };
            out.insert(task, Other { status, counts: (view.agents, view.shells), unread: view.unread, run });
        }
    }
    Ok((out, busy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Kind, Norm, Source};
    use crate::store::temp as store;

    fn ingest(s: &mut Store, id: &str, kind: Kind, prompt: Option<&str>, now: i64) {
        let n = Norm {
            native_id: id.into(),
            kind,
            cwd: Some("/tmp/p".into()),
            transcript: None,
            prompt: prompt.map(Into::into),
        };
        s.ingest("claude", &n, "{}", now, None).unwrap();
    }

    fn keys(b: &Board) -> Vec<(&str, Vec<&str>)> {
        b.groups.iter().map(|g| (g.key, g.rows.iter().map(|r| r.title.as_str()).collect())).collect()
    }

    #[test]
    fn every_group_is_present_even_when_empty() {
        let (_d, s) = store();
        let b = build(&s, 0, 0, false, &|_, _| true).unwrap();
        assert_eq!(
            b.groups.iter().map(|g| g.key).collect::<Vec<_>>(),
            ["needs_you", "working", "open", "scheduled", "inbox", "done", "parked"]
        );
        assert!(b.groups.iter().all(|g| g.collapsed == (g.key == "parked")));
    }

    #[test]
    fn all_shows_dropped_tasks_and_every_row_says_when_its_state_changed() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("thrown away"), 10);
        ingest(&mut s, "b", Kind::Prompt, Some("kept"), 11);
        let id = |title: &str| -> String {
            s.conn.query_row("SELECT id FROM task WHERE title = ?1", [title], |r| r.get(0)).unwrap()
        };
        let row = |b: &Board, title: &str| -> (String, String, i64) {
            let r = b.groups.iter().flat_map(|g| &g.rows).find(|r| r.title == title).unwrap();
            (r.state.clone(), r.status.to_string(), r.changed)
        };
        let created = row(&build(&s, 20, 0, false, &|_, _| true).unwrap(), "kept").2;
        assert_eq!(created, 11);
        s.conn.execute("UPDATE task SET state = 'dropped' WHERE id = ?1", [id("thrown away")]).unwrap();
        let b = build(&s, 20, 0, false, &|_, _| true).unwrap();
        assert!(b.groups.iter().flat_map(|g| &g.rows).all(|r| r.title != "thrown away"));
        assert_eq!(b.groups.len(), 7);
        let b = build(&s, 20, 0, true, &|_, _| true).unwrap();
        assert_eq!(keys(&b)[7], ("dropped", vec!["thrown away"]));
        assert!(b.groups[7].collapsed);
        let (state, status, changed) = row(&b, "thrown away");
        assert_eq!((state.as_str(), status.as_str()), ("dropped", "dropped"));
        // The real clock: the change happened just now.
        assert!(changed > 1_700_000_000_000);
        // Reopening moves it too, though it clears `closed`.
        s.conn.execute("UPDATE task SET state = 'done', closed = 30 WHERE id = ?1", [id("kept")]).unwrap();
        let done = row(&build(&s, 40, 0, true, &|_, _| true).unwrap(), "kept").2;
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.conn.execute("UPDATE task SET state = 'open', closed = NULL WHERE id = ?1", [id("kept")]).unwrap();
        assert!(row(&build(&s, 40, 0, true, &|_, _| true).unwrap(), "kept").2 > done);
    }

    #[test]
    fn sessions_land_by_status() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("alpha"), 10);
        ingest(&mut s, "b", Kind::Prompt, Some("beta"), 11);
        ingest(&mut s, "b", Kind::TurnEnd { agents: 0, shells: 0 }, None, 12);
        ingest(&mut s, "c", Kind::Prompt, Some("gamma"), 13);
        ingest(&mut s, "c", Kind::Ended, None, 14);
        ingest(&mut s, "d", Kind::Started(Source::Startup), None, 15);
        let b = build(&s, 20, 0, false, &|_, _| true).unwrap();
        assert_eq!(
            keys(&b),
            [
                ("needs_you", vec!["beta"]),
                ("working", vec!["alpha"]),
                ("open", vec![]),
                ("scheduled", vec![]),
                ("inbox", vec!["gamma"]),
                ("done", vec![]),
                ("parked", vec![]),
            ]
        );
    }

    #[test]
    fn a_session_waiting_on_its_sub_agents_is_working_and_counts_them() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("alpha"), 10);
        ingest(&mut s, "a", Kind::TurnEnd { agents: 2, shells: 1 }, None, 11);
        ingest(&mut s, "b", Kind::Prompt, Some("beta"), 12);
        ingest(&mut s, "b", Kind::TurnEnd { agents: 0, shells: 1 }, None, 13);
        let b = build(&s, 20, 0, false, &|_, _| true).unwrap();
        let rows = |key| {
            b.groups
                .iter()
                .find(|g| g.key == key)
                .unwrap()
                .rows
                .iter()
                .map(|r| (r.status, r.agents, r.shells))
                .collect::<Vec<_>>()
        };
        assert_eq!(rows("working"), [("delegated", 2, 1)]);
        assert_eq!(rows("needs_you"), [("waiting", 0, 1)]);
    }

    #[test]
    fn what_nobody_has_looked_at_comes_first_until_seen() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("alpha"), 10);
        ingest(&mut s, "a", Kind::TurnEnd { agents: 0, shells: 0 }, None, 11);
        ingest(&mut s, "b", Kind::Prompt, Some("beta"), 12);
        ingest(&mut s, "b", Kind::Blocked, None, 13);
        ingest(&mut s, "c", Kind::Started(Source::Startup), None, 14);
        let id = |title: &str| -> String {
            s.conn.query_row("SELECT id FROM task WHERE title = ?1", [title], |r| r.get(0)).unwrap()
        };
        let needs_you = |s: &Store| -> Vec<(String, bool, i64)> {
            build(s, 30, 0, false, &|_, _| true).unwrap().groups[0]
                .rows
                .iter()
                .map(|r| (r.title.clone(), r.unread, r.last))
                .collect()
        };
        assert_eq!(needs_you(&s), [("beta".into(), true, 13), ("alpha".into(), true, 11)]);

        // Looking does not make a task newer.
        assert_eq!(store::mark_seen(&s.conn, &id("beta"), 20).unwrap(), 1);
        assert_eq!(store::mark_seen(&s.conn, &id("beta"), 21).unwrap(), 0);
        assert_eq!(needs_you(&s), [("alpha".into(), true, 11), ("beta".into(), false, 13)]);

        // The next thing it asks is new again.
        ingest(&mut s, "b", Kind::ToolRan, None, 22);
        ingest(&mut s, "b", Kind::TurnEnd { agents: 0, shells: 0 }, None, 23);
        assert_eq!(needs_you(&s)[0], ("beta".into(), true, 23));
    }

    #[test]
    fn a_session_you_started_or_interrupted_is_not_unread() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("alpha"), 10);
        ingest(&mut s, "a", Kind::TurnEnd { agents: 0, shells: 0 }, None, 11);
        ingest(&mut s, "a", Kind::Started(Source::Resume), None, 12);
        let row = |s: &Store| {
            let b = build(s, 30, 0, false, &|_, _| true).unwrap();
            (b.groups[0].rows[0].status, b.groups[0].rows[0].unread)
        };
        assert_eq!(row(&s), ("waiting", false));
        let run: String =
            s.conn.query_row("SELECT id FROM run ORDER BY started DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        s.record_interrupt(&run, 13).unwrap();
        assert_eq!(row(&s), ("waiting", false));
    }

    #[test]
    fn a_parked_task_stays_parked_whatever_its_session_does() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("someday"), 10);
        ingest(&mut s, "a", Kind::Blocked, None, 11);
        s.conn.execute("UPDATE task SET state='parked', closed=12, inbox=0", []).unwrap();
        // Old as it is, it is not aged off the board the way done tasks are.
        let now = 12 + RECENT_MS + 1;
        let b = build(&s, now, now - 1000, false, &|_, _| true).unwrap();
        let parked = b.groups.iter().find(|g| g.key == "parked").unwrap();
        assert_eq!(
            parked.rows.iter().map(|r| (r.title.as_str(), r.status)).collect::<Vec<_>>(),
            [("someday", "parked")]
        );
        assert_eq!(b.groups.iter().map(|g| g.rows.len()).sum::<usize>(), 1);
    }

    #[test]
    fn a_row_is_busy_while_any_of_its_sessions_is_at_work() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("working"), 10);
        ingest(&mut s, "b", Kind::Prompt, Some("blocked"), 10);
        ingest(&mut s, "b", Kind::Blocked, None, 11);
        ingest(&mut s, "c", Kind::Prompt, Some("waiting"), 10);
        ingest(&mut s, "c", Kind::TurnEnd { agents: 0, shells: 0 }, None, 11);
        // Waiting, but a server it started still runs.
        ingest(&mut s, "e", Kind::Prompt, Some("shell left"), 10);
        ingest(&mut s, "e", Kind::TurnEnd { agents: 0, shells: 1 }, None, 11);
        ingest(&mut s, "d", Kind::Prompt, Some("two sessions"), 10);
        // A newer run of it waits; the older one is still at work.
        let task: String =
            s.conn.query_row("SELECT id FROM task WHERE title = 'two sessions'", [], |r| r.get(0)).unwrap();
        s.conn
            .execute(
                "INSERT INTO run (id, task, agent, native_id, relation, started, launch)
                 VALUES ('F', ?1, 'claude', 'f', 'fork', 12, 'managed')",
                [&task],
            )
            .unwrap();
        ingest(&mut s, "f", Kind::Prompt, None, 13);
        ingest(&mut s, "f", Kind::TurnEnd { agents: 0, shells: 0 }, None, 14);
        let b = build(&s, 20, 0, false, &|_, _| true).unwrap();
        let mut busy: Vec<(&str, &str, bool)> =
            b.groups.iter().flat_map(|g| &g.rows).map(|r| (r.title.as_str(), r.status, r.busy)).collect();
        busy.sort();
        assert_eq!(
            busy,
            [
                ("blocked", "blocked", true),
                ("shell left", "waiting", true),
                ("two sessions", "waiting", true),
                ("waiting", "waiting", false),
                ("working", "working", true)
            ]
        );
    }

    #[test]
    fn a_live_run_needing_you_is_not_hidden_by_a_newer_one() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("alpha"), 10);
        ingest(&mut s, "a", Kind::Blocked, None, 11);
        // `start` forks it: a newer run of the same task that never got going.
        let task: String = s.conn.query_row("SELECT id FROM task", [], |r| r.get(0)).unwrap();
        s.conn
            .execute(
                "INSERT INTO run (id, task, agent, native_id, relation, started, launch)
                 VALUES ('F', ?1, 'claude', 'f', 'fork', 12, 'managed')",
                [&task],
            )
            .unwrap();
        let b = build(&s, 20, 0, false, &|_, _| true).unwrap();
        assert_eq!(keys(&b)[0], ("needs_you", vec!["alpha"]));
        assert_eq!(b.groups[0].rows[0].status, "blocked");
    }

    #[test]
    fn a_done_task_stays_while_its_worktree_does() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("landing"), 1);
        ingest(&mut s, "b", Kind::Prompt, Some("landed"), 2);
        s.conn.execute("UPDATE task SET state='done', closed=3, inbox=0", []).unwrap();
        let id = |title: &str| -> String {
            s.conn.query_row("SELECT id FROM task WHERE title = ?1", [title], |r| r.get(0)).unwrap()
        };
        let (landing, landed) = (id("landing"), id("landed"));
        s.conn
            .execute(
                "INSERT INTO worktree (path, task, repo, branch, base, state, created)
                 VALUES ('/w/a', ?1, '/r', 'varde/a', 'main', 'active', 1),
                        ('/w/b', ?2, '/r', 'varde/b', 'main', 'merged', 1)",
                [&landing, &landed],
            )
            .unwrap();
        s.conn
            .execute(
                "INSERT INTO git_state (task, stage, label, checks, facts, checked)
                 VALUES (?1, 'pr', 'PR #7 ✓', 'passing', '{}', 5), (?2, 'landed', 'landed', NULL, '{}', 5)",
                [&landing, &landed],
            )
            .unwrap();
        let now = 3 + RECENT_MS;
        let b = build(&s, now, now - 1000, false, &|_, _| true).unwrap();
        let done = b.groups.iter().find(|g| g.key == "done").unwrap();
        assert_eq!(done.rows.len(), 1);
        let row = &done.rows[0];
        assert_eq!(row.title, "landing");
        assert!(row.tracked);
        // When it closed, and its cairn's Status, for a review.
        assert_eq!((row.closed, row.cairn_status.as_deref()), (Some(3), None));
        s.conn
            .execute(
                "INSERT INTO cairn (task, version, updated, status) VALUES (?1, 1, 3, 'PR open, waiting on review.')",
                [&landing],
            )
            .unwrap();
        let b = build(&s, now, now - 1000, false, &|_, _| true).unwrap();
        assert_eq!(b.groups.iter().find(|g| g.key == "done").unwrap().rows[0].cairn_status, None);
        let b = build(&s, now, now - 1000, true, &|_, _| true).unwrap();
        // Both closed at 3: with --all, "landed" is there too, in either order.
        let done = b.groups.iter().find(|g| g.key == "done").unwrap();
        let row = done.rows.iter().find(|r| r.title == "landing").unwrap();
        assert_eq!(row.cairn_status.as_deref(), Some("PR open, waiting on review."));
        let git = row.git.as_ref().unwrap();
        assert_eq!(
            (git.stage.as_str(), git.label.as_str(), git.checks.as_deref()),
            ("pr", "PR #7 ✓", Some("passing"))
        );
    }

    #[test]
    fn only_old_done_tasks_leave_the_board() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("old"), 1);
        ingest(&mut s, "a", Kind::Ended, None, 2);
        ingest(&mut s, "b", Kind::Prompt, Some("finished"), 3);
        s.conn.execute("UPDATE task SET state='done', closed=4 WHERE title='finished'", []).unwrap();
        let now = 2 + 365 * 24 * 3600 * 1000;
        let b = build(&s, now, now - 1000, false, &|_, _| true).unwrap();
        assert_eq!(keys(&b)[4], ("inbox", vec!["old"]));
        assert_eq!(b.groups.iter().map(|g| g.rows.len()).sum::<usize>(), 1);
        let b = build(&s, now, now - 1000, true, &|_, _| true).unwrap();
        assert_eq!(b.groups.iter().map(|g| g.rows.len()).sum::<usize>(), 2);
    }

    #[test]
    fn an_accepted_session_moves_from_the_inbox_to_open() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("keeper"), 1);
        ingest(&mut s, "a", Kind::Ended, None, 2);
        let id: String = s.conn.query_row("SELECT id FROM task", [], |r| r.get(0)).unwrap();
        crate::task::accept(&s, &id).unwrap();
        let b = build(&s, 10, 0, false, &|_, _| true).unwrap();
        assert_eq!(keys(&b)[2], ("open", vec!["keeper"]));
        assert!(!b.groups[2].rows[0].inbox);
    }

    #[test]
    fn a_deferred_task_is_in_the_inbox_and_names_its_origin() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("big job"), 1);
        let run: String = s.conn.query_row("SELECT id FROM run", [], |r| r.get(0)).unwrap();
        let origin: String = s.conn.query_row("SELECT id FROM task", [], |r| r.get(0)).unwrap();
        crate::task::defer(&s, "later bit", None, &run, None, 2).unwrap();
        let b = build(&s, 3, 0, false, &|_, _| true).unwrap();
        let row = &b.groups[4].rows.iter().find(|r| r.title == "later bit").unwrap();
        let o = row.origin.as_ref().unwrap();
        assert_eq!((o.task.as_str(), o.title.as_str()), (origin.as_str(), "big job"));
        assert!(row.run.is_none());
        assert_eq!(row.status, "open");
    }

    #[test]
    fn a_task_never_started_does_not_show_its_project_checkout() {
        let (_d, mut s) = store();
        ingest(&mut s, "a", Kind::Prompt, Some("started"), 1);
        let run: String = s.conn.query_row("SELECT id FROM run", [], |r| r.get(0)).unwrap();
        crate::task::defer(&s, "not started", None, &run, None, 2).unwrap();
        s.conn.execute("UPDATE task SET project = '/p'", []).unwrap();
        s.conn
            .execute(
                "INSERT INTO git_state (task, stage, label, checks, facts, checked)
                 SELECT id, 'committed', 'committed', NULL, '{}', 3 FROM task",
                [],
            )
            .unwrap();
        let b = build(&s, 3, 0, false, &|_, _| true).unwrap();
        let row = |title: &str| b.groups.iter().flat_map(|g| &g.rows).find(|r| r.title == title).unwrap();
        assert!(row("started").tracked);
        assert!(row("started").git.is_some());
        assert!(!row("not started").tracked);
        assert!(row("not started").git.is_none());
    }

    #[test]
    fn a_board_kept_to_one_project_counts_what_it_leaves_out() {
        let (_d, mut s) = store();
        crate::task::create(&s, "here", None, Some("/a"), None, 1).unwrap();
        crate::task::create(&s, "nowhere", None, None, None, 2).unwrap();
        ingest(&mut s, "b", Kind::Prompt, Some("waiting"), 3);
        ingest(&mut s, "b", Kind::TurnEnd { agents: 0, shells: 0 }, None, 4);
        let mut b = build(&s, 5, 0, false, &|_, _| true).unwrap();
        assert!(b.has("/a") && b.has("/tmp/p") && !b.has("/c"));
        assert_eq!(b.only("/a"), Elsewhere { tasks: 2, need_you: 1 });
        assert_eq!(keys(&b)[2], ("open", vec!["here"]));
    }
}
