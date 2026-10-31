//! `varde steward`, `trust`, `asks`, `answer` and `watch` (§6.6), and the
//! wait a delegated session's hook makes for the steward's answer.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use varde_core::plan::Want;
use varde_core::steward::{self, Kind};
use varde_core::store::Store;
use varde_core::task;

use crate::{now_ms, process, start, Paths};

const BUSY: Duration = Duration::from_secs(2);

/// How long a hook waits on the steward at most, whatever the hand-over
/// says: under the hook's timeout in the plugin, so the hook answers
/// before the agent gives up on it.
pub const WAIT_MAX: Duration = Duration::from_secs(25 * 60);

/// How often a waiting hook, and `watch`, look at the store.
const POLL: Duration = Duration::from_millis(250);

/// What the steward's task is called, and what it is first told.
const TITLE: &str = "Steward";
const CONTEXT: &str = "You are the user's varde steward. Start by reading your manual: varde steward guide";

/// The steward's directory: its own project, outside any repository.
fn dir(paths: &Paths) -> PathBuf {
    paths.home.join("steward")
}

/// The file `watch` keeps its pid in: a hook waits only while someone
/// watches.
fn watcher(paths: &Paths) -> PathBuf {
    paths.home.join("steward.watch")
}

pub fn on(paths: &Paths) -> Result<()> {
    let (id, made) = enable(paths)?;
    let was = if made { "" } else { "already " };
    println!("the steward is {was}on: task {}; varde steward goes to it", task::short(&id));
    Ok(())
}

/// Turn the steward on, giving it a task of its own the first time: its
/// task, and whether it was turned on now.
pub fn enable(paths: &Paths) -> Result<(String, bool)> {
    let store = paths.open(BUSY)?;
    let dir = dir(paths);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // One at a time, though two may turn it on at once.
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate)?;
    if let Some(id) = steward::task(&tx)? {
        return Ok((id, false));
    }
    // Turned on again, it is the steward it was, with its memory of you.
    let earlier: Option<String> = tx
        .query_row(
            "SELECT id FROM task WHERE project = ?1 AND title = ?2 AND state != 'dropped'
             ORDER BY created DESC LIMIT 1",
            (dir.display().to_string(), TITLE),
            |r| r.get(0),
        )
        .optional()?;
    let id = match earlier {
        Some(id) => id,
        None => task::create(&store, TITLE, Some(CONTEXT), Some(&dir.display().to_string()), None, now_ms())?,
    };
    steward::enable(&store, &id, now_ms())?;
    tx.commit()?;
    Ok((id, true))
}

pub fn off(paths: &Paths) -> Result<()> {
    let store = paths.open(BUSY)?;
    match steward::disable(&store, now_ms())? {
        Some(id) => println!("the steward is off; its task {} stays, as an ordinary one", task::short(&id)),
        None => println!("the steward is already off"),
    }
    Ok(())
}

/// `varde steward`: go to its session, starting or resuming it.
pub fn go(paths: &Paths) -> Result<i32> {
    let id = {
        let store = paths.open(BUSY)?;
        steward::task(&store.conn)?.context("the steward is off: varde steward on")?
    };
    let opts = start::Options {
        want: Want::Continue,
        worktree: Some(false),
        context: None,
        agent: None,
        project: None,
        take_over: false,
        run: None,
    };
    start::attach(paths, &id, opts)
}

pub fn guide(topic: Option<&str>) -> Result<()> {
    match topic {
        None => println!("{}", crate::steward_guide::INDEX),
        Some(t) => match crate::steward_guide::TOPICS.iter().find(|(name, _)| *name == t) {
            Some((_, text)) => println!("{text}"),
            None => {
                let names: Vec<&str> = crate::steward_guide::TOPICS.iter().map(|(n, _)| *n).collect();
                bail!("no topic '{t}': {}", names.join(", "))
            }
        },
    }
    Ok(())
}

/// `30m`, `2h`, `1d`, as milliseconds.
fn span(text: &str) -> Result<i64> {
    let bad = || anyhow::anyhow!("'{text}' is not a span like 30m, 2h or 1d");
    let unit = match text.chars().last().ok_or_else(bad)? {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return Err(bad()),
    };
    let n: i64 = text[..text.len() - 1].parse().ok().filter(|n| *n > 0).ok_or_else(bad)?;
    Ok(n * unit)
}

