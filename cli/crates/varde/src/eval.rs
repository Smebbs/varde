//! `varde eval`: what memory was worth, offline, from the store and the
//! agents' transcripts (design §7.7). It only reads. Each measure is a
//! heuristic, said as such:
//!
//! - *Reach*: of the sessions whose task had a cairn when they started,
//!   how many were given it (`shown`), or had it in their conversation
//!   already, by how they started.
//! - *Use*: of the anchored bullets a session was shown, how many it went
//!   on to touch: a tool call after it was shown names the anchor. And of
//!   the recalls an agent made, how many it followed with `varde show` of
//!   a hit.
//! - *Re-discovery*: bullets an update added that say what an earlier
//!   bullet or note already said (`cairn::similar`): one the session was
//!   shown (ignored), one in its project recall would have found (missed),
//!   or one only in another project (out of reach).
//! - *Fidelity*: how much of a note's specific words reached the cairn, and
//!   stayed; bullets dropped without a reason; churn between versions.
//! - *Staleness*: the share of shown bullets now marked stale.
//! - *Cost*: tokens per update and promotion, and per bullet used.
//! - *Holdout*: fresh sessions held out of memory beside the rest: tool
//!   calls and minutes before the first edit, and re-discoveries.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use varde_agent::ToolCall;
use varde_core::cairn::{self, Cairn};
use varde_core::store::KEEP_LOG_MS;
use varde_core::task;

use crate::{now_ms, Paths};

const DAY_MS: i64 = 86_400_000;

/// `--since`: a date (`2026-10-01`) or a number of days back (`30d`).
pub fn since(arg: &str, now: i64) -> Result<i64> {
    if let Some(days) = arg.strip_suffix('d').and_then(|d| d.parse::<i64>().ok()) {
        return days.checked_mul(DAY_MS).map(|ms| now - ms).context("--since: too many days");
    }
    // A real date: `2026-13-45` is refused, not read as some other day.
    chrono::NaiveDate::parse_from_str(arg, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc().timestamp_millis())
        .with_context(|| {
            format!("--since takes a date like 2026-10-01 or a count of days like 30d, not {arg}")
        })
}

struct Run {
    id: String,
    task: String,
    agent: String,
    relation: String,
    launch: String,
    holdout: Option<i64>,
    started: i64,
    transcript: Option<String>,
    project: Option<String>,
}

struct Shown {
    run: String,
    ts: i64,
    kind: String,
    task: Option<String>,
    items: Vec<String>,
}

/// A bullet or a note that something could have repeated.
struct Known {
    id: String,
    task: Option<String>,
    project: Option<String>,
    text: String,
    created: i64,
    words: HashSet<String>,
}

pub fn run(paths: &Paths, all: bool, since_arg: Option<&str>, as_json: bool) -> Result<()> {
    let store = paths.open(Duration::from_secs(2))?;
    let now = now_ms();
    // What runs were shown is kept only so long, so no further back.
    let from = since_arg.map(|s| since(s, now)).transpose()?.unwrap_or(0).max(now - KEEP_LOG_MS);
    let scope = crate::scope(&store, all)?;
    let report = report(&store.conn, scope.as_deref(), from, &transcripts)?;
    if as_json {
        println!("{report}");
    } else if report["runs"] == 0 {
        println!(
            "nothing to measure yet: eval reads the sessions of the last 30 days, and there are none here"
        );
    } else {
        print!("{}", describe(&report));
    }
    Ok(())
}

/// Every tool call of a transcript, by its agent's adapter.
fn transcripts(agent: &str, path: &str) -> Option<Vec<ToolCall>> {
    varde_agent::by_name(agent)?.tool_calls(Path::new(path)).ok()
}

