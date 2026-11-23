//! `varde follow <task>`: watch a task's sessions as they work, read-only.
//! What each says, the tools it calls and what it was asked, read from its
//! transcript as it grows; each check as it lands; and, while a loop goes
//! on, the next round as it starts. It writes nothing, so it never stands
//! in a session's way: a background round goes on as it would.

use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rusqlite::OptionalExtension;
use varde_core::flow::{self as core, State};
use varde_core::task;

use crate::{now_ms, Paths};

/// How much of a session already under way is shown first.
const BACKLOG: u64 = 20_000;

/// The newest run of the task: id, agent, transcript, round, profile,
/// model, exit code, whether it runs in the background, and in a sandbox.
type Newest = (
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i32>,
    bool,
    bool,
    bool,
);

struct Following {
    run: String,
    agent: Box<dyn varde_agent::Agent>,
    transcript: Option<PathBuf>,
    offset: u64,
    ended: bool,
}

pub fn run(paths: &Paths, reference: &str) -> Result<()> {
    let store = paths.open(Duration::from_secs(2))?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    let title: String = store.conn.query_row("SELECT title FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
    // RET or q stops it, in a terminal; elsewhere, closing it does.
    let stop = Arc::new(AtomicBool::new(false));
    if unsafe { libc::isatty(0) } == 1 {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = std::io::stdin().lock().lines().next();
            stop.store(true, Ordering::Relaxed);
        });
        println!("following {short} ({title}); RET stops");
    } else {
        println!("following {short} ({title})");
    }
    let alive = |pid: i64, start: i64| crate::process::alive(pid, start);
    let mut current: Option<Following> = None;
    let mut told: Option<i64> = None;
    let mut first = true;
    let mut quiet_since = now_ms();
    while !stop.load(Ordering::Relaxed) {
        let newest: Option<Newest> = store
            .conn
            .query_row(
                "SELECT id, agent, transcript, round, profile, model, exit_code, coalesce(background, 0),
                            coalesce(fenced, 0), coalesce(pid, 0) > 0 OR exit_code IS NOT NULL
                     FROM run WHERE task = ?1 ORDER BY started DESC, rowid DESC LIMIT 1",
                [&id],
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
            )
            .optional()?;
        let Some((run, agent, transcript, round, profile, model, exit, background, fenced, started)) = newest
        else {
            if first {
                println!("{short} has no session yet; waiting for one");
                first = false;
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        // Not until it has started: what it runs in is known by then.
        if !started {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        if current.as_ref().is_none_or(|c| c.run != run) {
            let Some(adapter) = varde_agent::by_name(&agent) else { break };
            let what = match (round, background) {
                (Some(n), _) if n > 0 => format!("round {n}"),
                (_, true) => "background session".to_string(),
                _ => "session".to_string(),
            };
            let on = match (profile, model) {
                (Some(p), Some(m)) => format!(" · {p} ({m})"),
                (Some(p), None) => format!(" · {p}"),
                (None, Some(m)) => format!(" · {m}"),
                (None, None) => String::new(),
            };
            let walled = if fenced { " · in a sandbox" } else { "" };
            println!("\n── {what}{on}{walled} · run {}", task::short(&run));
            // Under way already: only its end. A new one: from its start.
            let offset = match transcript.as_deref() {
                Some(t) if first => size(Path::new(t)).saturating_sub(BACKLOG),
                _ => 0,
            };
            current =
                Some(Following { run: run.clone(), agent: adapter, transcript: None, offset, ended: false });
            told = None;
        }
        first = false;
        let c = current.as_mut().expect("a run followed");
        if c.transcript.is_none() {
            c.transcript = transcript.map(PathBuf::from);
        }
        let mut said = false;
        if let Some(t) = &c.transcript {
            let (lines, end) = new_lines(t, c.offset);
            c.offset = end;
            for l in lines {
                for shown in c.agent.follow(&l) {
                    // Nothing a session wrote steers the terminal.
                    let shown: String =
                        shown.chars().map(|c| if c.is_control() && c != '\t' { '?' } else { c }).collect();
                    println!("{shown}");
                    said = true;
                }
            }
        }
        if said {
            quiet_since = now_ms();
        }
        // Each check of it, once.
        if let Some(check) =
            core::checks(&store.conn, &id, 0)?.into_iter().rev().find(|k| k.run.as_deref() == Some(&run))
        {
            if told != Some(check.id) {
                told = Some(check.id);
                println!("── check: {}", crate::flow::result_line(&check, now_ms()));
            }
        }
        if exit.is_some() && !c.ended {
            c.ended = true;
            println!(
                "── ended{}",
                exit.filter(|e| *e != 0).map(|e| format!(" (exit {e})")).unwrap_or_default()
            );
        }
        if exit.is_some() {
            // What comes next: a loop's next round, or nothing.
            match core::state(&store.conn, &id, now_ms(), &alive)? {
                Some(
                    s @ (State::Working { .. }
                    | State::Checking { .. }
                    | State::Due { .. }
                    | State::Attended { .. }),
                ) => {
                    let _ = s;
                }
                Some(s) => {
                    println!("── loop: {}", crate::flow::state_words(&s));
                    break;
                }
                // No loop: once its check, if it has one, is in.
                None => {
                    let checked = crate::flow::def(&store, &id)?.is_none() || told.is_some();
                    if checked || now_ms() - quiet_since > 120_000 {
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    Ok(())
}

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// The whole lines of `path` after `from`, and where they end. A start
/// within a line skips to the next.
fn new_lines(path: &Path, from: u64) -> (Vec<String>, u64) {
    let Ok(mut f) = varde_agent::open_transcript(path) else { return (vec![], from) };
    if f.seek(SeekFrom::Start(from)).is_err() {
        return (vec![], from);
    }
    let mut bytes = Vec::new();
    if f.read_to_end(&mut bytes).is_err() {
        return (vec![], from);
    }
    let Some(last) = bytes.iter().rposition(|b| *b == b'\n') else { return (vec![], from) };
    let whole = &bytes[..=last];
    let text = String::from_utf8_lossy(whole);
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    // Begun mid-line: that line is not whole.
    if from > 0
        && !lines.is_empty()
        && !std::fs::File::open(path).is_ok_and(|mut f| starts_line(&mut f, from))
    {
        lines.remove(0);
    }
    (lines, from + whole.len() as u64)
}

/// Whether byte `at` of `f` begins a line.
fn starts_line(f: &mut std::fs::File, at: u64) -> bool {
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(at - 1)).is_ok() && f.read_exact(&mut b).is_ok() && b[0] == b'\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whole_lines_are_read_and_a_start_mid_line_skips_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        std::fs::write(&p, "one\ntwo\nthr").unwrap();
        let (lines, end) = new_lines(&p, 0);
        assert_eq!((lines, end), (vec!["one".to_string(), "two".to_string()], 8));
        let (lines, _) = new_lines(&p, 2);
        assert_eq!(lines, vec!["two".to_string()]);
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        assert_eq!(new_lines(&p, 8).0, vec!["three".to_string()]);
    }
}
