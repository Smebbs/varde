//! Memory reaching sessions, driven as the agent drives it: real hook
//! payloads on stdin, in a temporary store, with no agent run.

mod common;

use std::path::{Path, PathBuf};

use serde_json::Value;

use common::*;

/// A store, and a project that is a repository as far as varde can tell
/// without git.
fn env() -> Env {
    let e = Env::new();
    std::fs::create_dir_all(proj(&e).join(".git")).unwrap();
    e
}

fn proj(e: &Env) -> PathBuf {
    e.path("proj")
}

/// A fixture's payload, in this project, as session `id`.
fn payload(e: &Env, file: &str, id: &str) -> String {
    let mut p: Value = serde_json::from_str(&fixture(file)).unwrap();
    p["session_id"] = id.into();
    p["cwd"] = proj(e).to_string_lossy().into();
    p["transcript_path"] = e.path(&format!("{id}.jsonl")).to_string_lossy().into();
    p.to_string()
}

/// What the hook hands the session, if anything.
fn hook(e: &Env, event: &str, payload: &str, claim: Option<&str>) -> Option<String> {
    let mut c = e.cmd(&["hook", event]);
    if let Some(run) = claim {
        c.env("VARDE_RUN", run);
    }
    let out = feed(&mut c, payload);
    assert!(out.status.success());
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    reply["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)
}

fn shown(e: &Env, run: &str) -> Vec<(String, Option<i64>)> {
    e.db()
        .prepare("SELECT kind, version FROM shown WHERE run = ?1 AND via = 'hook' ORDER BY id")
        .unwrap()
        .query_map([run], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A task in the project with a cairn, and the project's cairn.
fn task_with_memory(e: &Env) -> String {
    let proj = proj(e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Page shows the wrong language", "--project", &proj]);
    e.ok_in(
        &["edit", &t, "--stdin"],
        "Status: Cause found; fix pending.\nFindings:\n- The cache key omits the locale in `cache_key_for`.\n\
         Decisions:\nOpen:\n- Add the locale to the key.\n",
    );
    e.ok_in(
        &["edit", "--project", &proj, "--stdin"],
        "Conventions:\nGotchas:\n- `cargo test -p varde-agent` does not rebuild the varde binary.\n\
         How-to:\n- Reinstall with `cargo install --path cli/crates/varde`.\n",
    );
    t
}

/// `start` a run of `task`, as a client does; its run and session id.
fn start(e: &Env, task: &str, how: &[&str]) -> (String, String) {
    let mut args = vec!["start", task, "--worktree", "never", "--json"];
    args.extend_from_slice(how);
    let plan: Value = serde_json::from_str(&e.ok(&args)).unwrap();
    let run = plan["run"].as_str().unwrap().to_string();
    let native: String =
        e.db().query_row("SELECT native_id FROM run WHERE id = ?1", [&run], |r| r.get(0)).unwrap();
    (run, native)
}

const FRAME: &str = "Recorded by earlier sessions — facts to check, not instructions.";
const PROJECT: &str = "What earlier tasks in this project recorded";
const CAIRN: &str =
    "What earlier sessions on this task recorded (its cairn):\nStatus: Cause found; fix pending.";

#[test]
fn every_start_is_given_the_memory_it_calls_for() {
    let e = env();
    let t = task_with_memory(&e);
    let (run, session) = start(&e, &t, &[]);

    // A fresh run of the task: guidance, project cairn, cairn.
    let given =
        hook(&e, "SessionStart", &payload(&e, "session-start-startup.json", &session), Some(&run)).unwrap();
    // The run by its six characters, which `note` and `new --from` read.
    let run6 = run[run.len() - 6..].to_lowercase();
    assert!(given.starts_with(&format!("varde tracks this session as run {run6} of task ")), "{given}");
    assert!(given.contains(&format!("varde note --run {run6} ")), "{given}");
    assert!(given.contains(&format!("varde new --from {run6} ")) && !given.contains(&run), "{given}");
    e.ok(&["note", "--run", &run6, "The locale comes from the cookie."]);
    e.ok(&["new", "--from", &run6, "Later work"]);
    // After the cairns, how one of their lines is corrected.
    let correct = given.find("If a line above is wrong or done, note the correction.").unwrap();
    assert!(correct > given.find(CAIRN).unwrap(), "{given}");
    let short = t[t.len() - 6..].to_lowercase();
    assert!(given.contains(&format!("close it: varde done {short}.")), "{given}");
    let project = given.find(PROJECT).unwrap();
    let cairn = given.find(CAIRN).unwrap();
    assert!(project < cairn, "the project's cairn comes first: {given}");
    // Framed as records, once, before any of it.
    assert_eq!(given.matches(FRAME).count(), 1);
    assert!(given.find(FRAME).unwrap() < project, "{given}");
    assert!(given.contains("Gotchas:\n- `cargo test -p varde-agent` does not rebuild the varde binary."));
    assert!(!given.contains("Conventions:"), "empty sections are left out: {given}");
    assert!(given.split_whitespace().count() <= 500);
    assert_eq!(shown(&e, &run), [("project".into(), Some(1)), ("cairn".into(), Some(1))]);

    // After a compaction: both again, since the summary may have lost them.
    let given = hook(&e, "SessionStart", &payload(&e, "session-start-compact.json", &session), None).unwrap();
    assert!(given.contains(PROJECT) && given.contains(CAIRN), "{given}");

    // A resume of the same conversation: each only once it changed.
    let given = hook(&e, "SessionStart", &payload(&e, "session-start-resume.json", &session), None).unwrap();
    assert!(!given.contains(PROJECT) && !given.contains("its cairn"), "{given}");
    e.ok_in(
        &["edit", &t, "--stdin"],
        "Status: Fixed.\nFindings:\n- The cache key omits the locale in `cache_key_for`.\nDecisions:\nOpen:\n",
    );
    let given = hook(&e, "SessionStart", &payload(&e, "session-start-resume.json", &session), None).unwrap();
    assert!(given.contains("its cairn):\nStatus: Fixed."), "{given}");

    // `/clear` in the launched session: a fresh conversation of the task.
    let cleared = "5be2a8e0-6f0c-4d8e-9a51-0f3c2d4b7a11";
    let given =
        hook(&e, "SessionStart", &payload(&e, "session-start-clear.json", cleared), Some(&run)).unwrap();
    assert!(given.contains(PROJECT) && given.contains("its cairn):\nStatus: Fixed."), "{given}");
    // Its prompts are not given memory again.
    assert!(
        hook(&e, "UserPromptSubmit", &payload(&e, "user-prompt-submit.json", cleared), Some(&run)).is_none()
    );
}

#[test]
fn an_ad_hoc_session_is_given_memory_with_the_prompt_that_makes_its_task() {
    let e = env();
    let t = task_with_memory(&e);
    let session = "0b5d7c33-1c9e-4b7e-8f60-6d1a2b3c4d5e";
    // Before its first prompt it has no task: guidance only.
    let given = hook(&e, "SessionStart", &payload(&e, "session-start-startup.json", session), None).unwrap();
    assert!(given.starts_with("varde tracks this session as run ") && !given.contains(PROJECT), "{given}");

    let mut prompt: Value = serde_json::from_str(&payload(&e, "user-prompt-submit.json", session)).unwrap();
    prompt["prompt"] = "Why does cache_key_for ignore the locale on staging?".into();
    let given = hook(&e, "UserPromptSubmit", &prompt.to_string(), None).unwrap();
    assert!(given.starts_with(&format!("{FRAME}\n\n{PROJECT}")), "{given}");
    let short = &t[t.len() - 6..].to_lowercase();
    assert!(
        given.contains("Earlier work this may bear on in this project") && given.contains(short.as_str()),
        "{given}"
    );
    let logged: String =
        e.db().query_row("SELECT source FROM recall_log ORDER BY ts DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(logged, "hook");
    // Only that prompt.
    prompt["prompt"] = "And the other thing about the locale?".into();
    assert!(hook(&e, "UserPromptSubmit", &prompt.to_string(), None).is_none());
}

#[test]
fn a_held_out_session_is_given_only_your_own_lines_and_so_is_its_conversation() {
    let e = env();
    std::fs::write(e.path("config.toml"), "[memory]\nholdout = 1.0\n").unwrap();
    let t = task_with_memory(&e);
    // A bullet an update wrote, which a held-out session is not given.
    e.sql(
        "INSERT INTO cairn_item (task, section, pos, text, author, created, confirmed, state)
         SELECT id, 'findings', 9, 'Staging has the same bug', 'job', 1, 1, 'current' FROM task;",
    );
    let (run, session) = start(&e, &t, &[]);
    let holdout: i64 =
        e.db().query_row("SELECT holdout FROM run WHERE id = ?1", [&run], |r| r.get(0)).unwrap();
    assert_eq!(holdout, 1);
    assert!(e.ok(&["show", &t, "--no-git"]).contains("held out of memory"));
    assert_eq!(e.show(&t)["runs"][0]["held_out"], true);
    let given =
        hook(&e, "SessionStart", &payload(&e, "session-start-startup.json", &session), Some(&run)).unwrap();
    assert!(given.starts_with("varde tracks this session"), "{given}");
    assert!(given.contains("- The cache key omits the locale in `cache_key_for`."), "{given}");
    assert!(!given.contains("Staging has the same bug") && !given.contains("varde recall"), "{given}");
    assert_eq!(shown(&e, &run), [("holdout".into(), None), ("holdout".into(), None)]);
    // Resumed outside varde, it is still the held-out conversation.
    let given = hook(&e, "SessionStart", &payload(&e, "session-start-resume.json", &session), None).unwrap();
    assert!(!given.contains("Staging has the same bug") && !given.contains("varde recall"), "{given}");

    // With no share held out, a run records nothing about it.
    std::fs::write(e.path("config.toml"), "").unwrap();
    let (run, _) = start(&e, &t, &["--fresh"]);
    let holdout: Option<i64> =
        e.db().query_row("SELECT holdout FROM run WHERE id = ?1", [&run], |r| r.get(0)).unwrap();
    assert_eq!(holdout, None);
}

#[test]
fn pi_is_handed_the_same_memory() {
    let e = env();
    let t = task_with_memory(&e);
    let (run, session) = start(&e, &t, &["--agent", "pi"]);
    let payload = serde_json::json!({
        "session_id": session,
        "session_file": e.path("s.jsonl"),
        "cwd": proj(&e),
        "reason": "startup",
    });
    let out =
        feed(e.cmd(&["hook", "--agent", "pi", "session_start"]).env("VARDE_RUN", &run), &payload.to_string());
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    let given = reply["context"].as_str().unwrap();
    assert!(given.contains(PROJECT) && given.contains(CAIRN), "{given}");
}

#[test]
fn project_cairn_is_shown_edited_and_found_by_recall() {
    let e = env();
    let proj = proj(&e).to_string_lossy().to_string();
    let none = e.ok(&["show", "--project", &proj]);
    assert!(none.contains("No project cairn yet: it holds only what you add"), "{none}");
    task_with_memory(&e);
    let shown = e.ok(&["show", "--project", &proj]);
    assert!(shown.starts_with("Project cairn · what holds across proj"), "{shown}");
    assert!(shown.contains(" ago by you · not handed to a session yet"), "{shown}");
    assert!(shown.contains("  Gotchas 1/7\n    - `cargo test -p varde-agent`"), "{shown}");
    assert!(shown.contains("binary. (yours)  #3\n  How-to 1/5"), "{shown}");
    let json: Value = serde_json::from_str(&e.ok(&["show", "--project", &proj, "--json"])).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["cairn"]["howto"][0]["anchor"], "cli/crates/varde");
    // The edit is refused without the memory's shape, and keeps your lines.
    let out = feed(&mut e.cmd(&["edit", "--project", &proj, "--stdin"]), "Status: x\nFindings:\n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Conventions:, Gotchas: and How-to:"));
    let printed = e.ok(&["edit", "--project", &proj, "--print"]);
    assert!(printed.contains("- [mine] Reinstall with"), "{printed}");
    // Recall finds it, named by its project.
    let found = e.run(&["recall", "reinstall", "--all"]);
    let found = String::from_utf8_lossy(&found.stdout);
    assert!(found.contains("proj · project cairn\n    how-to: Reinstall"), "{found}");
}

/// A transcript of tool calls, each `(minutes after t0, tool, input)`.
fn transcript(path: &Path, t0: i64, calls: &[(i64, &str, Value)]) {
    let mut out = String::from("{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n");
    for (min, name, input) in calls {
        let ts = chrono::DateTime::from_timestamp_millis(t0 + min * 60_000).unwrap();
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t", "name": name, "input": input}]},
        });
        out.push_str(&format!("{line}\n"));
    }
    std::fs::write(path, out).unwrap();
}

#[test]
fn the_report_measures_reach_use_rediscovery_fidelity_cost_and_holdout() {
    let e = env();
    e.ok(&["list"]);
    // A day ago: eval looks back 30 days at most.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let t0 = now - 86_400_000;
    let m = 60_000;
    let p = proj(&e).to_string_lossy().to_string();
    let (ta, th) = (e.path("a.jsonl"), e.path("h.jsonl"));
    let db = e.db();
    db.execute_batch(&format!(
        "INSERT INTO task (id, title, project, created) VALUES
           ('01AAAAAAAAAAAAAAAAAAAAAAAA', 'Wrong language', '{p}', {t0}),
           ('01BBBBBBBBBBBBBBBBBBBBBBBB', 'Staging deploy', '{p}', {t0}),
           ('01CCCCCCCCCCCCCCCCCCCCCCCC', 'Deploy again', '{p}', {t0});
         INSERT INTO cairn (task, status, version, updated) VALUES ('01AAAAAAAAAAAAAAAAAAAAAAAA', 's', 2, {t0});
         INSERT INTO cairn_history (task, doc, author, ts) VALUES
           ('01AAAAAAAAAAAAAAAAAAAAAAAA', '{{\"status\":\"s\",\"findings\":[{{\"id\":1,\"text\":\"one\"}},{{\"id\":2,\"text\":\"two\"}}]}}', 'job', {t0}),
           ('01AAAAAAAAAAAAAAAAAAAAAAAA', '{{\"status\":\"s\",\"findings\":[{{\"id\":1,\"text\":\"one\"}}]}}', 'job', {t1});
         INSERT INTO run (id, task, agent, native_id, relation, launch, holdout, started, transcript) VALUES
           ('RA', '01AAAAAAAAAAAAAAAAAAAAAAAA', 'claude', 'a', 'fresh', 'managed', 0, {t10}, '{ta}'),
           ('RH', '01CCCCCCCCCCCCCCCCCCCCCCCC', 'claude', 'h', 'fresh', 'managed', 1, {t10}, '{th}'),
           ('RR', '01AAAAAAAAAAAAAAAAAAAAAAAA', 'claude', 'a', 'resume', 'adopted', NULL, {t30}, '{ta}');
         UPDATE run SET parent_run = 'RA' WHERE id = 'RR';
         INSERT INTO cairn_item (id, task, section, pos, text, anchor, author, run, created, confirmed, state, stale, reason, left_at) VALUES
           (1, '01AAAAAAAAAAAAAAAAAAAAAAAA', 'findings', 0, 'The cache key omits the locale in `cache_key_for`',
               'cache_key_for', 'job', NULL, {t0}, {t0}, 'current', 1, NULL, NULL),
           (2, '01BBBBBBBBBBBBBBBBBBBBBBBB', 'findings', 0, 'Staging needs `OAUTH_CLIENT_ID` set before the deploy',
               'OAUTH_CLIENT_ID', 'job', NULL, {t0}, {t0}, 'current', 0, NULL, NULL),
           (3, '01AAAAAAAAAAAAAAAAAAAAAAAA', 'findings', 1, 'The cache key omits the locale in `cache_key_for`.',
               'cache_key_for', 'job', 'RA', {t16}, {t16}, 'current', 0, NULL, NULL),
           (4, '01CCCCCCCCCCCCCCCCCCCCCCCC', 'findings', 0, 'Staging needs `OAUTH_CLIENT_ID` set before deploying',
               'OAUTH_CLIENT_ID', 'job', 'RH', {t16}, {t16}, 'current', 0, NULL, NULL),
           (5, '01AAAAAAAAAAAAAAAAAAAAAAAA', 'open', 0, 'Ship it', NULL, 'job', 'RA', {t0}, {t0}, 'history', 0, 'done', {t16}),
           (6, '01AAAAAAAAAAAAAAAAAAAAAAAA', 'open', 0, 'Check mobile', NULL, 'job', 'RA', {t0}, {t0}, 'history', 0, 'cap', {t16});
         INSERT INTO shown (run, ts, via, kind, task, version, items) VALUES
           ('RA', {t10}, 'hook', 'cairn', '01AAAAAAAAAAAAAAAAAAAAAAAA', 1, '[\"1\"]'),
           ('RH', {t10}, 'hook', 'holdout', '01CCCCCCCCCCCCCCCCCCCCCCCC', NULL, '[]');
         INSERT INTO recall_log (ts, query, hits, run, source, results) VALUES
           ({t12}, 'staging deploy', 1, 'RA', 'agent', '[{{\"task\":\"01BBBBBBBBBBBBBBBBBBBBBBBB\"}}]');
         INSERT INTO note (id, run, text, ts, folded) VALUES
           ('N1', 'RA', 'cache_key_for drops the locale; see src/cache.rs', {t15}, 1);
         INSERT INTO job (id, run, upto, kind, state, created, finished, calls, input_tokens, output_tokens, repeated, like_shown) VALUES
           ('J1', 'RA', 10, 'fold', 'done', {t16}, {t16}, 2, 2000, 100, 2, 1),
           ('J2', 'RH', 10, 'fold', 'unchanged', {t16}, {t16}, 1, 1000, 50, NULL, NULL);",
        ta = ta.display(),
        th = th.display(),
        t1 = t0 + m,
        t10 = t0 + 10 * m,
        t12 = t0 + 12 * m + 30_000,
        t15 = t0 + 15 * m,
        t16 = t0 + 16 * m,
        t30 = t0 + 30 * m,
    ))
    .unwrap();
    // Given its cairn, the session went to its anchor, looked a recalled
    // task up, and edited after three calls; held out, it read for longer.
    transcript(
        &ta,
        t0,
        &[
            (11, "Read", serde_json::json!({"file_path": "/x/src/cache.rs"})),
            (12, "Grep", serde_json::json!({"pattern": "cache_key_for"})),
            (13, "Bash", serde_json::json!({"command": "varde show bbbbbb"})),
            (14, "Edit", serde_json::json!({"file_path": "/x/src/cache.rs"})),
        ],
    );
    transcript(
        &th,
        t0,
        &[
            (11, "Read", serde_json::json!({"file_path": "/x/a.rs"})),
            (12, "Read", serde_json::json!({"file_path": "/x/b.rs"})),
            (13, "Read", serde_json::json!({"file_path": "/x/c.rs"})),
            (20, "Edit", serde_json::json!({"file_path": "/x/c.rs"})),
        ],
    );

    let r: Value = serde_json::from_str(&e.ok(&["eval", "--all", "--json"])).unwrap();
    assert_eq!((r["runs"].as_u64(), r["transcripts"].as_u64()), (Some(3), Some(3)), "{r:#}");
    let fresh = &r["reach"]["cairn"][0];
    assert_eq!(
        (fresh["runs"].as_u64(), fresh["eligible"].as_u64(), fresh["given"].as_u64()),
        (Some(2), Some(1), Some(1))
    );
    assert_eq!(fresh["held_out"], 1);
    // A resume whose conversation was given the cairn has it.
    let resume = &r["reach"]["cairn"][1];
    assert_eq!((resume["given"].as_u64(), resume["in_context"].as_u64()), (Some(0), Some(1)), "{resume}");
    let cairn = &r["use"]["bullets"][1];
    assert_eq!(
        (cairn["shown"].as_u64(), cairn["anchored"].as_u64(), cairn["touched"].as_u64()),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(r["use"]["recalls"], serde_json::json!({"asked": 1, "with_hits": 1, "followed_by_show": 1}));
    assert_eq!(
        r["rediscovery"],
        serde_json::json!({"added": 4, "ignored": 1, "missed": 1, "unreachable": 0,
                           "at_update": {"jobs": 1, "repeated": 2, "like_shown": 1}}),
        "{r:#}"
    );
    let f = &r["fidelity"];
    assert_eq!((f["with_specifics"].as_u64(), f["carried"].as_f64()), (Some(1), Some(0.5)), "{f}");
    assert_eq!(f["drops"], serde_json::json!({"with_reason": 1, "cap": 1}));
    assert_eq!((f["updates"].as_u64(), f["churn"].as_f64()), (Some(1), Some(0.5)));
    assert_eq!(r["staleness"]["shown_stale_now"], 1);
    assert_eq!(r["cost"]["by_kind"][0]["input_tokens"], 3000);
    // A job whose model changed nothing cost as much, and is told apart.
    assert_eq!(
        (r["cost"]["by_kind"][0]["jobs"].as_u64(), r["cost"]["by_kind"][0]["unchanged"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(r["cost"]["per_bullet_used"], 3150);
    let (held, given) = (&r["holdout"]["held_out"], &r["holdout"]["given"]);
    assert_eq!(
        (held["calls_before_edit"].as_f64(), held["minutes_before_edit"].as_f64()),
        (Some(3.0), Some(10.0))
    );
    assert_eq!(
        (given["calls_before_edit"].as_f64(), given["minutes_before_edit"].as_f64()),
        (Some(3.0), Some(4.0))
    );
    assert_eq!((held["rediscovered"].as_u64(), given["rediscovered"].as_u64()), (Some(1), Some(1)));

    // As text, and for a window with nothing in it.
    let text = e.ok(&["eval", "--all"]);
    assert!(text.contains("reach:") && text.contains("held out"), "{text}");
    let none: Value =
        serde_json::from_str(&e.ok(&["eval", "--all", "--json", "--since", "2099-01-01"])).unwrap();
    assert_eq!(none["runs"], 0);
    // Nor further back than what runs were shown is kept.
    db.execute(
        "INSERT INTO run (id, task, agent, native_id, relation, launch, started) VALUES
           ('RO', '01AAAAAAAAAAAAAAAAAAAAAAAA', 'claude', 'o', 'fresh', 'managed', ?1)",
        [now - 31 * 86_400_000],
    )
    .unwrap();
    let back: Value =
        serde_json::from_str(&e.ok(&["eval", "--all", "--json", "--since", "2000-01-01"])).unwrap();
    assert_eq!(back["runs"], 3);
    // By default, the project you are in.
    let here = e.cmd(&["eval", "--json"]).current_dir(proj(&e)).output().unwrap();
    let here: Value = serde_json::from_slice(&here.stdout).unwrap();
    assert_eq!((here["scope"].as_str(), here["runs"].as_u64()), (Some(p.as_str()), Some(3)));
}

#[test]
fn a_noted_secret_is_stored_redacted_and_related_work_stays_in_its_project() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix the staging deploy", "--project", &p]);
    let (run, _) = start(&e, &t, &[]);
    let said = e.ok(&[
        "note",
        "--run",
        &run,
        "staging_deploy needs OAUTH_CLIENT_SECRET=Zm9vYmFy1234567890 exported first",
    ]);
    assert!(said.contains("[redacted]"), "{said}");
    assert_eq!(
        e.show(&t)["notes"],
        serde_json::json!(["staging_deploy needs OAUTH_CLIENT_SECRET=[redacted] exported first"])
    );

    // An ad-hoc session in another project is offered nothing of it.
    let other = e.path("elsewhere");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let session = "0b5d7c33-1c9e-4b7e-8f60-6d1a2b3c4d5e";
    let at_other = |file: &str| {
        let mut v: Value = serde_json::from_str(&payload(&e, file, session)).unwrap();
        v["cwd"] = other.to_string_lossy().into();
        v
    };
    hook(&e, "SessionStart", &at_other("session-start-startup.json").to_string(), None);
    let mut prompt = at_other("user-prompt-submit.json");
    prompt["prompt"] = "How do I run staging_deploy with OAUTH_CLIENT_SECRET?".into();
    let given = hook(&e, "UserPromptSubmit", &prompt.to_string(), None).unwrap_or_default();
    assert!(!given.contains("staging_deploy") && !given.contains("Zm9vYmFy"), "{given}");
    // Asked for, recall still looks everywhere.
    let found = e.ok(&["recall", "--all", "exported"]);
    assert!(found.contains("[redacted]"), "{found}");
}

#[test]
fn related_work_says_its_task_is_over_its_code_is_gone_and_never_repeats_a_bullet_shown() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    e.ok_in(
        &["edit", "--project", &p, "--stdin"],
        "Conventions:\nGotchas:\n- `cache_key_for` must include the region\nHow-to:\n",
    );
    let old = e.ok(&["new", "Fix the locale cache key bug", "--project", &p]);
    e.ok_in(
        &["edit", &old, "--stdin"],
        "Status: Fixed.\nFindings:\n- `cache_key_for` keys entries by locale only\n\
         - `cache_key_for` must include the region\nDecisions:\nOpen:\n",
    );
    e.sql("UPDATE task SET state = 'done'; UPDATE cairn_item SET stale = 1 WHERE text LIKE '%locale only';");
    let t = e.ok(&["new", "Add the region to cache_key_for", "--project", &p]);
    let (run, session) = start(&e, &t, &[]);
    let given =
        hook(&e, "SessionStart", &payload(&e, "session-start-startup.json", &session), Some(&run)).unwrap();
    let short = &old[old.len() - 6..].to_lowercase();
    assert!(
        given.contains(&format!(
            "- {short} (done) Fix the locale cache key bug: finding: `cache_key_for` keys entries by locale only (stale?)"
        )),
        "{given}"
    );
    assert_eq!(given.matches("must include the region").count(), 1, "{given}");
    // And so does recall, as text and as JSON.
    let found = e.ok(&["recall", "cache_key_for", "locale"]);
    assert!(found.contains("(done)") && found.contains("locale only (stale?)"), "{found}");
    let json: Value = serde_json::from_str(&e.ok(&["recall", "cache_key_for", "locale", "--json"])).unwrap();
    assert_eq!(
        (json["hits"][0]["state"].as_str(), json["hits"][0]["stale"].as_bool()),
        (Some("done"), Some(true))
    );
}

#[test]
fn recall_shows_the_bullet_that_matched_before_the_title() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix the locale cache key bug", "--project", &p]);
    e.ok_in(
        &["edit", &t, "--stdin"],
        "Status: s\nFindings:\n- It keys entries by locale only\nDecisions:\nOpen:\n- Check the locale fallback\n",
    );
    let found = e.ok(&["recall", "locale", "cache"]);
    assert!(found.contains("finding: It keys entries by locale only"), "{found}");
    // One hit a task: its other matching lines are counted.
    let found = e.ok(&["recall", "locale"]);
    assert!(found.contains("  (+1 more in this task)"), "{found}");
    let hits: Value = serde_json::from_str(&e.ok(&["recall", "locale", "--json"])).unwrap();
    assert_eq!(hits["hits"][0]["more"], 1);
}

#[test]
fn a_compacted_session_is_told_its_fold_is_still_running() {
    let e = env();
    let t = task_with_memory(&e);
    let (run, session) = start(&e, &t, &[]);
    hook(&e, "SessionStart", &payload(&e, "session-start-startup.json", &session), Some(&run));
    e.sql(&format!(
        "INSERT INTO job (id, run, upto, kind, state, created) VALUES ('J', '{run}', 10, 'fold', 'queued', 1);"
    ));
    let given = hook(&e, "SessionStart", &payload(&e, "session-start-compact.json", &session), None).unwrap();
    assert!(given.contains("from before the compaction is still running"), "{given}");
}

#[test]
fn an_edit_past_the_caps_is_refused_and_what_was_typed_is_kept() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let gotchas: String = (0..9).map(|n| format!("- `g{n}` bites\n")).collect();
    let text = format!("Conventions:\nGotchas:\n{gotchas}How-to:\n");
    let out = feed(&mut e.cmd(&["edit", "--project", &p, "--stdin"]), &text);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Gotchas has 9 lines, and holds at most 7"), "{err}");
    let kept = err.split("kept in ").nth(1).unwrap().trim();
    assert_eq!(std::fs::read_to_string(kept).unwrap(), text);
    let _ = std::fs::remove_file(kept);
    // So is one in the wrong shape.
    let out = feed(&mut e.cmd(&["edit", "--project", &p, "--stdin"]), "Gotchaz:\n- x\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("kept in "));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_file(err.split("kept in ").nth(1).unwrap().trim());
}

#[test]
fn an_edit_that_would_lose_a_line_is_refused() {
    let e = env();
    let t = task_with_memory(&e);
    let before = e.ok(&["edit", &t, "--print"]);
    for (text, says) in [
        ("Status: s\n- typed here\nFindings:\nDecisions:\nOpen:\n", "line 2 is a bullet under no heading"),
        ("Status: s\nFindings:\nDecisions:\nStatus: t\nOpen:\n", "line 4 is a second Status"),
    ] {
        let out = feed(&mut e.cmd(&["edit", &t, "--stdin"]), text);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(!out.status.success() && err.contains(says), "{err}");
        let kept = err.split("kept in ").nth(1).unwrap().trim();
        assert_eq!(std::fs::read_to_string(kept).unwrap(), text);
        let _ = std::fs::remove_file(kept);
    }
    assert_eq!(e.ok(&["edit", &t, "--print"]), before);
}

#[test]
fn the_editor_is_opened_with_the_format_said_and_it_is_not_saved() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix it", "--project", &p]);
    let editor = e.script(
        "editor",
        "#!/bin/sh\ngrep -q '^#: One line per bullet' \"$1\" || exit 1\nprintf -- '- `x` is new\\n' >> \"$1\"\n",
    );
    // An empty $VISUAL is unset, so $EDITOR opens it.
    let out = e.cmd(&["edit", &t]).env("VISUAL", "").env("EDITOR", &editor).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed = e.ok(&["edit", &t, "--print"]);
    assert!(
        printed.ends_with("Open:\n- [mine] `x` is new") && !printed.contains("One line per bullet"),
        "{printed}"
    );
}

#[test]
fn a_missing_editor_is_named_and_leaves_no_file() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix it", "--project", &p]);
    let tmp = e.path("tmp");
    std::fs::create_dir(&tmp).unwrap();
    let out = e
        .cmd(&["edit", &t])
        .env("VISUAL", "")
        .env("EDITOR", "no-such-editor --wait")
        .env("TMPDIR", &tmp)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("no-such-editor not found: set $VISUAL or $EDITOR"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0, "a temp file was left");
    // A real editor that fails keeps what it had.
    let out = e.cmd(&["edit", &t]).env("VISUAL", "false").env("TMPDIR", &tmp).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("false exited with") && err.contains("kept in"), "{err}");
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 1);
}

