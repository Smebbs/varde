//! Loops and trails (`docs/proposal-loops-and-graphs.md`): what varde
//! knows of a task's check, loop and edges, and what follows from them.
//! Everything here is derived from stored facts, never stored itself: a
//! loop's round and state, whether a task is verified, and whether a task
//! in a trail is ready.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// What a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    /// It could not run: a missing command, a timeout, no checkout.
    Error,
    /// The files changed while it ran, so it says nothing.
    Unchecked,
    /// A round touched a path the guard keeps.
    Flagged,
}

impl Verdict {
    pub fn parse(s: &str) -> Verdict {
        match s {
            "pass" => Verdict::Pass,
            "fail" => Verdict::Fail,
            "unchecked" => Verdict::Unchecked,
            "flagged" => Verdict::Flagged,
            _ => Verdict::Error,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Error => "error",
            Verdict::Unchecked => "unchecked",
            Verdict::Flagged => "flagged",
        }
    }
}

/// One check varde ran.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: i64,
    pub run: Option<String>,
    pub started: i64,
    pub finished: i64,
    pub verdict: Verdict,
    pub tree: Option<String>,
    pub fingerprint: Option<String>,
    /// One line: `exit 101`, `touched tests/a.rs`, the review's verdict.
    pub summary: Option<String>,
    /// The file holding its output.
    pub output: Option<String>,
}

/// One round of a loop: a run the loop started.
#[derive(Debug, Clone, Serialize)]
pub struct Round {
    pub run: String,
    pub n: i64,
    pub started: i64,
    /// When it ended, if it has.
    pub ended: Option<i64>,
    pub exit_code: Option<i32>,
    /// Its agent process is still there, or it is being launched.
    pub live: bool,
    /// Its last words were a question to you.
    pub asked: bool,
    pub profile: Option<String>,
}

/// A loop as set.
#[derive(Debug, Clone, Serialize)]
pub struct LoopDef {
    pub max: i64,
    pub minutes: Option<i64>,
    pub escalate: Vec<String>,
    pub started: i64,
    pub paused: bool,
}

/// Where a loop stands. The first that applies wins, in the order the
/// proposal gives: flagged, error, verified, spent, stuck, asked, then
/// at work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum State {
    Paused {
        rounds: i64,
    },
    /// A round is at work.
    Working {
        round: i64,
    },
    /// A round ended and its check is due or running.
    Checking {
        round: i64,
        run: String,
    },
    /// The next round should start, on `profile`.
    Due {
        round: i64,
        profile: Option<String>,
    },
    Verified {
        rounds: i64,
    },
    Flagged {
        rounds: i64,
        why: String,
    },
    Error {
        rounds: i64,
        why: String,
    },
    Spent {
        rounds: i64,
        why: String,
    },
    Stuck {
        rounds: i64,
    },
    Asked {
        rounds: i64,
    },
}

impl State {
    /// Whether varde still has something to do: start a round, or check one.
    pub fn active(&self) -> bool {
        matches!(self, State::Working { .. } | State::Checking { .. } | State::Due { .. })
    }

    pub fn name(&self) -> &'static str {
        match self {
            State::Paused { .. } => "paused",
            State::Working { .. } => "working",
            State::Checking { .. } => "checking",
            State::Due { .. } => "due",
            State::Verified { .. } => "verified",
            State::Flagged { .. } => "flagged",
            State::Error { .. } => "error",
            State::Spent { .. } => "spent",
            State::Stuck { .. } => "stuck",
            State::Asked { .. } => "asked",
        }
    }
}

/// With nothing but a model's review to decide, a loop gets this many
/// rounds and then ends with you.
pub const REVIEW_ONLY_MAX: i64 = 2;