/// The report as data, over runs in `scope` (every project if `None`)
/// started from `from` on. `calls` reads a run's transcript.
pub fn report(
    conn: &Connection,
    scope: Option<&str>,
    from: i64,
    calls: &dyn Fn(&str, &str) -> Option<Vec<ToolCall>>,
) -> Result<Value> {
    let runs = runs(conn, scope, from)?;
    let ids: HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let shown: Vec<Shown> = load_shown(conn)?.into_iter().filter(|s| ids.contains(s.run.as_str())).collect();
    let lineage = lineage(conn)?;
    let windows = windows(conn)?;

    // The tool calls of each run, inside its stretch of its transcript.
    let mut parsed: HashMap<String, Option<Vec<ToolCall>>> = HashMap::new();
    let mut tools: HashMap<&str, Vec<ToolCall>> = HashMap::new();
    for r in &runs {
        let Some(t) = &r.transcript else { continue };
        let all = parsed.entry(t.clone()).or_insert_with(|| calls(&r.agent, t));
        let Some(all) = all else { continue };
        let end = windows.get(&r.id).copied().unwrap_or(i64::MAX);
        let mine: Vec<ToolCall> =
            all.iter().filter(|c| c.ts.is_some_and(|ts| ts >= r.started && ts < end)).cloned().collect();
        tools.insert(&r.id, mine);
    }

    let reach = reach(conn, &runs, &shown, &lineage)?;
    let (use_, used) = usage(conn, &runs, &shown, &tools)?;
    let (mut rediscovery, per_run) = rediscovery(conn, &runs, &shown, &lineage)?;
    rediscovery["at_update"] = repeated(conn, scope, from)?;
    let fidelity = fidelity(conn, scope, from)?;
    let staleness = staleness(conn, scope, &shown)?;
    let cost = cost(conn, scope, from, used)?;
    let holdout = holdout(&runs, &tools, &per_run);
    Ok(json!({
        "v": 1,
        "scope": scope,
        "since": (from > 0).then_some(from),
        "runs": runs.len(),
        "transcripts": tools.len(),
        "reach": reach,
        "use": use_,
        "rediscovery": rediscovery,
        "fidelity": fidelity,
        "staleness": staleness,
        "cost": cost,
        "holdout": holdout,
    }))
}