#[test]
fn a_task_s_context_is_edited_above_its_cairn() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix it", "--project", &p, "--body", "Why it matters."]);
    let cairn = "Status: \nFindings:\nDecisions:\nOpen:";
    let printed = e.ok(&["edit", &t, "--print"]) + "\n";
    assert_eq!(printed, format!("#: version 0\nContext:\nWhy it matters.\n\n{cairn}\n"));
    let edit = |input: &str| e.ok_in(&["edit", &t, "--stdin"], input);
    assert_eq!(edit(&printed), "unchanged");

    let longer = "Why it matters.\n\n- and where to start\nFindings: none yet";
    assert_eq!(edit(&printed.replace("Why it matters.", longer)), "saved");
    assert_eq!(e.show(&t)["body"], longer);
    // `context` is its name now; `body` stays for v2 clients.
    assert_eq!(e.show(&t)["context"], longer);
    // Only the context changed: the cairn was not written.
    assert!(e.show(&t)["cairn"].is_null());
    // An edit without a Context: line leaves the context alone.
    assert_eq!(edit(&format!("{cairn}\n- [mine] `x` is new\n")), "saved");
    assert_eq!(e.show(&t)["body"], longer);
    assert_eq!(edit(&format!("Context:\n\n{cairn}\n- [mine] `x` is new\n")), "saved");
    assert!(e.show(&t)["body"].is_null() && e.show(&t)["context"].is_null());
    assert!(e.ok(&["edit", &t, "--print"]).contains("\nContext:\n\nStatus: "));
}