pub fn trust(
    paths: &Paths,
    reference: &str,
    span_text: Option<&str>,
    brief: Option<String>,
    rules: &[String],
    off: bool,
) -> Result<()> {
    let store = paths.open(BUSY)?;
    yours(&store, "Handing a task to the steward")?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    let now = now_ms();
    if off {
        if steward::untrust(&store, &id, now)? {
            println!("{short} is yours again");
        } else {
            println!("{short} was not handed to the steward");
        }
        return Ok(());
    }
    // Rules alone add to the hand-over there is.
    if span_text.is_none() && brief.is_none() && !rules.is_empty() && steward::allow(&store, &id, rules, now)?
    {
        println!("the hook now allows {} on {short} by itself", rules.join(", "));
        return Ok(());
    }
    let until = now + span(span_text.unwrap_or("2h"))?;
    let brief = crate::arg_or_stdin(brief)?.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    steward::trust(&store, &id, brief.as_deref(), rules, until, now)?;
    println!("{short} is the steward's until {}; varde trust {short} --off takes it back", clock(until));
    Ok(())
}

fn clock(ms: i64) -> String {
    let t = varde_core::schedule::from_ms(ms);
    t.format("%H:%M").to_string()
}

pub fn asks(paths: &Paths, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    steward::expire(&store.conn, now_ms(), WAIT_MAX.as_millis() as i64)?;
    let asks = steward::open_asks(&store.conn, i64::MIN)?;
    if as_json {
        println!("{}", json!({"v": 1, "asks": asks.iter().map(|a| ask_json(&store, a)).collect::<Vec<_>>()}));
        return Ok(());
    }
    if asks.is_empty() {
        println!("nothing waits on the steward");
    }
    for a in &asks {
        println!("{}  {}  {}  {}", task::short(&a.id), task::short(&a.task), a.kind, summary(a));
    }
    Ok(())
}

/// One line saying what an ask is about.
fn summary(a: &steward::Ask) -> String {
    let s = |k: &str| a.input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match (a.kind.as_str(), a.tool.as_str()) {
        ("question", _) => {
            questions(&a.input).iter().map(|(q, _)| q.as_str()).collect::<Vec<_>>().join(" / ")
        }
        (_, "Bash") => format!("Bash: {}", s("command")),
        (_, "Edit" | "Write" | "MultiEdit") => format!("{}: {}", a.tool, s("file_path")),
        (_, tool) => format!("{tool}: {}", a.input),
    }
}

/// An `AskUserQuestion`'s questions, each with its options' labels.
fn questions(input: &Value) -> Vec<(String, Vec<String>)> {
    let Some(qs) = input.get("questions").and_then(Value::as_array) else { return vec![] };
    qs.iter()
        .map(|q| {
            let text = q.get("question").and_then(Value::as_str).unwrap_or("").to_string();
            let labels = q.get("options").and_then(Value::as_array).map_or(vec![], |os| {
                os.iter().filter_map(|o| o.get("label").and_then(Value::as_str).map(str::to_string)).collect()
            });
            (text, labels)
        })
        .collect()
}

/// An ask with what the steward needs to decide it: the task, and the
/// hand-over's brief.
fn ask_json(store: &Store, a: &steward::Ask) -> Value {
    let title: Option<String> =
        store.conn.query_row("SELECT title FROM task WHERE id = ?1", [&a.task], |r| r.get(0)).ok();
    let brief = steward::trusted(&store.conn, &a.task, now_ms()).ok().flatten().and_then(|t| t.brief);
    json!({
        "ask": task::short(&a.id),
        "task": task::short(&a.task),
        "title": title,
        "brief": brief,
        "kind": a.kind,
        "tool": a.tool,
        "input": a.input,
        "summary": summary(a),
        "created": a.created,
    })
}