/// Where a loop stands, from its rounds (oldest first) and the checks
/// run since it was set (oldest first). `base` is the profile a round
/// runs on before any escalation; `review_only` says no command decides.
pub fn derive(
    def: &LoopDef,
    base: Option<&str>,
    review_only: bool,
    rounds: &[Round],
    checks: &[Check],
) -> State {
    let max = if review_only { def.max.min(REVIEW_ONLY_MAX) } else { def.max };
    let done = rounds.len() as i64;
    let Some(last) = rounds.last() else {
        return if def.paused {
            State::Paused { rounds: 0 }
        } else {
            State::Due { round: 1, profile: base.map(String::from) }
        };
    };
    // The check that speaks for a round: its own, or one run since it
    // started (`varde check --now` after it stopped).
    let of_a_round = |c: &Check| rounds.iter().any(|r| c.run.as_deref() == Some(&r.run));
    let check_of = |r: &Round| -> Option<&Check> {
        checks
            .iter()
            .rev()
            .find(|c| c.run.as_deref() == Some(&r.run))
            .or_else(|| checks.iter().rev().find(|c| !of_a_round(c) && c.started >= r.started))
    };
    if last.live {
        return State::Working { round: last.n };
    }
    let Some(c) = check_of(last) else {
        return State::Checking { round: last.n, run: last.run.clone() };
    };
    match c.verdict {
        Verdict::Flagged => {
            return State::Flagged { rounds: done, why: c.summary.clone().unwrap_or_default() };
        }
        Verdict::Error => {
            return State::Error {
                rounds: done,
                why: c.summary.clone().unwrap_or_else(|| "the check could not run".into()),
            };
        }
        Verdict::Pass => return State::Verified { rounds: done },
        Verdict::Unchecked => return State::Checking { round: last.n, run: last.run.clone() },
        Verdict::Fail => {}
    }
    let failed = |r: &Round| r.exit_code.is_some_and(|c| c != 0);
    if rounds.len() >= 2 && failed(last) && failed(&rounds[rounds.len() - 2]) {
        return State::Error { rounds: done, why: "the agent failed two rounds in a row".into() };
    }
    if done >= max {
        return State::Spent { rounds: done, why: format!("{done} of {max} rounds") };
    }
    if let Some(limit) = def.minutes {
        let used: i64 = rounds.iter().map(|r| r.ended.unwrap_or(r.started) - r.started).sum();
        if used >= limit * 60_000 {
            return State::Spent { rounds: done, why: format!("{} of {limit} minutes", used / 60_000) };
        }
    }
    // Stuck: a round that failed as the one before it did, on the same
    // files. Each time moves the loop to the next profile it escalates to.
    let mut stuck = 0usize;
    let mut now_stuck = false;
    for pair in rounds.windows(2) {
        let (a, b) = (check_of(&pair[0]), check_of(&pair[1]));
        let same = match (a, b) {
            (Some(a), Some(b)) => {
                a.verdict == Verdict::Fail
                    && b.verdict == Verdict::Fail
                    && a.fingerprint.is_some()
                    && a.fingerprint == b.fingerprint
                    && a.tree == b.tree
            }
            _ => false,
        };
        if same {
            stuck += 1;
        }
        now_stuck = same;
    }
    if now_stuck && stuck > def.escalate.len() {
        return State::Stuck { rounds: done };
    }
    if last.asked {
        return State::Asked { rounds: done };
    }
    if def.paused {
        return State::Paused { rounds: done };
    }
    let profile = match stuck {
        0 => base.map(String::from),
        n => def.escalate.get(n - 1).cloned().or_else(|| base.map(String::from)),
    };
    State::Due { round: last.n + 1, profile }
}

