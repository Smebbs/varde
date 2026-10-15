//! Recall: the full-text index of every task's memory and every
//! project's, searched by `varde recall` and for related work.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::Owner;
use crate::store::Store;

/// Rebuild what recall searches for one task: its title, its bullets,
/// current and former, and its notes. A task with only a title is found
/// by it, after every task that matched more, and is never offered as
/// related work.
pub fn reindex(conn: &Connection, task: &str) -> Result<()> {
    delete_rows(conn, "c7 = ?1", task)?;
    let mut rows = indexed_items(conn, Owner::Task(task))?;
    let notes: Vec<(String, String, String)> = conn
        .prepare_cached(
            "SELECT 'notes', n.id, n.text FROM note n JOIN run r ON r.id = n.run WHERE r.task = ?1",
        )?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    rows.extend(notes);
    let title: String = conn.query_row("SELECT title FROM task WHERE id = ?1", [task], |r| r.get(0))?;
    rows.push(("title".into(), String::new(), title));
    let mut insert = conn.prepare_cached(
        "INSERT INTO memory_fts (task, kind, ref, title, findings, decisions, open, history, notes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (kind, reference, text) in rows {
        let col = |c: &str| (kind == c).then_some(text.as_str());
        insert.execute(params![
            task,
            kind,
            reference,
            col("title"),
            col("findings"),
            col("decisions"),
            col("open"),
            col("history"),
            col("notes")
        ])?;
    }
    Ok(())
}

/// Index the title of every task not yet indexed: those a hook made,
/// which does not write the index on the way in. Returns how many.
pub fn index_titles(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "INSERT INTO memory_fts (task, kind, ref, title)
         SELECT id, 'title', '', title FROM task
         WHERE id NOT IN (SELECT c7 FROM memory_fts_content WHERE c7 IS NOT NULL)",
        [],
    )?)
}

/// Rebuild what recall searches for a project's cairn: its bullets,
/// current and former.
pub fn reindex_project(conn: &Connection, project: &str) -> Result<()> {
    delete_rows(conn, "c7 IS NULL AND c10 = ?1", project)?;
    let rows = indexed_items(conn, Owner::Project(project))?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO memory_fts (memory, history, kind, ref, scope) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (kind, reference, text) in rows {
        let history = kind == "history";
        insert.execute(params![
            (!history).then_some(&text),
            history.then_some(&text),
            kind,
            reference,
            project
        ])?;
    }
    Ok(())
}