#[test]
fn a_linked_task_s_context_is_shown_but_not_edited() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix it", "--project", &p, "--body", "Why it matters.\nAnd more."]);
    e.ok(&["link", &t, "org"]);
    let cairn = "Status: \nFindings:\nDecisions:\nOpen:";
    let printed = e.ok(&["edit", &t, "--print"]) + "\n";
    assert_eq!(
        printed,
        format!(
            "#: version 0\n#: Context, from org: edit it there.\n#: Why it matters.\n#: And more.\n{cairn}\n"
        )
    );
    assert_eq!(e.ok_in(&["edit", &t, "--stdin"], &printed), "unchanged");
    assert_eq!(e.ok_in(&["edit", &t, "--stdin"], &format!("{printed}- [mine] `x` is new\n")), "saved");

    let out = feed(&mut e.cmd(&["edit", &t, "--stdin"]), &format!("Context:\nOther.\n\n{cairn}\n"));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success() && err.contains("from org: edit it there"), "{err}");
    let _ = std::fs::remove_file(err.split("kept in ").nth(1).unwrap().trim());
    assert_eq!(e.show(&t)["body"], "Why it matters.\nAnd more.");
}

#[test]
fn a_note_says_when_no_update_will_fold_it_in() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix it", "--project", &p]);
    let (run, _) = start(&e, &t, &[]);
    assert!(e.ok(&["note", "--run", &run, "a"]).contains("at the next update"));
    e.sql("UPDATE task SET state = 'parked';");
    assert!(e.ok(&["note", "--run", &run, "b"]).contains("the task is parked"));
    std::fs::write(e.path("config.toml"), "[memory]\nenabled = false\n").unwrap();
    assert!(e.ok(&["note", "--run", &run, "c"]).contains("memory updates are off"));
}