/// The checks of a task, oldest first, finished since `since`.
pub fn checks(conn: &Connection, task: &str, since: i64) -> Result<Vec<Check>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, run, started, finished, verdict, tree, fingerprint, summary, output
         FROM check_result WHERE task = ?1 AND finished >= ?2 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![task, since], |r| {
        Ok(Check {
            id: r.get(0)?,
            run: r.get(1)?,
            started: r.get(2)?,
            finished: r.get(3)?,
            verdict: Verdict::parse(&r.get::<_, String>(4)?),
            tree: r.get(5)?,
            fingerprint: r.get(6)?,
            summary: r.get(7)?,
            output: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A task's newest check.
pub fn last_check(conn: &Connection, task: &str) -> Result<Option<Check>> {
    Ok(conn
        .query_row("SELECT max(id) FROM check_result WHERE task = ?1", [task], |r| {
            r.get::<_, Option<i64>>(0)
        })?
        .map(|id| -> Result<Option<Check>> { Ok(checks(conn, task, 0)?.into_iter().find(|c| c.id == id)) })
        .transpose()?
        .flatten())
}

/// A task's loop, if it has one.
pub fn loop_of(conn: &Connection, task: &str) -> Result<Option<LoopDef>> {
    Ok(conn
        .query_row("SELECT max, minutes, escalate, started, paused FROM loop WHERE task = ?1", [task], |r| {
            Ok(LoopDef {
                max: r.get(0)?,
                minutes: r.get(1)?,
                escalate: r
                    .get::<_, Option<String>>(2)?
                    .and_then(|e| serde_json::from_str(&e).ok())
                    .unwrap_or_default(),
                started: r.get(3)?,
                paused: r.get(4)?,
            })
        })
        .optional()?)
}

/// The rounds of a task's loop since `since`, oldest first. `alive`
/// says whether an agent process is still running (pid, start time).
pub fn rounds(
    conn: &Connection,
    task: &str,
    since: i64,
    now: i64,
    alive: &dyn Fn(i64, i64) -> bool,
) -> Result<Vec<Round>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.id, r.round, r.started, r.exit_code, r.pid, r.pid_start, coalesce(r.asked, 0), r.profile,
                (SELECT max(ts) FROM event e WHERE e.run = r.id AND e.kind = 'ended')
         FROM run r WHERE r.task = ?1 AND r.round > 0 AND r.started >= ?2 ORDER BY r.started, r.rowid",
    )?;
    let rows = stmt.query_map(params![task, since], |r| {
        let exit: Option<i32> = r.get(3)?;
        let pid: Option<i64> = r.get(4)?;
        let started: i64 = r.get(2)?;
        let live = exit.is_none()
            && match pid {
                // Recorded and not launched yet, or launching.
                None | Some(0) => now - started < LAUNCHING_MS,
                Some(p) => alive(p, r.get::<_, Option<i64>>(5)?.unwrap_or(0)),
            };
        Ok(Round {
            run: r.get(0)?,
            n: r.get(1)?,
            started,
            ended: r.get::<_, Option<i64>>(8)?.or(exit.map(|_| started)),
            exit_code: exit,
            live,
            asked: r.get(6)?,
            profile: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A run recorded this long ago and never launched is not coming.
const LAUNCHING_MS: i64 = 2 * 60 * 1000;

/// Whether the task's check is a model's review alone.
pub fn review_only(conn: &Connection, task: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT cmd IS NULL AND review FROM check_def WHERE task = ?1", [task], |r| r.get(0))
        .optional()?
        .unwrap_or(false))
}

/// Where a task's loop stands, if it has one.
pub fn state(
    conn: &Connection,
    task: &str,
    now: i64,
    alive: &dyn Fn(i64, i64) -> bool,
) -> Result<Option<State>> {
    let Some(def) = loop_of(conn, task)? else { return Ok(None) };
    let base: Option<String> =
        conn.query_row("SELECT profile FROM task WHERE id = ?1", [task], |r| r.get(0))?;
    let rounds = rounds(conn, task, def.started, now, alive)?;
    let checks = checks(conn, task, def.started)?;
    Ok(Some(derive(&def, base.as_deref(), review_only(conn, task)?, &rounds, &checks)))
}

/// The tree a task's checkout had when the git refresh or a check last
/// looked, if varde knows it.
pub fn tree_now(conn: &Connection, task: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT tree FROM git_state WHERE task = ?1", [task], |r| r.get(0))
        .optional()?
        .flatten())
}

/// Whether you approved the task's work, and it has not changed since.
pub fn approved(conn: &Connection, task: &str) -> Result<bool> {
    let tree: Option<Option<String>> = conn
        .query_row("SELECT tree FROM approval WHERE task = ?1 ORDER BY id DESC LIMIT 1", [task], |r| r.get(0))
        .optional()?;
    let Some(tree) = tree else { return Ok(false) };
    Ok(match (tree, tree_now(conn, task)?) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    })
}

/// Whether varde knows the task's work is good: its newest check passed
/// for the files there now, or you approved it.
pub fn verified(conn: &Connection, task: &str) -> Result<bool> {
    if approved(conn, task)? {
        return Ok(true);
    }
    let Some(c) = last_check(conn, task)? else { return Ok(false) };
    if c.verdict != Verdict::Pass {
        return Ok(false);
    }
    Ok(match (c.tree, tree_now(conn, task)?) {
        (Some(a), Some(b)) => a == b,
        // Outside git: no run of it has started since.
        _ => !conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1 AND started > ?2)",
            params![task, c.started],
            |r| r.get::<_, bool>(0),
        )?,
    })
}

/// What a task waits on in its trail.
#[derive(Debug, Clone, Serialize)]
pub struct Wait {
    pub after: String,
    /// `verified` or `approved`.
    pub kind: String,
    pub met: bool,
}

/// The edges out of `task`, each with whether it is met. Once the task
/// has run, they are behind it: met.
pub fn waits(conn: &Connection, task: &str) -> Result<Vec<Wait>> {
    let started: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [task], |r| r.get(0))?;
    let edges: Vec<(String, String)> = conn
        .prepare_cached("SELECT after, kind FROM edge WHERE task = ?1 ORDER BY rowid")?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    edges
        .into_iter()
        .map(|(after, kind)| {
            let met =
                started || if kind == "approved" { approved(conn, &after)? } else { verified(conn, &after)? };
            Ok(Wait { after, kind, met })
        })
        .collect()
}