/// `varde answer <ask> ANSWER… [--why]`: `allow` or `deny` a permission;
/// for a question, one answer per question, in order, an option's label
/// or words of your own. `--pass` hands it back to the user at once.
pub fn answer(paths: &Paths, reference: &str, words: &[String], why: Option<&str>, pass: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    yours(&store, "Answering in the user's name")?;
    let a = steward::resolve_ask(&store.conn, reference)?;
    let short = task::short(&a.id);
    if pass {
        steward::settle(&store.conn, &a.id, None, why, "passed", now_ms())?;
        println!("{short} goes to the user");
        return Ok(());
    }
    let answer = match a.kind.as_str() {
        "permission" => match words {
            [w] if w == "allow" => json!({"behavior": "allow"}),
            [w] if w == "deny" => {
                let why = why.context("a deny needs --why: the session is told it")?;
                json!({"behavior": "deny", "message": why})
            }
            _ => bail!("a permission is answered allow or deny"),
        },
        _ => {
            let qs = questions(&a.input);
            if words.len() != qs.len() {
                bail!("{} questions, {} answers: one answer each, in order", qs.len(), words.len());
            }
            let answers: serde_json::Map<String, Value> =
                qs.into_iter().zip(words).map(|((q, _), w)| (q, Value::String(w.clone()))).collect();
            json!({"answers": answers})
        }
    };
    if !steward::settle(&store.conn, &a.id, Some(&answer.to_string()), why, "steward", now_ms())? {
        bail!("{short} was settled meanwhile");
    }
    println!("{short} answered");
    Ok(())
}

/// `varde watch`: one JSON line per ask as it comes, and per handed-over
/// task (`all`: per task) that comes to need you, until stopped. While it runs, delegated
/// sessions wait on the steward; without it they ask you as before.
pub fn watch(paths: &Paths, all: bool) -> Result<()> {
    let file = watcher(paths);
    std::fs::write(
        &file,
        format!("{} {}", std::process::id(), process::start_time(std::process::id()).unwrap_or(0)),
    )
    .with_context(|| format!("writing {}", file.display()))?;
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut needing: std::collections::HashSet<String> = Default::default();
    let mut out = std::io::stdout();
    loop {
        let store = paths.open(BUSY)?;
        let now = now_ms();
        // Asks whose hook has gone are settled, so none waits for ever.
        steward::expire(&store.conn, now, WAIT_MAX.as_millis() as i64)?;
        // By what was shown, not by time: hooks commit out of order.
        let open = steward::open_asks(&store.conn, i64::MIN)?;
        seen.retain(|id| open.iter().any(|a| &a.id == id));
        for a in open {
            if !seen.insert(a.id.clone()) {
                continue;
            }
            let mut line = ask_json(&store, &a);
            line["event"] = json!("ask");
            writeln!(out, "{line}")?;
        }
        // A handed-over task whose session stopped for you: a question in
        // plain text, or a turn that ended. Said once each time.
        let trusted: Vec<String> = steward::trusts(&store.conn, now)?.into_iter().map(|t| t.task).collect();
        let board = varde_core::board::build(&store, now, 0, false, &process::alive)?;
        for row in board.groups.iter().flat_map(|g| &g.rows).filter(|r| all || trusted.contains(&r.task)) {
            let needs = board.groups.first().is_some_and(|g| g.rows.iter().any(|x| x.task == row.task));
            if needs && needing.insert(row.task.clone()) {
                writeln!(
                    out,
                    "{}",
                    json!({"event": "needs-you", "task": row.short, "title": row.title, "status": row.status,
                           "held": trusted.contains(&row.task)})
                )?;
            } else if !needs {
                needing.remove(&row.task);
            }
        }
        out.flush()?;
        drop(store);
        std::thread::sleep(POLL * 4);
    }
}

