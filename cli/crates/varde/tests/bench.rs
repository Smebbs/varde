//! The latency budgets, measured end to end on a large store: `varde
//! list --json` under 10 ms and a hook under 5 ms at p99, with 1 000
//! tasks of 2 runs and 500 events each, every task with a cairn of ten
//! bullets and two notes, and a project cairn. The hooks that hand a
//! session its memory (a fresh start, a resume, a compaction, an ad-hoc
//! session's first prompt) are timed on their own. Ignored by default,
//! and only meaningful in a release build:
//!
//! ```sh
//! cargo test --release -p varde --test bench -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TASKS: i64 = 1000;
const OPEN: i64 = 100;
const EVENTS: i64 = 500;
const PROJECT: &str = "/tmp/bench";

fn varde_out(home: &Path, args: &[&str], stdin: &str, claim: Option<&str>) -> (Duration, String) {
    let started = Instant::now();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_varde"));
    cmd.args(args)
        .env("VARDE_HOME", home)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("VARDE_INTERNAL")
        .env_remove("VARDE_RUN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(run) = claim {
        cmd.env("VARDE_RUN", run);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let took = started.elapsed();
    assert!(out.status.success());
    (took, String::from_utf8_lossy(&out.stdout).into_owned())
}

fn varde(home: &Path, args: &[&str], stdin: &str) -> Duration {
    varde_out(home, args, stdin, None).0
}

/// Words for titles, bullets and notes: a few common, most rare, as in a
/// real store, where `varde` is in every other row and most words in few.
struct Words(u64);

impl Words {
    const VOCAB: usize = 600;

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn word(&mut self) -> String {
        // Skewed: low indexes come up far more often.
        let u = (self.next() % 10_000) as f64 / 10_000.0;
        let i = (u * u * Self::VOCAB as f64) as usize;
        let w = Self::spell(i);
        match i % 7 {
            0 => format!("`{w}_{}`", Self::spell(i + 7)),
            1 => format!("src/{w}.rs"),
            _ => w,
        }
    }

    /// Word `i` of the vocabulary: letters that read as a word, and
    /// share no more of a start with each other than words do.
    fn spell(i: usize) -> String {
        const C: &[u8] = b"bcdfghklmnprstvz";
        const V: &[u8] = b"aeiou";
        let mut h = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut w = String::new();
        for k in 0..(4 + i % 4) {
            let set = if k % 2 == 0 { C } else { V };
            w.push(set[(h % set.len() as u64) as usize] as char);
            h /= set.len() as u64;
        }
        w
    }

    fn text(&mut self, n: usize) -> String {
        (0..n).map(|_| self.word()).collect::<Vec<_>>().join(" ")
    }
}

fn q(s: &str) -> String {
    s.replace('\'', "''")
}

/// 1 000 tasks: 100 open and 900 closed 20 days ago. Each has an older
/// run from ten days ago and a newer one from today, each with 500
/// events whose payloads are the size real ones are, and memory: a
/// cairn of ten bullets, two notes, and one project cairn for all.
fn fill(home: &Path) {
    varde(home, &["list", "--json"], "");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let day = 86_400_000;
    let sql = format!(
        "BEGIN;
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < {TASKS} - 1)
         INSERT INTO task (id, title, project, state, inbox, created, closed)
         SELECT printf('01BENCH%019d', i), 'bench task ' || i, '{PROJECT}',
                CASE WHEN i < {OPEN} THEN 'open' ELSE 'done' END, 0, {now} - 30 * {day},
                CASE WHEN i < {OPEN} THEN NULL ELSE {now} - 20 * {day} END
         FROM n;
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 2 * {TASKS} - 1)
         INSERT INTO run (id, task, agent, native_id, relation, cwd, started, transcript)
         SELECT printf('01RUN%021d', i), printf('01BENCH%019d', i / 2), 'claude', printf('native-%d', i),
                'fresh', '{PROJECT}', {now} - CASE WHEN i % 2 = 0 THEN 10 * {day} ELSE {day} END,
                printf('/tmp/bench/native-%d.jsonl', i)
         FROM n;
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < {EVENTS} - 1)
         INSERT INTO event (run, kind, ts, raw)
         SELECT r.id,
                CASE WHEN n.i % 50 = 0 THEN 'prompt' WHEN n.i % 50 = 49 THEN 'turn_end'
                     WHEN n.i % 50 = 48 THEN 'idle' ELSE 'unblocked' END,
                r.started + n.i * 1000,
                printf('{{\"session_id\":\"%s\",\"prompt\":\"%s\"}}', r.native_id, hex(randomblob(150)))
         FROM run r, n;
         COMMIT;"
    );
    sqlite(home, &sql);

    // Memory, with the search index recall keeps of it.
    let mut w = Words(0x9E37_79B9_7F4A_7C15);
    let mut sql = String::from("BEGIN;\n");
    let mut id = 0;
    for t in 0..TASKS {
        let task = format!("01BENCH{t:019}");
        let title = format!("Fix {} in {}", w.text(2), w.text(2)).replace('`', "");
        writeln!(sql, "UPDATE task SET title = '{}' WHERE id = '{task}';", q(&title)).unwrap();
        writeln!(
            sql,
            "INSERT INTO memory_fts (task, kind, ref, title) VALUES ('{task}', 'title', '', '{}');",
            q(&title)
        )
        .unwrap();
        writeln!(
            sql,
            "INSERT INTO cairn (task, status, version, updated) VALUES ('{task}', '{}', 3, {now});",
            q(&w.text(8))
        )
        .unwrap();
        for b in 0..10 {
            id += 1;
            let section = if b < 7 { "findings" } else { "decisions" };
            let text = w.text(14);
            writeln!(
                sql,
                "INSERT INTO cairn_item (id, task, section, pos, text, anchor, author, created, confirmed, state)
                 VALUES ({id}, '{task}', '{section}', {b}, '{t2}', 'fn_{b}', 'job', {now}, {now}, 'current');
                 INSERT INTO memory_fts (task, kind, ref, {section}) VALUES ('{task}', '{section}', '{id}', '{t2}');",
                t2 = q(&text)
            )
            .unwrap();
        }
        for n in 0..2 {
            let text = w.text(30);
            let run = format!("01RUN{:021}", 2 * t + 1);
            writeln!(
                sql,
                "INSERT INTO note (id, run, text, ts, folded) VALUES ('N{t}-{n}', '{run}', '{t2}', {now}, 1);
                 INSERT INTO memory_fts (task, kind, ref, notes) VALUES ('{task}', 'notes', 'N{t}-{n}', '{t2}');",
                t2 = q(&text)
            )
            .unwrap();
        }
    }
    writeln!(sql, "INSERT INTO project_cairn (project, version, updated) VALUES ('{PROJECT}', 4, {now});")
        .unwrap();
    for (b, section) in ["conventions", "gotchas", "gotchas", "gotchas", "howto", "howto"].iter().enumerate()
    {
        id += 1;
        let text = w.text(18);
        writeln!(
            sql,
            "INSERT INTO cairn_item (id, project, section, pos, text, anchor, author, created, confirmed, state)
             VALUES ({id}, '{PROJECT}', '{section}', {b}, '{t2}', 'fn_{b}', 'job', {now}, {now}, 'current');
             INSERT INTO memory_fts (memory, kind, ref, scope) VALUES ('{t2}', '{section}', '{id}', '{PROJECT}');",
            t2 = q(&text)
        )
        .unwrap();
    }
    sql.push_str("COMMIT;\n");
    sqlite(home, &sql);
}