fn runs(conn: &Connection, scope: Option<&str>, from: i64) -> Result<Vec<Run>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.task, r.agent, r.relation, r.launch, r.holdout, r.started, r.transcript, t.project
         FROM run r JOIN task t ON t.id = r.task
         WHERE r.started >= ?1 AND (?2 IS NULL OR t.project = ?2) ORDER BY r.started",
    )?;
    let rows = stmt.query_map(params![from, scope], |r| {
        Ok(Run {
            id: r.get(0)?,
            task: r.get(1)?,
            agent: r.get(2)?,
            relation: r.get(3)?,
            launch: r.get(4)?,
            holdout: r.get(5)?,
            started: r.get(6)?,
            transcript: r.get(7)?,
            project: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn load_shown(conn: &Connection) -> Result<Vec<Shown>> {
    let mut stmt = conn.prepare("SELECT run, ts, kind, task, items FROM shown ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        let items: String = r.get(4)?;
        Ok(Shown {
            run: r.get(0)?,
            ts: r.get(1)?,
            kind: r.get(2)?,
            task: r.get(3)?,
            items: serde_json::from_str(&items).unwrap_or_default(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Each run's conversation: itself and the runs it carries on, through
/// resumes and forks back to its fresh start.
fn lineage(conn: &Connection) -> Result<HashMap<String, Vec<String>>> {
    let all: Vec<(String, Option<String>, String)> = conn
        .prepare("SELECT id, parent_run, relation FROM run")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let by_id: HashMap<&str, (&Option<String>, &str)> =
        all.iter().map(|(id, p, rel)| (id.as_str(), (p, rel.as_str()))).collect();
    let mut out = HashMap::new();
    for (id, ..) in &all {
        let mut line = vec![id.clone()];
        let mut at = id.as_str();
        while let Some((Some(parent), rel)) = by_id.get(at) {
            if !matches!(*rel, "resume" | "fork") || line.contains(parent) {
                break;
            }
            line.push(parent.clone());
            at = parent;
        }
        out.insert(id.clone(), line);
    }
    Ok(out)
}

/// Where each run's stretch of a shared transcript ends: when the next run
/// on it started.
fn windows(conn: &Connection) -> Result<HashMap<String, i64>> {
    let all: Vec<(String, String, i64)> = conn
        .prepare("SELECT id, transcript, started FROM run WHERE transcript IS NOT NULL ORDER BY transcript, started")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = HashMap::new();
    for pair in all.windows(2) {
        if pair[0].1 == pair[1].1 {
            out.insert(pair[0].0.clone(), pair[1].2);
        }
    }
    Ok(out)
}

/// When each task first had a cairn, and each project a memory.
fn first_versions(conn: &Connection) -> Result<(HashMap<String, i64>, HashMap<String, i64>)> {
    let tasks = conn
        .prepare("SELECT task, min(ts) FROM cairn_history GROUP BY task")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let projects = conn
        .prepare("SELECT project, min(ts) FROM project_cairn_history GROUP BY project")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((tasks, projects))
}

fn reach(
    conn: &Connection,
    runs: &[Run],
    shown: &[Shown],
    lineage: &HashMap<String, Vec<String>>,
) -> Result<Value> {
    let (cairns, memories) = first_versions(conn)?;
    let given = |run: &str, kind: &str, task: Option<&str>| {
        shown.iter().any(|s| s.run == run && s.kind == kind && (task.is_none() || s.task.as_deref() == task))
    };
    let mut by_start: Vec<Value> = Vec::new();
    for start in ["fresh", "resume", "fork"] {
        let these: Vec<&Run> = runs.iter().filter(|r| r.relation == start).collect();
        let eligible: Vec<&&Run> =
            these.iter().filter(|r| cairns.get(&r.task).is_some_and(|t| *t < r.started)).collect();
        let held = |r: &Run| r.holdout == Some(1) || given(&r.id, "holdout", None);
        let gave = eligible.iter().filter(|r| given(&r.id, "cairn", Some(&r.task))).count();
        let had = eligible
            .iter()
            .filter(|r| {
                lineage.get(&r.id).is_some_and(|l| l.iter().any(|id| given(id, "cairn", Some(&r.task))))
            })
            .count();
        by_start.push(json!({
            "start": start,
            "runs": these.len(),
            "eligible": eligible.len(),
            "given": gave,
            "in_context": had,
            "held_out": these.iter().filter(|r| held(r)).count(),
        }));
    }
    let eligible: Vec<&Run> = runs
        .iter()
        .filter(|r| r.project.as_ref().and_then(|p| memories.get(p)).is_some_and(|t| *t < r.started))
        .collect();
    let project = json!({
        "runs": runs.len(),
        "eligible": eligible.len(),
        "given": eligible.iter().filter(|r| given(&r.id, "project", None)).count(),
    });
    Ok(json!({"cairn": by_start, "project": project}))
}

/// Whether a tool call's input names `needle` as a word of its own: not
/// inside a longer name (`run` in `run_job`, `cache.rs` in `keycache.rs`),
/// and not a name under four characters, which any call may hold.
fn mentions(input: &str, needle: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    needle.chars().count() >= 4
        && input.match_indices(needle).any(|(i, _)| {
            !word(input[..i].chars().next_back()) && !word(input[i + needle.len()..].chars().next())
        })
}

/// What a tool call has to name for an anchor to count as touched.
fn needle(anchor: &str) -> &str {
    if cairn::is_path(anchor) {
        anchor.trim_start_matches("./").trim_end_matches('/')
    } else {
        cairn::grep_term(anchor)
    }
}

/// Anchors of bullets by id, with whether each is stale now.
fn anchors(conn: &Connection) -> Result<HashMap<String, (Option<String>, bool)>> {
    let rows = conn
        .prepare("SELECT id, anchor, coalesce(stale, 0) FROM cairn_item")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?.to_string(), (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn usage(
    conn: &Connection,
    runs: &[Run],
    shown: &[Shown],
    tools: &HashMap<&str, Vec<ToolCall>>,
) -> Result<(Value, usize)> {
    let anchors = anchors(conn)?;
    let mut by_kind: HashMap<&str, (usize, usize, usize)> = HashMap::new();
    let mut used_total = 0;
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for s in shown.iter().filter(|s| matches!(s.kind.as_str(), "cairn" | "project" | "related")) {
        let Some(calls) = tools.get(s.run.as_str()) else { continue };
        for id in &s.items {
            if !seen.insert((s.run.as_str(), id.as_str())) {
                continue;
            }
            let e = by_kind.entry(s.kind.as_str()).or_default();
            e.0 += 1;
            let Some((Some(anchor), _)) = anchors.get(id) else { continue };
            e.1 += 1;
            let n = needle(anchor);
            if calls.iter().any(|c| c.ts.is_some_and(|t| t >= s.ts) && mentions(&c.input, n)) {
                e.2 += 1;
                used_total += 1;
            }
        }
    }
    let kinds: Vec<Value> = ["project", "cairn", "related"]
        .iter()
        .map(|k| {
            let (shown, anchored, touched) = by_kind.get(k).copied().unwrap_or_default();
            json!({"kind": k, "shown": shown, "anchored": anchored, "touched": touched})
        })
        .collect();

    // Recalls by agents, and whether a hit was then looked at.
    let ids: HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let recalls: Vec<(String, i64, String)> = conn
        .prepare("SELECT run, ts, coalesce(results, '[]') FROM recall_log WHERE source = 'agent' AND run IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let (mut asked, mut hits, mut followed) = (0, 0, 0);
    for (run, ts, results) in recalls.iter().filter(|(run, ..)| ids.contains(run.as_str())) {
        asked += 1;
        let found: Vec<Value> = serde_json::from_str(results).unwrap_or_default();
        let tasks: Vec<String> = found
            .iter()
            .filter_map(|h| h["task"].as_str())
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        if tasks.is_empty() {
            continue;
        }
        hits += 1;
        let calls = tools.get(run.as_str()).map(Vec::as_slice).unwrap_or_default();
        let looked = calls.iter().any(|c| {
            c.ts.is_some_and(|t| t >= *ts)
                && c.input.contains("varde show")
                && tasks.iter().any(|t| c.input.contains(t.as_str()) || c.input.contains(&task::short(t)))
        });
        followed += usize::from(looked);
    }
    Ok((
        json!({"bullets": kinds, "recalls": {"asked": asked, "with_hits": hits, "followed_by_show": followed}}),
        used_total,
    ))
}

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Every bullet and note, for finding what an update repeated.
fn known(conn: &Connection) -> Result<Vec<Known>> {
    let mut out: Vec<Known> = conn
        .prepare(
            "SELECT i.id, i.task, coalesce(t.project, i.project), i.text, i.created FROM cairn_item i
             LEFT JOIN task t ON t.id = i.task WHERE i.of IS NULL",
        )?
        .query_map([], |r| {
            let text: String = r.get(3)?;
            Ok(Known {
                id: r.get::<_, i64>(0)?.to_string(),
                task: r.get(1)?,
                project: r.get(2)?,
                words: words(&text),
                text,
                created: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let notes: Vec<Known> = conn
        .prepare("SELECT n.id, r.task, t.project, n.text, n.ts FROM note n JOIN run r ON r.id = n.run LEFT JOIN task t ON t.id = r.task")?
        .query_map([], |r| {
            let text: String = r.get(3)?;
            Ok(Known { id: r.get(0)?, task: r.get(1)?, project: r.get(2)?, words: words(&text), text, created: r.get(4)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    out.extend(notes);
    Ok(out)
}

/// Whether two bullets may say the same thing, before the real test:
/// `similar` needs at least three in eight of their words in common.
fn close(a: &Known, b: &Known) -> bool {
    let common = a.words.intersection(&b.words).count();
    common * 8 >= (a.words.len() + b.words.len()) * 3 / 2 && cairn::similar(&a.text, &b.text)
}

/// Bullets updates added that say what was already known, and how each
/// run's updates fared.
fn rediscovery(
    conn: &Connection,
    runs: &[Run],
    shown: &[Shown],
    lineage: &HashMap<String, Vec<String>>,
) -> Result<(Value, HashMap<String, usize>)> {
    let known = known(conn)?;
    let by_run: HashMap<&str, &Run> = runs.iter().map(|r| (r.id.as_str(), r)).collect();
    let added: Vec<(String, String)> = conn
        .prepare(
            "SELECT id, run FROM cairn_item WHERE author = 'job' AND run IS NOT NULL AND of IS NULL
               AND task IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?.to_string(), r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let by_id: HashMap<&str, &Known> = known.iter().map(|k| (k.id.as_str(), k)).collect();
    let (mut total, mut ignored, mut missed, mut unreachable) = (0, 0, 0, 0);
    let mut per_run: HashMap<String, usize> = HashMap::new();
    for (id, run) in &added {
        let (Some(r), Some(item)) = (by_run.get(run.as_str()), by_id.get(id.as_str())) else { continue };
        total += 1;
        let was_shown: HashSet<&str> = lineage
            .get(run)
            .into_iter()
            .flatten()
            .flat_map(|l| shown.iter().filter(move |s| s.run == *l))
            .flat_map(|s| s.items.iter().map(String::as_str))
            .collect();
        let earlier: Vec<&Known> =
            known.iter().filter(|k| k.created < r.started && k.id != *id && close(item, k)).collect();
        if earlier.is_empty() {
            continue;
        }
        if earlier.iter().any(|k| was_shown.contains(k.id.as_str())) {
            ignored += 1;
        } else if earlier.iter().any(|k| k.project == r.project || k.task.as_deref() == Some(r.task.as_str()))
        {
            missed += 1;
        } else {
            unreachable += 1;
        }
        *per_run.entry(run.clone()).or_default() += 1;
    }
    Ok((json!({"added": total, "ignored": ignored, "missed": missed, "unreachable": unreachable}), per_run))
}

/// What updates counted as they ran: ADDs that repeated a bullet word for
/// word (which confirm it), and new bullets close to one the session had
/// been shown. Only jobs that counted them, from when they began to.
fn repeated(conn: &Connection, scope: Option<&str>, from: i64) -> Result<Value> {
    let (jobs, repeated, like_shown): (i64, i64, i64) = conn.query_row(
        "SELECT count(j.repeated), coalesce(sum(j.repeated), 0), coalesce(sum(j.like_shown), 0)
         FROM job j JOIN run r ON r.id = j.run JOIN task t ON t.id = r.task
         WHERE j.finished >= ?1 AND (?2 IS NULL OR t.project = ?2)",
        params![from, scope],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(json!({"jobs": jobs, "repeated": repeated, "like_shown": like_shown}))
}

/// A note's specific words: paths, and identifiers that are plainly code.
fn specifics(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| "()[]{},;:\"'`".contains(c)).trim_end_matches('.'))
        // A line number drifts; the file stays.
        .map(|w| match w.rsplit_once(':') {
            Some((head, line)) if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) => head,
            _ => w,
        })
        .filter(|w| cairn::is_path(w) || (cairn::is_identifier(w) && (w.contains('_') || w.contains("::"))))
        .map(str::to_string)
        .collect()
}

fn fidelity(conn: &Connection, scope: Option<&str>, from: i64) -> Result<Value> {
    // Notes folded into a cairn, and how much of each reached it.
    let notes: Vec<(String, String, i64)> = conn
        .prepare(
            "SELECT r.task, n.text, n.ts FROM note n JOIN run r ON r.id = n.run JOIN task t ON t.id = r.task
             WHERE n.folded = 1 AND n.ts >= ?1 AND (?2 IS NULL OR t.project = ?2)",
        )?
        .query_map(params![from, scope], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let items: Vec<(String, String, i64, bool)> = conn
        .prepare("SELECT task, text, created, state = 'current' FROM cairn_item WHERE task IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let (mut with, mut none) = (0, 0);
    let (mut carried, mut kept) = (0.0, 0.0);
    for (task, text, ts) in &notes {
        let words = specifics(text);
        if words.is_empty() {
            continue;
        }
        with += 1;
        let share = |current_only: bool| {
            let texts: Vec<&str> = items
                .iter()
                .filter(|(t, _, created, current)| {
                    t == task && *created >= *ts && (*current || !current_only)
                })
                .map(|(_, text, ..)| text.as_str())
                .collect();
            words.iter().filter(|w| texts.iter().any(|t| t.contains(w.as_str()))).count() as f64
                / words.len() as f64
        };
        let c = share(false);
        carried += c;
        kept += share(true);
        none += usize::from(c == 0.0);
    }
    let mean = |x: f64| (with > 0).then(|| x / with as f64);

    // Bullets that left a cairn, by why.
    let reasons: Vec<Option<String>> = conn
        .prepare(
            "SELECT i.reason FROM cairn_item i JOIN task t ON t.id = i.task
             WHERE i.state = 'history' AND i.left_at >= ?1 AND (?2 IS NULL OR t.project = ?2)",
        )?
        .query_map(params![from, scope], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut drops: HashMap<&str, usize> = HashMap::new();
    for r in &reasons {
        let kind = match r.as_deref() {
            Some("edited") | Some("left in a rewrite, before bullets had ids") => continue,
            Some("cap") => "cap",
            Some("removed by an edit" | "removed in varde edit") => "by_user",
            None | Some("" | "left out by an update") => "without_reason",
            Some(_) => "with_reason",
        };
        *drops.entry(kind).or_default() += 1;
    }

    // Churn: how much of each version the update after it took out.
    let versions: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT h.task, h.author, h.doc FROM cairn_history h JOIN task t ON t.id = h.task
             WHERE h.ts >= ?1 AND (?2 IS NULL OR t.project = ?2) ORDER BY h.task, h.ts, h.id",
        )?
        .query_map(params![from, scope], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let (mut updates, mut churn) = (0, 0.0);
    for pair in versions.windows(2) {
        let ((t0, _, d0), (t1, author, d1)) = (&pair[0], &pair[1]);
        if t0 != t1 || author != "job" {
            continue;
        }
        let (Ok(a), Ok(b)) = (serde_json::from_str::<Cairn>(d0), serde_json::from_str::<Cairn>(d1)) else {
            continue;
        };
        // Only between versions of bullets with ids: a whole-document
        // rewrite, before them, churned by its nature, not by its updates.
        if a.items().chain(b.items()).any(|(_, i)| i.id == 0) {
            continue;
        }
        let before: Vec<&str> = a.items().map(|(_, i)| i.text.as_str()).collect();
        if before.is_empty() {
            continue;
        }
        let after: HashSet<&str> = b.items().map(|(_, i)| i.text.as_str()).collect();
        updates += 1;
        churn += before.iter().filter(|t| !after.contains(*t)).count() as f64 / before.len() as f64;
    }
    Ok(json!({
        "notes": notes.len(),
        "with_specifics": with,
        "carried": mean(carried),
        "kept": mean(kept),
        "carried_none": none,
        "drops": drops,
        "updates": updates,
        "churn": (updates > 0).then(|| churn / updates as f64),
    }))
}

fn staleness(conn: &Connection, scope: Option<&str>, shown: &[Shown]) -> Result<Value> {
    let anchors = anchors(conn)?;
    let mut ids: HashSet<(&str, &str)> = HashSet::new();
    for s in shown.iter().filter(|s| matches!(s.kind.as_str(), "cairn" | "project" | "related")) {
        ids.extend(s.items.iter().map(|i| (s.run.as_str(), i.as_str())));
    }
    let anchored: Vec<bool> = ids
        .iter()
        .filter_map(|(_, id)| anchors.get(*id))
        .filter(|(a, _)| a.is_some())
        .map(|(_, s)| *s)
        .collect();
    let (current, stale): (i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(coalesce(i.stale, 0)), 0) FROM cairn_item i LEFT JOIN task t ON t.id = i.task
         WHERE i.state = 'current' AND i.anchor IS NOT NULL AND (?1 IS NULL OR coalesce(t.project, i.project) = ?1)",
        [scope],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(json!({
        "shown": ids.len(),
        "shown_anchored": anchored.len(),
        "shown_stale_now": anchored.iter().filter(|s| **s).count(),
        "current_anchored": current,
        "current_stale": stale,
    }))
}

fn cost(conn: &Connection, scope: Option<&str>, from: i64, used: usize) -> Result<Value> {
    // Per kind: jobs, jobs that recorded their tokens, calls, and tokens
    // in and out. A job from before tokens were recorded cost what is not
    // known, not nothing.
    type Sums = (i64, i64, i64, i64, i64);
    let sums = |r: &rusqlite::Row| -> rusqlite::Result<(String, Sums)> {
        Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
    };
    let mut by_kind: HashMap<String, Sums> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT j.kind, count(*), count(j.input_tokens), coalesce(sum(j.calls), 0),
                coalesce(sum(j.input_tokens), 0), coalesce(sum(j.output_tokens), 0)
         FROM job j JOIN run r ON r.id = j.run JOIN task t ON t.id = r.task
         WHERE j.state IN ('done', 'unchanged', 'skipped') AND j.finished >= ?1 AND (?2 IS NULL OR t.project = ?2)
         GROUP BY j.kind",
    )?;
    for row in stmt.query_map(params![from, scope], sums)? {
        let (kind, n) = row?;
        by_kind.insert(kind, n);
    }
    // Jobs whose model was asked and changed nothing, while they are kept.
    let unchanged: HashMap<String, i64> = conn
        .prepare(
            "SELECT j.kind, count(*) FROM job j JOIN run r ON r.id = j.run JOIN task t ON t.id = r.task
             WHERE j.state = 'unchanged' AND j.finished >= ?1 AND (?2 IS NULL OR t.project = ?2)
             GROUP BY j.kind",
        )?
        .query_map(params![from, scope], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut kinds: Vec<Value> = Vec::new();
    let mut tokens = 0;
    for kind in ["fold", "close", "promote"] {
        let (jobs, metered, calls, input, output) = by_kind.get(kind).copied().unwrap_or_default();
        tokens += input + output;
        kinds.push(json!({
            "kind": kind, "jobs": jobs, "unchanged": unchanged.get(kind).copied().unwrap_or(0),
            "metered": metered, "calls": calls, "input_tokens": input,
            "output_tokens": output, "per_job": (metered > 0).then(|| (input + output) / metered),
        }));
    }
    Ok(
        json!({"by_kind": kinds, "tokens": tokens, "per_bullet_used": (used > 0).then(|| tokens / used as i64)}),
    )
}

/// Tool calls and minutes before the first edit, for a run that made one.
fn orientation(r: &Run, calls: &[ToolCall]) -> Option<(usize, f64)> {
    let first = calls.iter().position(|c| c.edit)?;
    let at = calls[first].ts?;
    Some((first, (at - r.started) as f64 / 60_000.0))
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    xs.sort_by(|a, b| a.total_cmp(b));
    (!xs.is_empty()).then(|| xs[xs.len() / 2])
}

fn holdout(
    runs: &[Run],
    tools: &HashMap<&str, Vec<ToolCall>>,
    rediscovered: &HashMap<String, usize>,
) -> Value {
    let group = |held: i64| {
        let these: Vec<&Run> = runs
            .iter()
            .filter(|r| r.relation == "fresh" && r.launch == "managed" && r.holdout == Some(held))
            .collect();
        let oriented: Vec<(usize, f64)> =
            these.iter().filter_map(|r| orientation(r, tools.get(r.id.as_str())?)).collect();
        let again: usize = these.iter().map(|r| rediscovered.get(&r.id).copied().unwrap_or(0)).sum();
        json!({
            "runs": these.len(),
            "with_an_edit": oriented.len(),
            "calls_before_edit": median(oriented.iter().map(|o| o.0 as f64).collect()),
            "minutes_before_edit": median(oriented.iter().map(|o| o.1).collect()),
            "rediscovered": again,
            "rediscovered_per_run": (!these.is_empty()).then(|| again as f64 / these.len() as f64),
        })
    };
    json!({"held_out": group(1), "given": group(0)})
}

/// `n of of (p%)`; only asked for when `of` is more than none.
fn pct(n: &Value, of: &Value) -> String {
    match (n.as_f64(), of.as_f64()) {
        (Some(n), Some(of)) if of > 0.0 => format!("{n} of {of} ({:.0}%)", 100.0 * n / of),
        _ => "-".into(),
    }
}

fn num(v: &Value, digits: usize) -> String {
    v.as_f64().map_or_else(|| "-".into(), |x| format!("{x:.digits$}"))
}

/// The report as text, in the words the rest of varde uses: a cairn's
/// lines, and the updates that write them. A measure with nothing to
/// count is left out, and a part with no measures with it.
pub fn describe(r: &Value) -> String {
    let mut out = format!(
        "memory in {}{}: {} runs, {} transcripts read\n",
        r["scope"].as_str().unwrap_or("every project"),
        r["since"]
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|d| format!(" since {}", d.format("%Y-%m-%d")))
            .unwrap_or_default(),
        r["runs"],
        r["transcripts"]
    );
    let some = |v: &Value| v.as_u64().unwrap_or(0) > 0;
    let mut part = |title: &str, lines: Vec<String>| {
        if !lines.is_empty() {
            out.push_str(&format!("\n{title}\n"));
            for l in lines {
                out.push_str(&format!("  {l}\n"));
            }
        }
    };

    let mut reach = Vec::new();
    for k in r["reach"]["cairn"].as_array().into_iter().flatten().filter(|k| some(&k["eligible"])) {
        reach.push(format!(
            "{:<7} {} sessions, {} with a cairn: handed it {}; had it already from earlier in the conversation {}; held out {}",
            k["start"].as_str().unwrap_or(""),
            k["runs"],
            k["eligible"],
            pct(&k["given"], &k["eligible"]),
            pct(&k["in_context"], &k["eligible"]),
            k["held_out"]
        ));
    }
    let p = &r["reach"]["project"];
    if some(&p["eligible"]) {
        reach.push(format!(
            "project cairn handed to {} of the sessions whose project had one",
            pct(&p["given"], &p["eligible"])
        ));
    }
    part(
        "reach: did sessions get their task's cairn? (of those whose task had one, by how they started)",
        reach,
    );

    let mut used = Vec::new();
    for k in r["use"]["bullets"].as_array().into_iter().flatten().filter(|k| some(&k["anchored"])) {
        used.push(format!(
            "{:<8} {} lines shown, {} name code, used {}",
            k["kind"].as_str().unwrap_or(""),
            k["shown"],
            k["anchored"],
            pct(&k["touched"], &k["anchored"])
        ));
    }
    let rc = &r["use"]["recalls"];
    if some(&rc["asked"]) {
        let mut l =
            format!("agents ran varde recall {} times, {} found something", rc["asked"], rc["with_hits"]);
        if some(&rc["with_hits"]) {
            l.push_str(&format!(
                ", then looked at a hit with varde show {}",
                pct(&rc["followed_by_show"], &rc["with_hits"])
            ));
        }
        used.push(l);
    }
    part(
        "use: did sessions act on what they were shown? (a line is used when a later tool call names its file or identifier)",
        used,
    );

    let rd = &r["rediscovery"];
    let mut again = Vec::new();
    if some(&rd["added"]) {
        again.push(format!(
            "of {} lines updates added, {} repeat one the session had been shown, {} one recall could have found, {} one only another project knew",
            rd["added"], rd["ignored"], rd["missed"], rd["unreachable"]
        ));
    }
    let u = &rd["at_update"];
    if some(&u["jobs"]) {
        again.push(format!(
            "over {} updates: {} lines added word for word as one already there, {} close to a line the session had been shown",
            u["jobs"], u["repeated"], u["like_shown"]
        ));
    }
    part("re-discovery: did updates write down what was already known?", again);

    let f = &r["fidelity"];
    let mut kept = Vec::new();
    if some(&f["notes"]) {
        kept.push(format!(
            "{} notes taken into a cairn, {} naming files or identifiers; on average {} of those names reached the cairn and {} are still there; {} notes left no trace",
            f["notes"],
            f["with_specifics"],
            share(&f["carried"]),
            share(&f["kept"]),
            f["carried_none"]
        ));
    }
    let d = &f["drops"];
    let n = |k: &str| d[k].as_u64().unwrap_or(0);
    if n("with_reason") + n("without_reason") + n("cap") + n("by_user") > 0 {
        kept.push(format!(
            "lines that left a cairn: {} with a reason, {} without, {} pushed out by the size cap, {} removed by you",
            n("with_reason"),
            n("without_reason"),
            n("cap"),
            n("by_user"),
        ));
    }
    if some(&f["updates"]) {
        kept.push(format!(
            "each update took out {} of the lines before it, over {} updates",
            share(&f["churn"]),
            f["updates"]
        ));
    }
    part("fidelity: did notes and lines survive?", kept);

    let s = &r["staleness"];
    let mut stale = Vec::new();
    if some(&s["shown_anchored"]) {
        stale.push(format!(
            "lines shown that name code and are marked stale now: {}",
            pct(&s["shown_stale_now"], &s["shown_anchored"])
        ));
    }
    if some(&s["current_anchored"]) {
        stale.push(format!(
            "lines in cairns now that name code and are stale: {}",
            pct(&s["current_stale"], &s["current_anchored"])
        ));
    }
    part("staleness: do lines still name code that exists?", stale);

    let mut cost = Vec::new();
    for k in r["cost"]["by_kind"].as_array().into_iter().flatten().filter(|k| some(&k["jobs"])) {
        cost.push(format!(
            "{:<8} {} updates ({} changed nothing), {} model calls, {} tokens in, {} out, {} an update (of the {} that recorded tokens)",
            k["kind"].as_str().unwrap_or(""),
            k["jobs"],
            k["unchanged"],
            k["calls"],
            k["input_tokens"],
            k["output_tokens"],
            num(&k["per_job"], 0),
            k["metered"]
        ));
    }
    if !r["cost"]["per_bullet_used"].is_null() {
        cost.push(format!("tokens for each line a session used: {}", num(&r["cost"]["per_bullet_used"], 0)));
    }
    part("cost: tokens the updates spent", cost);

    let (held, given) = (&r["holdout"]["held_out"], &r["holdout"]["given"]);
    if some(&held["runs"]) {
        let rows = [("held out", held), ("given", given)].map(|(name, g)| {
            format!(
                "{name:<8} {} sessions, {} edited code: median {} tool calls and {} minutes before the first edit; {} re-discoveries",
                g["runs"],
                g["with_an_edit"],
                num(&g["calls_before_edit"], 0),
                num(&g["minutes_before_edit"], 1),
                g["rediscovered"]
            )
        });
        part(
            "holdout: did memory help? (fresh sessions held out of memory beside those given it)",
            rows.into(),
        );
    }
    out
}

/// A share as a percentage.
fn share(v: &Value) -> String {
    v.as_f64().map_or_else(|| "-".into(), |x| format!("{:.0}%", 100.0 * x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_anchor_is_used_only_when_a_call_names_it_as_a_word() {
        assert!(mentions(r#"{"pattern":"cache_key_for"}"#, "cache_key_for"));
        assert!(mentions(r#"{"file_path":"/x/src/cache.rs"}"#, "cache.rs"));
        assert!(!mentions(r#"{"file_path":"/x/src/keycache.rs"}"#, "cache.rs"));
        assert!(!mentions(r#"{"command":"cargo run_job"}"#, "run_job2"));
        assert!(!mentions(r#"{"command":"run_job --x"}"#, "run"));
    }

    #[test]
    fn since_takes_a_date_or_days() {
        assert_eq!(since("2026-10-01", 0).unwrap(), 1_790_812_800_000);
        assert_eq!(since("30d", 31 * DAY_MS).unwrap(), DAY_MS);
        assert!(since("last week", 0).is_err());
        assert!(since("2026-13-45", 0).is_err());
        assert!(since("2026-02-30", 0).is_err());
    }

    #[test]
    fn specific_words_are_paths_and_plain_identifiers() {
        let s =
            specifics("The key in `cache_key_for` (src/cache.rs:12) omits the locale; see memory::recall.");
        assert!(
            s.contains("cache_key_for") && s.contains("src/cache.rs") && s.contains("memory::recall"),
            "{s:?}"
        );
        assert!(!s.contains("locale"));
    }
}