/// `varde say <task> TEXT`: tell a task something. With a session open,
/// working or waiting, the message is kept for it and goes in by the
/// agent's own way when it has one (`Agent::push`), else with the
/// session's next hook that carries context. With none, the task carries
/// on in the background with TEXT as its next turn: its latest
/// conversation resumed, or its first started. A closed task with no
/// session gets a follow-up instead. `wait` waits for the turn that
/// answers it, and prints what that turn said.
pub fn say(
    paths: &Paths,
    reference: &str,
    text: Option<String>,
    worktree: Option<bool>,
    how: Say,
) -> Result<()> {
    let text = crate::arg_or_stdin(text)?.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let text = text.context("nothing to say: give the text, or - to read it from stdin")?;
    let mut store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    let now = now_ms();
    let sender = sender(&store)?;
    // Sent from a session, the answer comes back to it as a message;
    // not to one waiting for it here, nor to the task's own session.
    // Nor does an answer to a message from `id` ask for one in turn.
    let reply_to = match sending_run(&store)? {
        Some(run) if !how.wait => {
            let own: Option<String> =
                store.conn.query_row("SELECT task FROM run WHERE id = ?1", [&run], |r| r.get(0))?;
            let answering: bool = store.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM message m JOIN run s ON s.id = m.reply_to
                   WHERE m.run = ?1 AND s.task = ?2 AND m.replied IS NULL)",
                (&run, &id),
                |r| r.get(0),
            )?;
            (own.as_deref() != Some(id.as_str()) && !answering).then_some(run)
        }
        _ => None,
    };
    let back = if reply_to.is_some() { "; its answer comes back here as a message" } else { "" };
    if let Some((message, pushed)) = deliver(&store, &id, &text, &sender, reply_to.as_deref(), how.now, now)?
    {
        if pushed {
            println!("{short}: in its open session now{back}");
        } else {
            println!("{short}: its open session gets it with its next tool call or prompt{back}");
        }
        drop(store);
        return if how.wait { answer_to(paths, &message) } else { Ok(()) };
    }
    // A task's session never starts another task's: what it says waits
    // for that task's next session. You and the steward start them.
    if let Caller::Task(..) = caller(&store)? {
        varde_core::message::send(&store.conn, &id, &text, Some(&sender), reply_to.as_deref(), now)?;
        println!("{short} has no session open: it gets this when it next runs");
        return Ok(());
    }
    let state: String = store.conn.query_row("SELECT state FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
    if matches!(state.as_str(), "done" | "dropped") && !how.reopen {
        bail!(
            "{short} is {state}: --reopen carries it on, open again; a follow-up is a task of its own, \
             varde new --from <its run>"
        );
    }
    let ran: bool =
        store.conn.query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [&id], |r| r.get(0))?;
    let opts = start::Options {
        want: Want::Continue,
        worktree,
        // A first start is told it with the task's own context.
        context: (!ran).then(|| if sender == "the user" { text.clone() } else { framed(&sender, &text) }),
        agent: None,
        project: None,
        take_over: false,
        run: None,
    };
    let (run, cwd) = match start::prepare(&mut store, &id, opts, false)? {
        start::Outcome::Ready { run, cwd, .. } => (run, cwd),
        start::Outcome::Focus { .. } => bail!("{short} has a session starting: say it again in a moment"),
        start::Outcome::Ask(ask) => bail!("{}", ask.message()),
    };
    // What the user typed goes as they typed it; the steward's says so.
    let prompt = if sender == "the user" { text.clone() } else { framed(&sender, &text) };
    if ran {
        store.conn.execute("UPDATE run SET background = 1, prompt = ?2 WHERE id = ?1", (&run, &prompt))?;
    } else {
        store.conn.execute("UPDATE run SET background = 1 WHERE id = ?1", [&run])?;
    }
    // Kept as a message too, so `wait` reads the reply as for any other.
    let message =
        varde_core::message::send(&store.conn, &id, &text, Some(&sender), reply_to.as_deref(), now)?;
    let transcript: Option<String> = store.conn.query_row(
        "SELECT p.transcript FROM run r LEFT JOIN run p ON p.id = r.parent_run WHERE r.id = ?1",
        [&run],
        |r| r.get(0),
    )?;
    varde_core::message::delivered(&store.conn, &message, &run, "resume", size(transcript.as_deref()), now)?;
    if let Err(e) = crate::schedule::launch(&run, &cwd) {
        store.record_exit(&run, 127, now_ms())?;
        return Err(e);
    }
    let verb = if ran { "carries on" } else { "starts" };
    println!("{short} {verb} in the background; varde attach {short} takes it over{back}");
    drop(store);
    if how.wait {
        answer_to(paths, &message)?;
    }
    Ok(())
}