/// Delete the index rows `filter` picks, by rowid. The filter is on
/// `memory_fts_content` (task is c7, scope c10), which has an index on
/// each: filtering `memory_fts` itself on an UNINDEXED column scans
/// every row.
fn delete_rows(conn: &Connection, filter: &str, key: &str) -> Result<()> {
    let rowids: Vec<i64> = conn
        .prepare_cached(&format!("SELECT id FROM memory_fts_content WHERE {filter}"))?
        .query_map([key], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut delete = conn.prepare_cached("DELETE FROM memory_fts WHERE rowid = ?1")?;
    for rowid in rowids {
        delete.execute([rowid])?;
    }
    Ok(())
}

/// `owner`'s bullets, current and former, as recall indexes them: what
/// each is (its section, or `history`), its id, and its text.
fn indexed_items(conn: &Connection, owner: Owner) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT CASE state WHEN 'current' THEN section ELSE 'history' END, id, text FROM cairn_item
         WHERE {} = ?1",
        owner.column()
    ))?;
    let rows =
        stmt.query_map([owner.key()], |r| Ok((r.get(0)?, r.get::<_, i64>(1)?.to_string(), r.get(2)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub task: String,
    pub short: String,
    pub title: String,
    pub project: Option<String>,
    pub status: String,
    /// The text that matched: a bullet, a former bullet, a note, or the
    /// title; for a task found by its id, its Status.
    pub snippet: String,
    /// What matched: `findings`, `decisions`, `open`, `history` (a bullet
    /// that has left the cairn), `notes`, `title`, or `id`; in a project's
    /// memory, which has no task, `conventions`, `gotchas` or `howto`.
    pub kind: String,
    /// The bullet's id, or the note's.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// How well it matched, higher is better: its BM25 score, or for
    /// related work how many of the text's specific terms it matched (an
    /// identifier counts two); none for a task found by id.
    pub score: Option<f64>,
    /// The task's state (`open`, `done`, `parked`, `dropped`); none for a
    /// project's cairn.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// The bullet's anchor no longer resolves in the code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    /// How many other lines of the same task (or project cairn) matched
    /// among the best rows, which one hit a task leaves out.
    #[serde(skip_serializing_if = "is_zero")]
    pub more: u32,
    #[serde(skip)]
    rowid: i64,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Whether bullet `reference` of kind `kind` is marked stale.
fn stale_of(conn: &Connection, kind: &str, reference: Option<&str>) -> Result<bool> {
    let Some(id) =
        reference.filter(|_| !matches!(kind, "notes" | "title" | "id")).and_then(|r| r.parse::<i64>().ok())
    else {
        return Ok(false);
    };
    Ok(conn
        .prepare_cached("SELECT coalesce(stale, 0) FROM cairn_item WHERE id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?
        .unwrap_or(false))
}

impl Hit {
    /// Whether two hits are in the same task, or the same project's cairn.
    fn same_place(&self, other: &Hit) -> bool {
        self.task == other.task && (!self.task.is_empty() || self.project == other.project)
    }
}

/// What recall found, and whether it looked beyond the project for it.
#[derive(Debug, Default, Serialize)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// No hit in the project: these are from every project.
    pub everywhere: bool,
}

/// Who asked, for the recall log: `agent` (`varde recall` in a session),
/// `user`, `preamble` or `hook`, and the run it was for.
#[derive(Clone, Copy)]
pub struct Asker<'a> {
    pub source: &'a str,
    pub run: Option<&'a str>,
}

/// Weights of title, findings, decisions, open, history, notes and
/// project cairn: Findings and Decisions are the densest statement of
/// what was learned, as a project's cairn is of what holds across its
/// tasks, and what has left a cairn ranks below what is in one.
const WEIGHTS: &str = "2.0, 3.0, 3.0, 1.5, 1.0, 2.0, 3.0";

/// Full-text search over every task's memory, best first, one hit per
/// task. A word that is a task's id, or the start of what `short` shows
/// (four characters or more), finds that task first, in any project:
/// agents look a task up by the id they were given. With no hit in
/// `project`, the best hits from every project.
pub fn recall(
    store: &Store,
    query: &str,
    project: Option<&str>,
    exclude: Option<&str>,
    limit: usize,
    asker: Asker,
    now: i64,
) -> Result<Found> {
    let mut found = Found::default();
    found.hits.extend(by_id(store, query, exclude, limit)?);
    if let Some(q) = fts_query(query) {
        let mut more = search(&store.conn, &q, project, exclude, limit)?;
        if more.is_empty() && project.is_some() {
            more = search(&store.conn, &q, None, exclude, limit)?;
            found.everywhere = !more.is_empty();
        }
        for h in more {
            if found.hits.len() < limit && !found.hits.iter().any(|k| k.same_place(&h)) {
                found.hits.push(h);
            }
        }
    }
    log(&store.conn, query, project, &found, asker, now)?;
    Ok(found)
}

/// Earlier work related to `text`, to offer a session that did not ask:
/// only hits on a bullet or a note, never on a title alone, that match
/// two of its specific terms, or one specific identifier. One word in
/// common is how a task about a loop was offered one about a
/// double-restart loop. Only in `project` (with none, only in tasks of
/// none): offered unasked, earlier work never crosses projects, which may
/// be different clients'. `varde recall` still looks everywhere when asked.
pub fn related(
    conn: &Connection,
    text: &str,
    project: Option<&str>,
    exclude: Option<&str>,
    limit: usize,
    asker: Asker,
    now: i64,
) -> Result<Found> {
    let found = related_quietly(conn, text, project, exclude, limit)?;
    log(conn, text, project, &found, asker, now)?;
    Ok(found)
}

/// At most this many terms of a text are looked for as related work: a
/// pasted prompt can hold hundreds, and each costs an index lookup.
const RELATED_TERMS: usize = 16;

/// The words of the current bullets of `task`'s cairn and of `project`'s
/// memory: what a session on it is shown anyway, so related work that
/// says one of them again is no news.
fn shown_anyway(
    conn: &Connection,
    project: Option<&str>,
    task: Option<&str>,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT text FROM cairn_item WHERE task = ?1 AND state = 'current'
         UNION ALL SELECT text FROM cairn_item WHERE project = ?2 AND state = 'current'",
    )?;
    let rows = stmt.query_map(params![task, project], |r| r.get::<_, String>(0))?;
    let mut out = std::collections::HashSet::new();
    for text in rows {
        out.insert(crate::cairn::normalized(&text?));
    }
    Ok(out)
}

/// `related`, without logging: for varde's own use, such as promotion.
/// Its work is bounded, not timed: at most `RELATED_TERMS` index lookups
/// and `RELATED_ROWS` rows read, which the bench holds to a hook's budget.
/// What the session is shown anyway, its own cairn and its project's
/// memory, is not offered again.
pub fn related_quietly(
    conn: &Connection,
    text: &str,
    project: Option<&str>,
    exclude: Option<&str>,
    limit: usize,
) -> Result<Found> {
    let mut found = Found::default();
    // Whole words only, stemmed: a prefix is for someone typing part of a
    // word, and costs a walk through every word it starts.
    let mut terms: Vec<(String, bool)> =
        terms(text).into_iter().map(|(t, ident)| (t.trim_end_matches('*').to_string(), ident)).collect();
    terms.dedup();
    terms.truncate(RELATED_TERMS);
    // Each row's weight: how many specific terms it matches, an
    // identifier counting twice. Only rows of weight two are related.
    let mut weight: HashMap<i64, u32> = HashMap::new();
    for ((_, ident), rows) in terms.iter().zip(specific_rows(conn, &terms)?) {
        let Some(rows) = rows else { continue };
        for r in rows {
            *weight.entry(r).or_default() += if *ident { 2 } else { 1 };
        }
    }
    // The related rows are known now. Each is read by its id from the
    // index's own content table, the most terms first, then by what it
    // is (a line of the user's, a finding or decision, a note, an open
    // item, a former bullet), then the newest; reading stops once
    // nothing left can outrank what was found. No ranking over every row that matched one word: on a
    // large store that is what cost.
    use std::cmp::Reverse;
    let mut rows: Vec<(i64, u32)> = weight.into_iter().filter(|(_, w)| *w >= 2).collect();
    rows.sort_by_key(|(rowid, w)| (Reverse(*w), Reverse(*rowid)));
    rows.truncate(RELATED_ROWS);
    // `memory_fts`'s columns, in its content table: the text in c1 to c5,
    // then task, kind and ref in c7, c8 and c9.
    let mut row = conn.prepare_cached(
        "SELECT c7, c8, c9, coalesce(c1, c2, c3, c4, c5) FROM memory_fts_content WHERE id = ?1",
    )?;
    let mut mine = conn.prepare_cached("SELECT mine FROM cairn_item WHERE id = ?1")?;
    let mut about = conn.prepare_cached(
        "SELECT t.title, t.project, c.status, t.state FROM task t LEFT JOIN cairn c ON c.task = t.id
         WHERE t.id = ?1",
    )?;
    type About = Option<(String, Option<String>, Option<String>, String)>;
    let mut tasks: HashMap<String, About> = HashMap::new();
    let mut said: Option<std::collections::HashSet<String>> = None;
    let mut hits: Vec<(u32, Hit)> = Vec::new();
    // Enough at `floor` or above: nothing below it counts.
    let enough = |hits: &[(u32, Hit)], floor: u32| {
        let mut seen: Vec<&str> = Vec::new();
        for (rank, h) in hits {
            if *rank >= floor && !seen.contains(&h.task.as_str()) {
                seen.push(&h.task);
            }
        }
        seen.len() >= limit
    };
    for (rowid, w) in rows {
        if enough(&hits, w * TIERS + TIERS - 1) {
            break;
        }
        let read = row
            .query_row([rowid], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .optional()?;
        // A title alone is not related work, and a project's cairn is
        // shown anyway.
        let Some((Some(task), kind, reference, text)) = read else { continue };
        if kind == "title" || exclude == Some(task.as_str()) {
            continue;
        }
        // Nor what the session is shown anyway, in other words or not.
        let said = said.get_or_insert_with(|| shown_anyway(conn, project, exclude).unwrap_or_default());
        if said.contains(&crate::cairn::normalized(text.as_deref().unwrap_or_default())) {
            continue;
        }
        if !tasks.contains_key(&task) {
            let a =
                about.query_row([&task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
            tasks.insert(task.clone(), a);
        }
        // Offered unasked, related work stays in its project: another
        // project may be another client's.
        let Some(Some((title, task_project, status, state))) = tasks.get(&task) else { continue };
        if task_project.as_deref() != project {
            continue;
        }
        let reference = reference.filter(|r| !r.is_empty());
        let cairn_line = matches!(kind.as_str(), "findings" | "decisions" | "open");
        let theirs = cairn_line
            && match reference.as_deref().and_then(|r| r.parse::<i64>().ok()) {
                Some(id) => mine.query_row([id], |r| r.get::<_, bool>(0)).optional()?.unwrap_or(false),
                None => false,
            };
        let rank = w * TIERS
            + match kind.as_str() {
                _ if theirs => 4,
                "findings" | "decisions" => 3,
                "notes" => 2,
                "open" => 1,
                _ => 0,
            };
        hits.push((
            rank,
            Hit {
                short: crate::task::short(&task),
                title: title.clone(),
                project: task_project.clone(),
                status: status.clone().unwrap_or_default(),
                stale: stale_of(conn, &kind, reference.as_deref())?,
                state: state.clone(),
                kind,
                reference,
                snippet: text.unwrap_or_default(),
                score: Some(w as f64),
                rowid,
                more: 0,
                task,
            },
        ));
    }
    hits.sort_by_key(|(rank, h)| (std::cmp::Reverse(*rank), std::cmp::Reverse(h.rowid)));
    for (_, h) in hits {
        if found.hits.len() < limit && !found.hits.iter().any(|k| k.task == h.task) {
            found.hits.push(h);
        }
    }
    Ok(found)
}

/// At most this many related rows are read, the best first.
const RELATED_ROWS: usize = 200;

/// How many kinds of hit a related row's rank tells apart within its
/// weight, the user's own lines first.
const TIERS: u32 = 5;

fn by_id(store: &Store, query: &str, exclude: Option<&str>, limit: usize) -> Result<Vec<Hit>> {
    let mut hits: Vec<Hit> = Vec::new();
    let ids = query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| (4..=6).contains(&w.len()) || w.len() == 26);
    let mut stmt = store.conn.prepare_cached(
        "SELECT t.id, t.title, t.project, c.status, t.state
         FROM task t LEFT JOIN cairn c ON c.task = t.id
         WHERE (t.id = upper(?1) OR (length(?1) <= 6 AND substr(lower(t.id), -6, length(?1)) = lower(?1)))
           AND (?2 IS NULL OR t.id != ?2) AND t.state != 'dropped'
         LIMIT 2",
    )?;
    for word in ids {
        let found: Vec<Hit> = stmt
            .query_map(params![word, exclude], |r| {
                let task: String = r.get(0)?;
                let status = r.get::<_, Option<String>>(3)?.unwrap_or_default();
                Ok(Hit {
                    short: crate::task::short(&task),
                    task,
                    title: r.get(1)?,
                    project: r.get(2)?,
                    snippet: status.clone(),
                    status,
                    kind: "id".into(),
                    reference: None,
                    score: None,
                    state: r.get(4)?,
                    stale: false,
                    rowid: 0,
                    more: 0,
                })
            })?
            .collect::<Result<_, _>>()?;
        // A prefix two tasks share names neither.
        if let [one] = &found[..] {
            if hits.len() < limit && !hits.iter().any(|h| h.task == one.task) {
                hits.push(one.clone());
            }
        }
    }
    Ok(hits)
}

/// The rows each of `terms` matches, for each that says something
/// specific: a term found in more than `COMMON_PERCENT` of what recall
/// searches (`varde` in varde's own tasks, `prod` in an ops team's)
/// matches too much to make a hit related, and is `None`. In a store too
/// small to tell, every term counts. One indexed lookup per term, which
/// stops once a term is known to be common.
fn specific_rows(conn: &Connection, terms: &[(String, bool)]) -> Result<Vec<Option<Vec<i64>>>> {
    const COMMON_PERCENT: i64 = 4;
    const SMALL: i64 = 200;
    let total = indexed_rows(conn)?;
    let most = if total < SMALL { i64::MAX - 1 } else { total * COMMON_PERCENT / 100 };
    let mut stmt = conn.prepare_cached("SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1 LIMIT ?2")?;
    let mut out = Vec::new();
    for (t, _) in terms {
        let rows: Vec<i64> = stmt.query_map(params![t, most + 1], |r| r.get(0))?.collect::<Result<_, _>>()?;
        out.push((rows.len() as i64 <= most).then_some(rows));
    }
    Ok(out)
}

/// How many rows recall searches, as of the last commit: the count FTS5
/// keeps itself, the first varint of its averages record (rowid 1 of
/// `memory_fts_data`), written when a transaction that changed the index
/// commits. Counting the table of row sizes read every row of it, a
/// quarter of a millisecond of a hook on a store of 13 000 rows.
pub(super) fn indexed_rows(conn: &Connection) -> Result<i64> {
    let averages: Option<Vec<u8>> = conn
        .prepare_cached("SELECT block FROM memory_fts_data WHERE id = 1")?
        .query_row([], |r| r.get(0))
        .optional()?;
    Ok(averages.map_or(0, |b| varint(&b) as i64))
}

/// More b-trees than this in the index and `merge_index` merges them.
const MAX_SEGMENTS: i64 = 4;

/// Merge the index's b-trees a bounded step towards one, when it has more
/// than `MAX_SEGMENTS`. Every reindex commits a b-tree of its own, and
/// FTS5 merges them only four at a level: a thousand tasks reindexed one
/// at a time left 22, and on 22 the terms related work looks up cost three
/// times what they did on one. A step writes at most 64 pages (about 10 ms
/// on 13 000 rows), so the write lock is never held for long; a merge goes
/// on where the last left off. What the index holds, and so every search,
/// is unchanged. Returns whether it merged.
pub fn merge_index(conn: &Connection) -> Result<bool> {
    let segments: i64 =
        conn.query_row("SELECT count(DISTINCT segid) FROM memory_fts_idx", [], |r| r.get(0))?;
    if segments <= MAX_SEGMENTS {
        return Ok(false);
    }
    conn.execute("INSERT INTO memory_fts (memory_fts, rank) VALUES ('merge', -64)", [])?;
    Ok(true)
}

/// SQLite's variable-length integer, as FTS5 writes its counts: seven
/// bits a byte, high bit set on all but the last, and a ninth byte of
/// eight. Empty is 0.
fn varint(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for (i, &b) in bytes.iter().take(9).enumerate() {
        if i == 8 {
            return (v << 8) | u64::from(b);
        }
        v = (v << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            break;
        }
    }
    v
}

/// Rows matching `q`, best first, the best of each task or project
/// memory, at most `limit`. A task's hit is a bullet or note that matched
/// before its title, which says nothing the hit's title does not; a task
/// that matched by its title alone comes after those that matched more.
fn search(
    conn: &Connection,
    q: &str,
    project: Option<&str>,
    exclude: Option<&str>,
    limit: usize,
) -> Result<Vec<Hit>> {
    // Several rows may match per task; the best of each is its hit. A row
    // of a project's cairn has no task, and its project in `scope`.
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.task, t.title, coalesce(t.project, f.scope), c.status, f.kind, f.ref,
                coalesce(f.findings, f.decisions, f.open, f.history, f.notes, f.memory, f.title),
                -bm25(memory_fts, {WEIGHTS}) AS score, f.rowid, coalesce(t.state, ''),
                coalesce(i.stale, 0)
         FROM memory_fts f LEFT JOIN task t ON t.id = f.task LEFT JOIN cairn c ON c.task = f.task
           LEFT JOIN cairn_item i ON f.kind NOT IN ('notes', 'title') AND i.id = CAST(f.ref AS INTEGER)
         WHERE memory_fts MATCH ?1 AND (f.task IS NULL OR t.id IS NOT NULL)
           AND (?2 IS NULL OR coalesce(t.project, f.scope) = ?2)
           AND (?3 IS NULL OR f.task IS NULL OR f.task != ?3)
         ORDER BY bm25(memory_fts, {WEIGHTS}) LIMIT ?4"
    ))?;
    let rows = stmt.query_map(params![q, project, exclude, (limit * 8) as i64], |r| {
        let task: Option<String> = r.get(0)?;
        let reference: String = r.get(5)?;
        Ok(Hit {
            short: task.as_deref().map_or_else(|| "project".to_string(), crate::task::short),
            title: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "project cairn".into()),
            task: task.unwrap_or_default(),
            project: r.get(2)?,
            status: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            kind: r.get(4)?,
            reference: (!reference.is_empty()).then_some(reference),
            snippet: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            score: Some(r.get(7)?),
            rowid: r.get(8)?,
            state: r.get(9)?,
            stale: r.get(10)?,
            more: 0,
        })
    })?;
    let mut hits: Vec<Hit> = Vec::new();
    for h in rows {
        let h = h?;
        match hits.iter_mut().find(|k| k.same_place(&h)) {
            // The task's best row was its title: this one says more.
            Some(k) if k.kind == "title" && h.kind != "title" => *k = Hit { more: k.more, ..h },
            // Another line of it matched: counted, not shown.
            Some(k) if h.kind != "title" => k.more += 1,
            Some(_) => {}
            None => hits.push(h),
        }
    }
    // Title-only tasks last; the order is otherwise BM25's.
    hits.sort_by_key(|h| h.kind == "title");
    hits.truncate(limit);
    Ok(hits)
}