fn sqlite(home: &Path, sql: &str) {
    rusqlite::Connection::open(home.join("varde.db")).unwrap().execute_batch(sql).unwrap();
}

fn percentile(times: &mut [Duration], p: f64) -> Duration {
    times.sort();
    times[((times.len() as f64 * p) as usize).min(times.len() - 1)]
}

fn payload(session: &str, event: &str, source: &str, prompt: &str) -> String {
    format!(
        "{{\"session_id\":\"{session}\",\"cwd\":\"{PROJECT}\",\"hook_event_name\":\"{event}\",\
          \"transcript_path\":\"/tmp/bench/{session}.jsonl\",\"prompt\":\"{prompt}\",\"source\":\"{source}\"}}"
    )
}

#[test]
#[ignore]
fn list_and_hooks_within_budget_on_a_large_store() {
    let home = tempfile::tempdir().unwrap();
    fill(home.path());
    // `VARDE_BENCH_KEEP=<dir>`: a copy of the filled store, to profile by hand.
    if let Some(keep) = std::env::var_os("VARDE_BENCH_KEEP") {
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::copy(home.path().join("varde.db"), Path::new(&keep).join("varde.db")).unwrap();
    }
    for _ in 0..3 {
        varde(home.path(), &["list", "--json"], "");
    }
    let mut list: Vec<Duration> = (0..50).map(|_| varde(home.path(), &["list", "--json"], "")).collect();
    let (median_list, p90) = (percentile(&mut list, 0.5), percentile(&mut list, 0.9));
    println!("varde list --json: median {median_list:?}, p90 {p90:?}");

    // A busy session on an open task: tool runs mostly, a turn now and then.
    let events = ["UserPromptSubmit", "PostToolUse", "PostToolUse", "PostToolUse", "Stop"];
    let mut hooks: Vec<Duration> = (0..200)
        .map(|i| events[i % events.len()])
        .map(|e| varde(home.path(), &["hook", e], &payload("native-1", e, "startup", "more work")))
        .collect();
    let (median, p99) = (percentile(&mut hooks, 0.5), percentile(&mut hooks, 0.99));
    println!("varde hook: median {median:?}, p99 {p99:?}");

    // The hooks that hand a session its memory. A fresh start of a known
    // task, claimed by a run `start` prepared; a resume of a conversation;
    // a compaction; and the first prompt of an ad-hoc session, which makes
    // its task and searches for what the prompt bears on.
    sqlite(
        home.path(),
        &format!(
            "INSERT INTO run (id, task, agent, native_id, relation, cwd, started, launch)
             VALUES ('01RUNBENCHLAUNCHED00000000', '01BENCH0000000000000000003', 'claude', 'launched', 'fresh',
                     '{PROJECT}', 1, 'managed');"
        ),
    );
    let claim = Some("01RUNBENCHLAUNCHED00000000");
    let (_, given) = varde_out(
        home.path(),
        &["hook", "SessionStart"],
        &payload("launched", "SessionStart", "startup", ""),
        claim,
    );
    assert!(given.contains("its cairn") && given.contains("in this project recorded"), "{given}");
    // Taken in turns, so a busy moment on the machine falls on every
    // kind alike; a plain turn end beside them is the yardstick.
    let mut words = Words(0xD1B5_4A32_D192_ED03);
    let kinds = ["turn end", "start, no task", "startup", "resume", "compact", "first prompt"];
    let mut times: Vec<Vec<Duration>> = vec![Vec::new(); kinds.len()];
    for i in 0..300 {
        for (k, kind) in kinds.iter().enumerate() {
            let (event, body, claim) = match *kind {
                "turn end" => ("Stop", payload("native-1", "Stop", "", ""), None),
                "start, no task" => {
                    ("SessionStart", payload(&format!("bare-{i}"), "SessionStart", "startup", ""), None)
                }
                "startup" => ("SessionStart", payload("launched", "SessionStart", "startup", ""), claim),
                "resume" => ("SessionStart", payload("native-7", "SessionStart", "resume", ""), None),
                "compact" => ("SessionStart", payload("launched", "SessionStart", "compact", ""), claim),
                _ => {
                    let prompt =
                        format!("Why does {} fail when {}", words.text(4), words.text(4)).replace('`', "");
                    let session = format!("adhoc-{i}");
                    ("UserPromptSubmit", payload(&session, "UserPromptSubmit", "", &prompt), None)
                }
            };
            times[k].push(varde_out(home.path(), &["hook", event], &body, claim).0);
        }
    }
    let mut worst = Duration::ZERO;
    for (kind, mut t) in kinds.iter().zip(times) {
        let (median, p90, p99) = (percentile(&mut t, 0.5), percentile(&mut t, 0.9), percentile(&mut t, 0.99));
        let what = if kind.contains(',') || *kind == "turn end" { "" } else { ", with memory" };
        println!("varde hook ({kind}{what}): median {median:?}, p90 {p90:?}, p99 {p99:?}");
        worst = worst.max(p99);
    }

    if !cfg!(debug_assertions) {
        assert!(median_list < Duration::from_millis(10), "list median {median_list:?}");
        assert!(p99 < Duration::from_millis(5), "hook p99 {p99:?}");
        // Once per session, beside an agent that takes a second to start.
        assert!(worst < Duration::from_millis(10), "memory hook p99 {worst:?}");
    }
}