/// What holds `task` from starting: the first of its waits not met, so
/// long as it has never run. What it waits on gates its start only: once
/// it has begun, an upstream it shares a worktree with moves as it works,
/// and that must not stop it.
pub fn held(conn: &Connection, task: &str) -> Result<Option<Wait>> {
    let started: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [task], |r| r.get(0))?;
    if started {
        return Ok(None);
    }
    Ok(waits(conn, task)?.into_iter().find(|w| !w.met))
}

/// The tasks joined to `task` by edges, either way: its trail, in the
/// order they were made.
pub fn trail(conn: &Connection, task: &str) -> Result<Vec<String>> {
    let mut seen = vec![task.to_string()];
    let mut i = 0;
    while i < seen.len() {
        let t = seen[i].clone();
        let next: Vec<String> = conn
            .prepare_cached(
                "SELECT after FROM edge WHERE task = ?1 UNION SELECT task FROM edge WHERE after = ?1",
            )?
            .query_map([&t], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for n in next {
            if !seen.contains(&n) {
                seen.push(n);
            }
        }
        i += 1;
    }
    let mut ordered: Vec<(i64, String)> = seen
        .into_iter()
        .map(|t| {
            let created: i64 =
                conn.query_row("SELECT created FROM task WHERE id = ?1", [&t], |r| r.get(0))?;
            Ok((created, t))
        })
        .collect::<Result<_>>()?;
    ordered.sort();
    Ok(ordered.into_iter().map(|(_, t)| t).collect())
}

/// Whether an edge from `task` to `after` would close a cycle.
pub fn would_cycle(conn: &Connection, task: &str, after: &str) -> Result<bool> {
    if task == after {
        return Ok(true);
    }
    // Is `task` upstream of `after` already?
    let mut todo = vec![after.to_string()];
    let mut seen = vec![];
    while let Some(t) = todo.pop() {
        if t == task {
            return Ok(true);
        }
        if seen.contains(&t) {
            continue;
        }
        let ups: Vec<String> = conn
            .prepare_cached("SELECT after FROM edge WHERE task = ?1")?
            .query_map([&t], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        todo.extend(ups);
        seen.push(t);
    }
    Ok(false)
}

/// What a board row says of a task's check, loop or trail.
#[derive(Debug, Clone, Serialize)]
pub struct Flow {
    /// `↺ 2/3 ✗ exit 101`, `✓ verified in 3`, `⧗ abc123`.
    pub label: String,
    /// `working`, `needs`, `failing`, `quiet` or `good`: for whoever colours it.
    pub tone: &'static str,
    /// The loop's state, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
    /// The group it belongs in, where that is not its session's:
    /// `working` while a loop goes on between rounds, `needs_you` once
    /// it stops.
    #[serde(skip)]
    pub group: Option<&'static str>,
}

/// The board's line for a task with a check, a loop or edges; `None`
/// for any other. Only stored facts and cached trees are read: nothing
/// here runs git.
pub fn row(
    conn: &Connection,
    task: &str,
    state: &str,
    now: i64,
    alive: &dyn Fn(i64, i64) -> bool,
) -> Result<Option<Flow>> {
    if state != "open" {
        return Ok(None);
    }
    // What it waits on comes first: a loop of it cannot start before.
    let waits = waits(conn, task)?;
    if let Some(w) = held(conn, task)? {
        let what = if w.kind == "approved" { "your approval of " } else { "" };
        return Ok(Some(Flow {
            label: format!("⧗ {what}{}", crate::task::short(&w.after)),
            tone: "quiet",
            state: None,
            group: None,
        }));
    }
    let auto: bool =
        conn.query_row("SELECT coalesce(auto, 0) FROM task WHERE id = ?1", [task], |r| r.get(0))?;
    if let Some(def) = loop_of(conn, task)? {
        let st = self::state(conn, task, now, alive)?.expect("a loop");
        let last_fail = || -> Result<String> {
            Ok(checks(conn, task, def.started)?
                .last()
                .filter(|c| c.verdict == Verdict::Fail)
                .and_then(|c| c.summary.clone())
                .map(|s| format!(" ✗ {s}"))
                .unwrap_or_default())
        };
        let max = if review_only(conn, task)? { def.max.min(REVIEW_ONLY_MAX) } else { def.max };
        let (label, tone, group) = match &st {
            State::Paused { rounds } => (format!("↺ paused {rounds}/{max}"), "quiet", None),
            State::Working { round } => {
                (format!("↺ {round}/{max}{}", last_fail()?), "working", Some("working"))
            }
            State::Checking { round, .. } => {
                (format!("↺ {round}/{max} checking"), "working", Some("working"))
            }
            // Set but not going: its trail was not started.
            State::Due { round: 1, .. } if !auto => ("↺ ready to loop".to_string(), "quiet", None),
            State::Due { round, profile } => (
                format!(
                    "↺ {round}/{max}{}{}",
                    profile.as_ref().map(|p| format!(" {p}")).unwrap_or_default(),
                    last_fail()?
                ),
                "working",
                Some("working"),
            ),
            State::Verified { rounds } => (format!("✓ verified in {rounds}"), "good", Some("needs_you")),
            State::Flagged { why, .. } => (format!("⚑ {why}"), "failing", Some("needs_you")),
            State::Error { why, .. } => (format!("✗ {why}"), "failing", Some("needs_you")),
            State::Spent { why, .. } => (format!("■ spent, {why}"), "needs", Some("needs_you")),
            State::Stuck { rounds } => (format!("■ stuck after {rounds}"), "needs", Some("needs_you")),
            State::Asked { .. } => ("? asked you".to_string(), "needs", Some("needs_you")),
        };
        return Ok(Some(Flow { label, tone, state: Some(st.name()), group }));
    }
    let started: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [task], |r| r.get(0))?;
    if !waits.is_empty() && !started {
        let label = if auto { "▸ starting" } else { "▸ ready" };
        return Ok(Some(Flow { label: label.into(), tone: "quiet", state: None, group: None }));
    }
    if approved(conn, task)? {
        return Ok(Some(Flow { label: "✓ approved".into(), tone: "good", state: None, group: None }));
    }
    let has_check: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM check_def WHERE task = ?1)", [task], |r| r.get(0))?;
    if !has_check {
        return Ok(None);
    }
    Ok(Some(match last_check(conn, task)? {
        None => Flow { label: "check not run".into(), tone: "quiet", state: None, group: None },
        Some(c) if c.verdict == Verdict::Pass && verified(conn, task)? => {
            Flow { label: "✓ check".into(), tone: "good", state: None, group: None }
        }
        Some(c) if c.verdict == Verdict::Pass => {
            Flow { label: "✓ check, changed since".into(), tone: "quiet", state: None, group: None }
        }
        Some(c) => Flow {
            label: format!("✗ {}", c.summary.unwrap_or_else(|| c.verdict.name().into())),
            tone: "failing",
            state: None,
            group: None,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(max: i64, escalate: &[&str]) -> LoopDef {
        LoopDef {
            max,
            minutes: None,
            escalate: escalate.iter().map(|s| s.to_string()).collect(),
            started: 0,
            paused: false,
        }
    }

    fn round(n: i64, live: bool) -> Round {
        Round {
            run: format!("r{n}"),
            n,
            started: n * 100,
            ended: (!live).then_some(n * 100 + 50),
            exit_code: (!live).then_some(0),
            live,
            asked: false,
            profile: None,
        }
    }

    fn check(n: i64, verdict: Verdict, fp: &str, tree: &str) -> Check {
        Check {
            id: n,
            run: Some(format!("r{n}")),
            started: n * 100 + 60,
            finished: n * 100 + 70,
            verdict,
            tree: Some(tree.into()),
            fingerprint: Some(fp.into()),
            summary: Some("exit 1".into()),
            output: None,
        }
    }

    #[test]
    fn a_new_loop_starts_its_first_round() {
        assert_eq!(
            derive(&def(3, &[]), Some("build"), false, &[], &[]),
            State::Due { round: 1, profile: Some("build".into()) }
        );
    }

    #[test]
    fn a_round_at_work_then_its_check_then_the_next() {
        let d = def(3, &[]);
        assert_eq!(derive(&d, None, false, &[round(1, true)], &[]), State::Working { round: 1 });
        assert_eq!(
            derive(&d, None, false, &[round(1, false)], &[]),
            State::Checking { round: 1, run: "r1".into() }
        );
        let c = [check(1, Verdict::Fail, "a", "t1")];
        assert_eq!(derive(&d, None, false, &[round(1, false)], &c), State::Due { round: 2, profile: None });
    }

    #[test]
    fn a_pass_verifies_and_the_cap_spends() {
        let d = def(2, &[]);
        let rs = [round(1, false), round(2, false)];
        let pass = [check(1, Verdict::Fail, "a", "t1"), check(2, Verdict::Pass, "", "t2")];
        assert_eq!(derive(&d, None, false, &rs, &pass), State::Verified { rounds: 2 });
        let fail = [check(1, Verdict::Fail, "a", "t1"), check(2, Verdict::Fail, "b", "t2")];
        assert!(matches!(derive(&d, None, false, &rs, &fail), State::Spent { rounds: 2, .. }));
    }

    #[test]
    fn the_same_failure_on_the_same_files_escalates_then_sticks() {
        let d = def(9, &["deep"]);
        let rs = [round(1, false), round(2, false)];
        let same = [check(1, Verdict::Fail, "a", "t1"), check(2, Verdict::Fail, "a", "t1")];
        assert_eq!(
            derive(&d, Some("build"), false, &rs, &same),
            State::Due { round: 3, profile: Some("deep".into()) }
        );
        let rs3 = [round(1, false), round(2, false), round(3, false)];
        let again = [
            check(1, Verdict::Fail, "a", "t1"),
            check(2, Verdict::Fail, "a", "t1"),
            check(3, Verdict::Fail, "a", "t1"),
        ];
        assert_eq!(derive(&d, Some("build"), false, &rs3, &again), State::Stuck { rounds: 3 });
        // The files changed: progress, so not stuck.
        let moved = [check(1, Verdict::Fail, "a", "t1"), check(2, Verdict::Fail, "a", "t2")];
        assert_eq!(
            derive(&d, Some("build"), false, &rs, &moved),
            State::Due { round: 3, profile: Some("build".into()) }
        );
    }

    #[test]
    fn flagged_wins_over_everything() {
        let d = def(1, &[]);
        let c = [check(1, Verdict::Flagged, "", "t1")];
        assert!(matches!(derive(&d, None, false, &[round(1, false)], &c), State::Flagged { .. }));
    }

    #[test]
    fn a_question_ends_with_you_unless_the_check_passed() {
        let d = def(3, &[]);
        let mut r = round(1, false);
        r.asked = true;
        assert_eq!(
            derive(&d, None, false, &[r.clone()], &[check(1, Verdict::Fail, "a", "t")]),
            State::Asked { rounds: 1 }
        );
        assert_eq!(
            derive(&d, None, false, &[r], &[check(1, Verdict::Pass, "", "t")]),
            State::Verified { rounds: 1 }
        );
    }

    #[test]
    fn a_review_alone_gets_two_rounds() {
        let d = def(5, &[]);
        let rs = [round(1, false), round(2, false)];
        let c = [check(1, Verdict::Fail, "a", "t1"), check(2, Verdict::Fail, "b", "t2")];
        assert!(matches!(derive(&d, None, true, &rs, &c), State::Spent { .. }));
    }

    #[test]
    fn two_agent_failures_in_a_row_are_an_error() {
        let d = def(5, &[]);
        let mut a = round(1, false);
        a.exit_code = Some(1);
        let mut b = round(2, false);
        b.exit_code = Some(1);
        let c = [check(1, Verdict::Fail, "a", "t1"), check(2, Verdict::Fail, "b", "t2")];
        assert!(matches!(derive(&d, None, false, &[a, b], &c), State::Error { .. }));
    }
}