/// Logged with who asked and what they got, so how well recall serves can
/// be measured before reaching for anything heavier than keyword search.
fn log(
    conn: &Connection,
    query: &str,
    project: Option<&str>,
    found: &Found,
    asker: Asker,
    now: i64,
) -> Result<()> {
    let results: Vec<serde_json::Value> = found
        .hits
        .iter()
        .map(|h| serde_json::json!({"task": h.task, "kind": h.kind, "ref": h.reference, "score": h.score}))
        .collect();
    conn.execute(
        "INSERT INTO recall_log (ts, query, hits, run, source, project, everywhere, results)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            now,
            query,
            found.hits.len() as i64,
            asker.run,
            asker.source,
            project,
            found.everywhere,
            serde_json::Value::Array(results).to_string()
        ],
    )?;
    Ok(())
}

/// Words that say nothing about what a task is: they match almost every
/// cairn, and once filled the slots of better hits.
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "aren",
    "arent",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "cannot",
    "cant",
    "could",
    "couldnt",
    "did",
    "didnt",
    "do",
    "does",
    "doesnt",
    "doing",
    "dont",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "get",
    "gets",
    "got",
    "had",
    "hadnt",
    "has",
    "hasnt",
    "have",
    "havent",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "isnt",
    "it",
    "its",
    "itself",
    "just",
    "let",
    "lets",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "she",
    "should",
    "shouldnt",
    "so",
    "some",
    "such",
    "than",
    "that",
    "thats",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "wasnt",
    "we",
    "were",
    "werent",
    "what",
    "whats",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "wont",
    "would",
    "wouldnt",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
    // What task titles and prompts say of everything.
    "another",
    "etc",
    "help",
    "lot",
    "lots",
    "maybe",
    "need",
    "perhaps",
    "please",
    "regarding",
    "seems",
    "still",
    "stuff",
    "sure",
    "thing",
    "things",
    "today",
    "try",
    "via",
    "want",
    "way",
    "within",
    "without",
];