#[test]
fn a_line_is_added_changed_removed_and_brought_back_one_at_a_time() {
    let e = env();
    let p = proj(&e).to_string_lossy().to_string();
    let t = e.ok(&["new", "Fix it", "--project", &p, "--body", "Why it matters."]);
    let json = |args: &[&str]| -> Value {
        let mut all = vec!["edit", t.as_str()];
        all.extend_from_slice(args);
        all.push("--json");
        serde_json::from_str(&e.ok(&all)).unwrap()
    };
    let refused = |args: &[&str]| -> String {
        let mut all = vec!["edit", t.as_str()];
        all.extend_from_slice(args);
        let out = e.run(&all);
        assert!(!out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    let added = json(&["--add", "findings", "The", "cache key omits", "the locale in `cache_key_for`."]);
    let id = added["id"].as_i64().unwrap();
    assert_eq!(
        (added["v"].as_i64(), added["version"].as_i64(), added["changed"].as_bool()),
        (Some(1), Some(1), Some(true))
    );
    let line = &added["cairn"]["findings"][0];
    assert_eq!(line["text"], "The cache key omits the locale in `cache_key_for`.");
    assert_eq!(
        (line["id"].as_i64(), line["mine"].as_bool(), line["anchor"].as_str()),
        (Some(id), Some(true), Some("cache_key_for"))
    );
    let set = e.ok(&["edit", &t, "--add", "status", "Cause found."]);
    assert_eq!(set, "Status set, yours now: updates won't change it; --rm status hands it back");
    assert_eq!(e.show(&t)["cairn"]["status_mine"], true);
    // Handed back, it keeps its words for the next update to change.
    let back = e.ok(&["edit", &t, "--rm", "status"]);
    assert_eq!(back, "Status handed back: the next update may change it");
    assert_eq!(e.show(&t)["cairn"]["status"], "Cause found.");
    assert_eq!(e.show(&t)["cairn"]["status_mine"], Value::Null);
    assert!(refused(&["--rm", "status"]).contains("the Status is already the updates'"));
    e.ok(&["edit", &t, "--add", "status", "Cause found."]);
    // A client is told the caps the core holds it to.
    assert_eq!(
        e.show(&t)["limits"],
        serde_json::json!({"status": 30, "line": 40, "words": 200,
                           "sections": {"findings": 7, "decisions": 5, "open": 5}})
    );
    let project: Value = serde_json::from_str(&e.ok(&["show", "--project", &p, "--json"])).unwrap();
    assert_eq!(
        project["limits"],
        serde_json::json!({"line": 40, "words": 200, "sections": {"conventions": 5, "gotchas": 7, "howto": 5}})
    );
    let open = e.ok_in(&["edit", &t, "--add", "open", "-"], "-x is read from stdin\n");
    assert!(open.starts_with("added line #"), "{open}");

    // What is refused says why, and nothing is written.
    assert!(refused(&["--add", "gotchas", "x"])
        .contains("no section \"gotchas\": a task's cairn has status, findings"));
    assert!(refused(&["--set", "999", "x"]).contains("not saved: no line #999 in this cairn"));
    assert!(refused(&["--set", "one", "x"]).contains("--set takes a line's id, the #12 varde show prints"));
    assert!(refused(&["--add", "open", &"word ".repeat(41)]).contains("at most 40 words"));
    for n in 1..5 {
        e.ok(&["edit", &t, "--add", "open", &format!("open item {n}")]);
    }
    assert!(refused(&["--add", "open", "one more"]).contains("not saved: Open is full (5 of 5)"));
    assert!(refused(&["--json"]).contains("--add"));
    assert!(refused(&["--add", "open", "x", "--print"]).contains("cannot be used with"));
    assert_eq!(e.show(&t)["usage"]["author"], "user");

    // Changed in place, it keeps its id; the same words change nothing.
    let set = json(&["--set", &id.to_string(), "The cache key omits the locale."]);
    assert_eq!(
        (set["id"].as_i64(), set["cairn"]["findings"][0]["text"].as_str()),
        (Some(id), Some("The cache key omits the locale."))
    );
    assert_eq!(e.ok(&["edit", &t, "--set", &id.to_string(), "The cache key omits the locale."]), "unchanged");

    // Removed, it leaves for history, which recall does not find, and
    // comes back.
    let rm = e.ok(&["edit", &t, "--rm", &format!("#{id}")]);
    assert_eq!(
        rm,
        format!("removed line #{id}; recall no longer finds it, and --restore {id} brings it back")
    );
    assert!(e.show(&t)["cairn"]["findings"].as_array().unwrap().is_empty());
    assert!(refused(&["--rm", &id.to_string()])
        .contains(&format!("not saved: line #{id} is already removed; --restore {id} brings it back")));
    let found = e.ok(&["recall", "locale"]);
    assert!(!found.contains("The cache key omits the locale."), "{found}");
    let back = json(&["--restore", &id.to_string()]);
    assert_eq!(back["cairn"]["findings"][0]["id"].as_i64(), Some(id));
    let found = e.ok(&["recall", "locale"]);
    assert!(found.contains("finding: The cache key omits the locale."), "{found}");
    assert!(refused(&["--restore", &id.to_string()]).contains("no line"));
    let shown: Value = serde_json::from_str(&e.ok(&["show", &t, "--json", "--no-git", "--history"])).unwrap();
    let reasons: Vec<&str> =
        shown["history"].as_array().unwrap().iter().filter_map(|v| v["reason"].as_str()).collect();
    assert!(
        reasons.contains(&format!("remove {id}").as_str())
            && reasons.last() == Some(&format!("restore {id}").as_str()),
        "{reasons:?}"
    );

    // `varde show` has the cairn first, then the context, as the board does.
    let shown = e.ok(&["show", &t, "--no-git"]);
    let at = |s: &str| shown.find(s).unwrap_or_else(|| panic!("{s} in {shown}"));
    assert!(
        at("Cairn · what its sessions learned, handed to the next one")
            < at("  Status  Cause found. (yours)")
    );
    assert!(
        at(&format!(
            "  Findings 1/7\n    - The cache key omits the locale. (yours)  #{id}\n  Decisions 0/5\n  Open 5/5"
        ))
            < at("\nContext\n  Why it matters.")
    );

    // A project's cairn takes its own sections.
    let out = e.ok(&[
        "edit",
        "--project",
        &p,
        "--add",
        "howto",
        "Reinstall with `cargo install --path cli/crates/varde`.",
    ]);
    assert!(out.starts_with("added line #"), "{out}");
    let out = e.run(&["edit", "--project", &p, "--add", "status", "x"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("a project cairn has no Status"));
}
