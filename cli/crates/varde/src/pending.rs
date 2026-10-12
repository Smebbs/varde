//! Events a hook could not write to the store, kept as JSON lines until
//! the next varde command applies them.

use std::fs;
use std::io::Write;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use varde_core::store::Store;

use crate::Paths;

pub fn append(
    paths: &Paths,
    agent: &str,
    event: &str,
    ts: i64,
    claim: Option<&str>,
    process: Option<(i64, i64)>,
    payload: &Value,
) -> Result<()> {
    write_line(
        paths,
        &json!({"agent": agent, "event": event, "ts": ts, "claim": claim, "process": process, "payload": payload}),
    )
}

fn write_line(paths: &Paths, line: &Value) -> Result<()> {
    paths.make_home()?;
    append_line(&paths.pending(), &line.to_string())
}

/// One write per line, so concurrent appends do not interleave.
fn append_line(path: &std::path::Path, line: &str) -> Result<()> {
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

/// A run's exit `varde run` could not record. It needs no agent to read.
pub fn append_exit(paths: &Paths, run: &str, code: i32, ts: i64) -> Result<()> {
    write_line(paths, &json!({"exit": {"run": run, "code": code}, "ts": ts}))
}

/// Apply pending events in the order they were written.
///
/// Every file is claimed before it is read, by renaming it to a name only
/// this process uses; a drain that loses the rename leaves the file to
/// whoever won. So concurrent drains (every hook drains) never apply an
/// event twice. A claim left behind by a crash is taken over after a
/// minute, judged by the time in its name: a rename keeps the file's
/// own time, so a claim just made of an old file would look abandoned.
///
/// A line that cannot be applied is moved to `pending.failed.jsonl` and
/// the rest go on: one bad event must not stop every later command. A
/// store that is merely busy keeps the remaining lines for next time.
pub fn drain(paths: &Paths, store: &mut Store) -> Result<()> {
    let now = crate::now_ms();
    let me = format!("{now}.{}", std::process::id());
    let mut files: Vec<std::path::PathBuf> = fs::read_dir(&paths.home)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // `pending.<ms>.<pid>.<i>.claim`
            let stale = || {
                n.split('.').nth(1).and_then(|ms| ms.parse::<i64>().ok()).is_some_and(|ms| now - ms > 60_000)
            };
            n == "pending.jsonl"
                || (n.starts_with("pending.") && n.ends_with(".jsonl") && !n.ends_with("failed.jsonl"))
                || (n.starts_with("pending.") && n.ends_with(".claim") && stale())
        })
        .collect();
    files.sort();
    for (i, file) in files.into_iter().enumerate() {
        let claimed = paths.home.join(format!("pending.{me}.{i}.claim"));
        if fs::rename(&file, &claimed).is_err() {
            continue; // Another drain has it.
        }
        // A write torn by a crash may leave bytes that are not UTF-8: it
        // costs that line, never the file.
        let bytes = fs::read(&claimed)?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        for (n, line) in lines.iter().enumerate() {
            if let Err(e) = apply(store, line) {
                if is_busy(&e) {
                    // Put the rest back for the next drain, in order.
                    let rest = paths.home.join(format!("pending.{me}.{i}.jsonl"));
                    fs::write(&rest, lines[n..].join("\n") + "\n")?;
                    fs::remove_file(&claimed)?;
                    return Ok(());
                }
                quarantine(paths, line, &e)?;
            }
        }
        fs::remove_file(&claimed)?;
    }
    Ok(())
}

fn is_busy(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<rusqlite::Error>().is_some_and(|r| {
            matches!(
                r.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            )
        })
    })
}

/// Keep an event that cannot be applied, with why, where `doctor` sees it.
fn quarantine(paths: &Paths, line: &str, e: &anyhow::Error) -> Result<()> {
    let entry = json!({"error": format!("{e:#}"), "line": line});
    append_line(&paths.home.join("pending.failed.jsonl"), &entry.to_string())
}

fn apply(store: &mut Store, line: &str) -> Result<()> {
    let v = serde_json::from_str::<Value>(line).context("not a JSON event")?;
    if let (Some(run), Some(code), Some(ts)) =
        (v["exit"]["run"].as_str(), v["exit"]["code"].as_i64(), v["ts"].as_i64())
    {
        return store.record_exit(run, code as i32, ts).map(drop);
    }
    let (Some(agent), Some(event), Some(ts)) = (v["agent"].as_str(), v["event"].as_str(), v["ts"].as_i64())
    else {
        return Ok(());
    };
    let Some(adapter) = varde_agent::by_name(agent) else { return Ok(()) };
    let process = || Some((v["process"][0].as_i64()?, v["process"][1].as_i64()?));
    if let Some(norm) = adapter.normalize(event, &v["payload"]) {
        store.ingest_from(
            adapter.name(),
            &norm,
            &v["payload"].to_string(),
            ts,
            v["claim"].as_str(),
            &process,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_is_taken_over_only_once_its_maker_is_long_gone() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { home: d.path().to_path_buf() };
        let mut store = paths.open(std::time::Duration::from_secs(1)).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO run (id, agent, native_id, relation, started, launch)
                 VALUES ('R1', 'claude', 'a', 'fresh', 1, 'managed'), ('R2', 'claude', 'b', 'fresh', 1, 'managed')",
                [],
            )
            .unwrap();
        let line = |run: &str| format!("{{\"exit\":{{\"run\":\"{run}\",\"code\":1}},\"ts\":2}}\n");
        // Just claimed by another drain, from a file written long ago.
        let fresh = d.path().join(format!("pending.{}.1.0.claim", crate::now_ms()));
        std::fs::write(&fresh, line("R1")).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&fresh).unwrap().set_modified(old).unwrap();
        // Left by a drain that died an hour ago.
        let dead = d.path().join(format!("pending.{}.1.0.claim", crate::now_ms() - 3_600_000));
        std::fs::write(&dead, line("R2")).unwrap();
        drain(&paths, &mut store).unwrap();
        assert!(fresh.exists());
        assert!(!dead.exists());
        let ended = |run: &str| -> i64 {
            store
                .conn
                .query_row("SELECT count(*) FROM event WHERE run = ?1 AND kind = 'ended'", [run], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!((ended("R1"), ended("R2")), (0, 1));
    }

    #[test]
    fn an_exit_left_pending_ends_its_run() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { home: d.path().to_path_buf() };
        let mut store = paths.open(std::time::Duration::from_secs(1)).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO run (id, agent, native_id, relation, started, launch)
                 VALUES ('R1', 'claude', 'a', 'fresh', 1, 'managed')",
                [],
            )
            .unwrap();
        append_exit(&paths, "R1", 3, 2).unwrap();
        drain(&paths, &mut store).unwrap();
        let (code, ended): (i64, i64) = store
            .conn
            .query_row(
                "SELECT exit_code, (SELECT count(*) FROM event WHERE run = 'R1' AND kind = 'ended')
                 FROM run WHERE id = 'R1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((code, ended), (3, 1));
    }
}