/// The FTS query for what someone typed: any word may match, stemmed, as
/// a prefix from three characters; stopwords are left out; an identifier
/// or path (`OAUTH_CLIENT_ID`, `memory::recall`, `src/run.rs`) matches as
/// the phrase it tokenizes to. Quoting keeps FTS syntax out of it. `None`
/// when nothing is left to search for.
pub fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = terms(query).into_iter().map(|(t, _)| t).collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// The terms of `fts_query`, each a query of its own, and whether each
/// is an identifier or a path: specific on its own, where a hyphenated
/// word (`log-in`) is not.
fn terms(query: &str) -> Vec<(String, bool)> {
    let mut terms: Vec<(String, bool)> = Vec::new();
    for raw in query.split_whitespace() {
        let raw = raw.replace(['\'', '’'], "");
        let parts: Vec<String> = raw
            .split(|c: char| !c.is_alphanumeric())
            .filter(|p| !p.is_empty())
            .map(str::to_lowercase)
            .collect();
        let raw = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let ident = parts.len() > 1 && raw.contains(['_', '.', '/', ':']);
        let term = match parts.as_slice() {
            [] => continue,
            [w] if STOPWORDS.contains(&w.as_str()) || w.chars().count() < 2 => continue,
            [w] if w.chars().count() < 3 => format!("\"{w}\""),
            [w] => format!("\"{w}\"*"),
            [.., last] => {
                let star = if last.chars().count() >= 3 { "*" } else { "" };
                format!("\"{}\"{star}", parts.join(" "))
            }
        };
        if !terms.iter().any(|(t, _)| *t == term) {
            terms.push((term, ident));
        }
    }
    terms
}