/// Give `text` to `task`'s open session, if it has one: kept as a
/// message, and pushed in where the agent allows, else left for the
/// session's next hook. The message and whether it went in now; `None`
/// with no session open, and nothing kept.
fn deliver(
    store: &Store,
    task: &str,
    text: &str,
    sender: &str,
    reply_to: Option<&str>,
    urgent: bool,
    now: i64,
) -> Result<Option<(String, bool)>> {
    let Some(live) = start::run_facts(store, task)?.into_iter().find(|r| r.live()) else { return Ok(None) };
    let message = varde_core::message::send(&store.conn, task, text, Some(sender), reply_to, now)?;
    let (agent, native, pid, transcript): (String, String, Option<i64>, Option<String>) = store
        .conn
        .query_row("SELECT agent, native_id, pid, transcript FROM run WHERE id = ?1", [&live.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    // Claimed before it is pushed, so a hook firing meanwhile does not take
    // it too; given back for the hook when the push does not go in.
    let mark = size(transcript.as_deref());
    varde_core::message::delivered(&store.conn, &message, &live.id, "push", mark, now)?;
    let pushed = match (varde_agent::by_name(&agent), pid.filter(|p| *p > 0)) {
        (Some(a), Some(pid)) => {
            let note = if reply_to.is_some() { ANSWER_HERE } else { "" };
            a.push(pid, &native, &format!("{}{note}", framed(sender, text)), urgent).unwrap_or(false)
        }
        _ => false,
    };
    if !pushed {
        varde_core::message::undeliver(&store.conn, &message)?;
    }
    Ok(Some((message, pushed)))
}

/// The run of the session this command runs in, when varde knows it.
fn sending_run(store: &Store) -> Result<Option<String>> {
    Ok(session(store)?.map(|s| s.0))
}

/// The session this command runs in: the nearest ancestor process that
/// is a live run's agent, as varde recorded it, with its task and title.
/// Read from the process tree, which a session cannot fake as it can its
/// environment (`VARDE_RUN`).
fn session(store: &Store) -> Result<Option<(String, String, String)>> {
    let mut stmt = store.conn.prepare_cached(
        "SELECT r.id, coalesce(r.pid_start, 0), t.id, t.title FROM run r JOIN task t ON t.id = r.task
         WHERE r.pid = ?1 AND r.exit_code IS NULL ORDER BY r.started DESC LIMIT 1",
    )?;
    for (pid, _) in process::ancestors() {
        let row: Option<(String, i64, String, String)> =
            stmt.query_row([pid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
        if let Some((run, start, task, title)) = row {
            if process::alive(i64::from(pid), start) {
                return Ok(Some((run, task, title)));
            }
        }
    }
    Ok(None)
}

/// Whether this runs in an agent session varde has no run of yet: told
/// by the marker Claude puts in its tool shells. Such a session holds no
/// hand-over, so a marker it could unset gives it nothing.
fn untracked_agent() -> bool {
    std::env::var_os("CLAUDECODE").is_some()
}

/// `varde reply <run>`, started by the hook as `run`'s turn ends: what
/// that turn reported goes back to each session that asked it something,
/// as a message of its own. One that is not open gets it when it next
/// runs: a reply never starts a session, and asks for no reply.
pub fn reply(paths: &Paths, run: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    let now = now_ms();
    let owed = varde_core::message::owed_by(&store.conn, run, now)?;
    if owed.is_empty() {
        return Ok(());
    }
    let (agent, transcript, task, title, ended): (String, Option<String>, String, String, bool) =
        store.conn.query_row(
            "SELECT r.agent, r.transcript, t.id, t.title, r.exit_code IS NOT NULL
             FROM run r JOIN task t ON t.id = r.task WHERE r.id = ?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
    // The turn's last lines may still be on their way to the transcript
    // as its end is heard: read it once it has stopped growing.
    if let Some(t) = &transcript {
        settle(std::path::Path::new(t));
    }
    // A pushed message counts as answered by this turn only once it is in
    // the transcript: one queued behind the turn that just ended waits for
    // the turn that takes it. One a hook handed over went into this turn.
    // A session gone answers what it can.
    let alive = !ended && start::run_facts(&store, &task)?.iter().any(|r| r.id == run && r.live());
    let taken = |m: &varde_core::message::Owed| {
        !alive
            || m.via.as_deref() != Some("push")
            || in_transcript(transcript.as_deref(), m.mark.unwrap_or(0), &m.text)
    };
    let answering: Vec<&varde_core::message::Owed> = owed.iter().filter(|m| taken(m)).collect();
    if answering.is_empty() {
        return Ok(());
    }
    // From where each went in: a push from where it shows in the
    // transcript, as the turn it was queued behind may have run on.
    let start = |m: &varde_core::message::Owed| {
        let mark = m.mark.unwrap_or(0);
        if m.via.as_deref() == Some("push") {
            found_at(transcript.as_deref(), mark, &m.text).unwrap_or(mark)
        } else {
            mark
        }
    };
    let from = answering.iter().map(|m| start(m)).min().unwrap_or(0);
    let said = match (varde_agent::by_name(&agent), &transcript) {
        (Some(a), Some(t)) => a.digest(std::path::Path::new(t), from, 4000).map(|d| d.0).unwrap_or_default(),
        _ => String::new(),
    };
    let said = said.trim();
    let text = if said.is_empty() {
        "Its turn ended with nothing to read back: varde said shows where it stands.".to_string()
    } else {
        format!("Its answer, as its turn ended:\n\n{said}")
    };
    let sender = format!("task {} ({title}), answering your varde say", task::short(&task));
    let mut senders: Vec<String> = Vec::new();
    for m in answering {
        if varde_core::message::answered(&store.conn, &m.id, now)? && !senders.contains(&m.reply_to) {
            senders.push(m.reply_to.clone());
        }
    }
    for to in senders {
        let Some(to_task) = store
            .conn
            .query_row("SELECT task FROM run WHERE id = ?1", [&to], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
        else {
            continue;
        };
        // Held for its next session when none is open.
        if deliver(&store, &to_task, &text, &sender, None, false, now)?.is_none() {
            varde_core::message::send(&store.conn, &to_task, &text, Some(&sender), None, now)?;
        }
    }
    Ok(())
}

/// Whether `text`'s first line is in `transcript` past `from`, as any
/// agent writes it: JSON-escaped, within a line of its own.
fn in_transcript(transcript: Option<&str>, from: u64, text: &str) -> bool {
    found_at(transcript, from, text).is_some()
}

/// Where the line holding `text`'s first line starts in `transcript`,
/// past `from`.
fn found_at(transcript: Option<&str>, from: u64, text: &str) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let path = transcript?;
    let first: String =
        text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(60).collect();
    if first.is_empty() {
        return Some(from);
    }
    let needle = serde_json::to_string(&first).unwrap_or_default();
    let needle = needle.trim_matches('"');
    let mut f = std::fs::File::open(path).ok()?;
    let mut tail = String::new();
    f.seek(SeekFrom::Start(from)).ok()?;
    f.read_to_string(&mut tail).ok()?;
    let at = tail.find(needle)?;
    // The start of its line: the digest reads whole lines.
    Some(from + tail[..at].rfind('\n').map_or(0, |n| n as u64 + 1))
}

/// Wait, up to a few seconds, until `path` has not grown for a moment.
fn settle(path: &std::path::Path) {
    let len = || std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = len();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        let now = len();
        if now == last {
            return;
        }
        last = now;
    }
}

/// How `say` says it.
#[derive(Clone, Copy, Default)]
pub struct Say {
    /// Wait for the turn that answers it, and print what it said.
    pub wait: bool,
    /// Cut into a turn under way, where the agent allows, rather than
    /// after it.
    pub now: bool,
    /// Carry a closed task on, open again, as starting it would.
    pub reopen: bool,
}

/// `varde said <task>`: what its latest session said last, read from its
/// transcript as the cairn's updates read it, whatever the agent.
pub fn said(paths: &Paths, reference: &str, budget: usize) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let row: Option<(String, Option<String>, String)> = store
        .conn
        .query_row(
            "SELECT agent, transcript, id FROM run WHERE task = ?1 AND transcript IS NOT NULL
             ORDER BY started DESC, rowid DESC LIMIT 1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((agent, Some(transcript), run)) = row else {
        println!("{} has said nothing yet", task::short(&id));
        return Ok(());
    };
    let status = varde_core::store::run_status(&store.conn, &run)?.map_or("ended", |s| s.name());
    let adapter = varde_agent::by_name(&agent).with_context(|| format!("unknown agent {agent}"))?;
    let (digest, _) = adapter.digest(std::path::Path::new(&transcript), 0, budget)?;
    println!("{} ({status}):\n{}", task::short(&id), digest.trim());
    Ok(())
}

/// Who a message is from, as the session is told: the steward when its
/// own session sends it, else the user.
fn sender(store: &Store) -> Result<String> {
    Ok(match caller(store)? {
        Caller::User => "the user".into(),
        Caller::Steward => "the user's varde steward".into(),
        Caller::Task(short, title) => format!("task {short} ({title}), another session varde runs"),
    })
}

/// Who runs this command.
enum Caller {
    User,
    Steward,
    /// Another task's session: its short id and title.
    Task(String, String),
}

fn caller(store: &Store) -> Result<Caller> {
    let steward = steward::task(&store.conn)?;
    Ok(match session(store)? {
        Some((_, task, _)) if Some(&task) == steward.as_ref() => Caller::Steward,
        Some((_, task, title)) => Caller::Task(task::short(&task), title),
        None if untracked_agent() => Caller::Task("?".into(), "an agent session".into()),
        None => Caller::User,
    })
}

/// Refuse `what` from another task's session: handing tasks over, rules
/// and answers in the user's name are the user's, and the steward's.
fn yours(store: &Store, what: &str) -> Result<()> {
    if let Caller::Task(short, _) = caller(store)? {
        bail!("{what} is the user's to do, or the steward's, not a task's session ({short})");
    }
    Ok(())
}

/// Whether this command runs in the steward's own session.
fn steward_run(store: &Store) -> Result<bool> {
    Ok(matches!(caller(store)?, Caller::Steward))
}

/// Whether this command runs in the steward's own session, which acts
/// for the user: what an agent may not do to a session of yours, it may.
pub fn from_steward() -> bool {
    Paths::from_env().open(BUSY).is_ok_and(|store| steward_run(&store).unwrap_or(false))
}

/// A message as its session reads it: who sent it, and how.
pub fn framed(sender: &str, text: &str) -> String {
    // Quoted line by line, so nothing in it reads as a header of its own:
    // a message cannot pass itself off as one from someone else.
    let quoted: Vec<String> = text.lines().map(|l| format!("│ {l}")).collect();
    format!("Message for this session from {sender}, sent through varde:\n\n{}", quoted.join("\n"))
}

/// `framed`, for a message kept in the store.
pub fn framed_for(m: &varde_core::message::Message) -> String {
    let note = if m.reply_to.is_some() { ANSWER_HERE } else { "" };
    format!("{}{note}", framed(m.sender.as_deref().unwrap_or("the user"), &m.text))
}

/// What a session is told with a message that wants an answer: varde
/// sends the turn back by itself, so the session answers in it.
const ANSWER_HERE: &str = "\n\n(varde sends what you report at the end of this turn back to the sender \
     by itself: answer here, and do not varde say it back.)";

fn size(path: Option<&str>) -> Option<u64> {
    std::fs::metadata(path?).ok().map(|m| m.len())
}

/// How long `say --wait` waits for its answer.
const ANSWER_MAX: Duration = Duration::from_secs(30 * 60);

/// Wait for message `id` to be in, and the turn that took it to end, then
/// print what was said from it on.
fn answer_to(paths: &Paths, id: &str) -> Result<()> {
    let deadline = Instant::now() + ANSWER_MAX;
    loop {
        let store = paths.open(BUSY)?;
        if let Some((run, mark, at)) = varde_core::message::landed(&store.conn, id)? {
            let (agent, transcript, exited): (String, Option<String>, bool) = store.conn.query_row(
                "SELECT agent, transcript, exit_code IS NOT NULL FROM run WHERE id = ?1",
                [&run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let ended: bool = store.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM event WHERE run = ?1 AND ts > ?2
                   AND kind IN ('turn_end', 'ended', 'interrupted'))",
                (&run, at),
                |r| r.get(0),
            )?;
            // The turn that took it, not one a push was queued behind.
            let (text, via): (String, Option<String>) =
                store.conn.query_row("SELECT text, via FROM message WHERE id = ?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let taken = via.as_deref() != Some("push")
                || in_transcript(transcript.as_deref(), mark.unwrap_or(0), &text);
            if (ended && taken) || exited {
                let from = match via.as_deref() {
                    Some("push") => found_at(transcript.as_deref(), mark.unwrap_or(0), &text).or(mark),
                    _ => mark,
                };
                let digest = match (varde_agent::by_name(&agent), transcript) {
                    (Some(a), Some(t)) => a.digest(std::path::Path::new(&t), from.unwrap_or(0), 4000)?.0,
                    _ => String::new(),
                };
                if digest.trim().is_empty() {
                    println!("(its turn ended with nothing to read back)");
                } else {
                    println!("{}", digest.trim());
                }
                return Ok(());
            }
        }
        drop(store);
        if Instant::now() >= deadline {
            bail!("no answer within 30 minutes; varde show tells where it stands");
        }
        std::thread::sleep(POLL * 4);
    }
}

/// Whether a `watch` runs: its pid, as it wrote it, is alive.
fn watched(paths: &Paths) -> bool {
    let Ok(text) = std::fs::read_to_string(watcher(paths)) else { return false };
    let mut it = text.split_whitespace().map(|w| w.parse::<i64>().unwrap_or(0));
    let (pid, start) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    process::alive(pid, start)
}

/// In a delegated session's hook: hand what it asks to the steward and
/// wait for the answer. `None`, at once or after waiting, leaves the
/// agent to ask the user as it would have. Kept out of the hook's
/// budget on every other task by one indexed read.
pub fn wait(paths: &Paths, store: &Store, run: &str, asked: &varde_agent::Asked) -> Result<Option<Value>> {
    let (kind, tool, input) = (asked.kind, asked.tool.as_str(), &asked.input);
    // The worktree, wherever in it the session has gone.
    let root = varde_core::project::root(std::path::Path::new(&asked.cwd));
    let cwd = root.to_string_lossy();
    let cwd = cwd.as_ref();
    let now = now_ms();
    let Some(task) = store
        .conn
        .query_row("SELECT task FROM run WHERE id = ?1", [run], |r| r.get::<_, Option<String>>(0))?
    else {
        return Ok(None);
    };
    let Some(trust) = steward::trusted(&store.conn, &task, now)? else { return Ok(None) };
    if steward::reserved(tool, input, cwd).is_some() {
        return Ok(None);
    }
    // A rule the user gave answers at once, steward or not, and is kept
    // as an ask the rule settled, so what it let through can be read back.
    if kind == Kind::Permission && steward::allowed(&trust.allow, tool, input, cwd) {
        let id = steward::ask(&store.conn, &task, run, kind, tool, &input.to_string(), now)?;
        let allow = json!({"behavior": "allow"});
        steward::settle(&store.conn, &id, Some(&allow.to_string()), None, "rule", now)?;
        return Ok(Some(allow));
    }
    if !watched(paths) {
        return Ok(None);
    }
    let id = steward::ask(&store.conn, &task, run, kind, tool, &input.to_string(), now)?;
    let deadline = Instant::now() + WAIT_MAX.min(Duration::from_millis((trust.until - now).max(0) as u64));
    loop {
        std::thread::sleep(POLL);
        // Answered, or passed to the user.
        if let Some(answer) = steward::settled(&store.conn, &id)? {
            return Ok(answer.and_then(|a| serde_json::from_str(&a).ok()));
        }
        // Answered at the keyboard (the tool ran, or the turn went on), or
        // nobody is there to answer: the hook stops waiting.
        let moved: bool = store.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM event WHERE run = ?1 AND ts > ?2
               AND kind IN ('unblocked','prompt','turn_end','delegated','interrupted','ended'))",
            (run, now),
            |r| r.get(0),
        )?;
        // Taken back (`T`, `trust --off`, the steward off): the user's again.
        let handed = steward::trusted(&store.conn, &task, now_ms())?.is_some();
        if moved || !handed || !watched(paths) || Instant::now() >= deadline {
            steward::settle(&store.conn, &id, None, None, "gone", now_ms())?;
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_minutes_hours_or_days() {
        assert_eq!(span("30m").unwrap(), 30 * 60_000);
        assert_eq!(span("2h").unwrap(), 2 * 3_600_000);
        assert_eq!(span("1d").unwrap(), 86_400_000);
        assert!(span("2").is_err());
        assert!(span("0h").is_err());
        assert!(span("h").is_err());
    }
}