#[cfg(test)]
mod calibrate {
    use super::*;

    /// `VARDE_RECALL_DB=<a copy of a store> cargo test --release -p varde-core -- --ignored calibrate --nocapture`:
    /// what each task's title would be offered as related work, and how
    /// long finding it took, to judge `related` against a real store.
    #[test]
    #[ignore]
    fn related_on_a_real_store() {
        let Ok(path) = std::env::var("VARDE_RECALL_DB") else { return };
        let s = Store::open(std::path::Path::new(&path), std::time::Duration::from_secs(1)).unwrap();
        let tasks: Vec<(String, String, Option<String>)> = s
            .conn
            .prepare("SELECT id, title, project FROM task ORDER BY created")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut times = Vec::new();
        for (id, title, project) in tasks {
            let terms = terms(&title);
            if terms.is_empty() {
                continue;
            }
            println!("\n## {title}");
            let specific = specific_rows(&s.conn, &terms).unwrap();
            println!(
                "   {:?}",
                terms
                    .iter()
                    .zip(&specific)
                    .filter(|(_, s)| s.is_some())
                    .map(|(t, _)| &t.0)
                    .collect::<Vec<_>>()
            );
            let started = std::time::Instant::now();
            let found = related_quietly(&s.conn, &title, project.as_deref(), Some(&id), 3).unwrap();
            times.push(started.elapsed());
            for h in found.hits {
                println!(
                    "  + {:5.2} {}  {}: {}",
                    h.score.unwrap(),
                    h.title.chars().take(30).collect::<String>(),
                    h.kind,
                    h.snippet.chars().take(90).collect::<String>()
                );
            }
        }
        times.sort();
        if let Some(max) = times.last() {
            println!("\nrelated: median {:?}, max {:?}", times[times.len() / 2], max);
        }
    }
}
