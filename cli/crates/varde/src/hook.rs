//! `varde hook <Event>`: called by the agent CLI on every hook.
//!
//! It must never fail the agent and never lose an event. Whatever
//! happens it prints `{}` and exits 0. When the store cannot be written,
//! the event goes to the pending file and the next varde command applies
//! it.

use std::io::{Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use serde_json::Value;
use varde_core::event::{Kind, Source};

use crate::{now_ms, pending, Paths};

/// How long to wait for another writer. Writes take well under a
/// millisecond, so hitting this means something is wrong, and the
/// pending file is the better place to wait.
const BUSY: Duration = Duration::from_millis(100);

/// Set by every process varde spawns for itself, so its own headless
/// agent calls are not tracked as sessions.
const INTERNAL: &str = "VARDE_INTERNAL";

/// `varde hook [--agent NAME] <Event>`, read leniently and before the
/// rest of the command line: a plugin newer or older than the binary may
/// pass what it does not know, and the hook must still answer.
pub fn args(args: &[String]) -> (String, String) {
    let mut agent = "claude".to_string();
    let mut event = String::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--agent" {
            agent = it.next().cloned().unwrap_or_default();
        } else if let Some(name) = a.strip_prefix("--agent=") {
            agent = name.to_string();
        } else if !a.starts_with('-') {
            // The last word: an unknown flag's value may come before it.
            event = a.clone();
        }
    }
    (agent, event)
}

pub fn run(paths: &Paths, agent: &str, event: &str) {
    // Read what the agent sends even when nothing is recorded: a hook
    // that exits first leaves the agent writing into a closed pipe.
    let mut input = String::new();
    let read = std::io::stdin().read_to_string(&mut input);
    let mut answer = None;
    // varde's own headless calls, and agents run by a script rather than
    // a person, are not sessions anyone comes back to.
    // A session a schedule started is in print mode, and is tracked.
    let background = std::env::var_os(crate::run::BACKGROUND).is_some_and(|x| !x.is_empty());
    let tracked = std::env::var_os(INTERNAL).is_none_or(|x| x.is_empty())
        && (background || !varde_agent::by_name(agent).is_some_and(|a| a.scripted()));
    if tracked {
        // A panic must not reach the agent as a failed hook.
        panic::set_hook(Box::new(|_| {}));
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            read.context("read stdin")?;
            record(paths, agent, event, &input)
        }));
        match outcome {
            Ok(Ok(a)) => answer = a,
            Ok(Err(e)) => log(paths, &format!("hook {event}: {e:#}")),
            Err(_) => log(paths, &format!("hook {event}: panic")),
        }
    }
    // A closed stdout must not make the hook fail.
    let reply = answer.map_or_else(|| "{}".to_string(), |a| a.to_string());
    let _ = writeln!(std::io::stdout(), "{reply}");
}

/// Records the event; returns the agent's reply handing the session
/// context, if any.
fn record(paths: &Paths, agent_name: &str, event: &str, input: &str) -> Result<Option<Value>> {
    let payload: Value = serde_json::from_str(input).context("payload is not JSON")?;
    let agent = varde_agent::by_name(agent_name).context("unknown agent")?;
    let Some(norm) = agent.normalize(event, &payload) else { return Ok(None) };
    let kept = agent.keep(payload);
    let now = now_ms();
    let claim = crate::run_env();
    // The agent process, looked up once and only when needed.
    let found = std::cell::OnceCell::new();
    let found = || found.get_or_init(|| crate::process::agent(agent.process_names()));
    let me = || found().as_ref()?.pid.map(|p| (p as i64, crate::process::start_time(p).unwrap_or(0)));

    // Only the event itself goes to the pending file if it cannot be
    // recorded. What follows a recorded event (linking a fork, queueing a
    // fold, the guidance) is logged on failure: replaying the event for
    // it would record it twice.
    let mut store = match paths.open(BUSY) {
        Ok(s) => s,
        Err(e) => return defer(paths, agent.name(), event, now, claim.as_deref(), &me, &kept, e),
    };
    // A fork varde did not start is linked to its family's task: after
    // its start (its transcript exists by then), and again before its
    // first prompt, which would otherwise make it a task of its own.
    let link = |store: &mut varde_core::store::Store| {
        store
            .link_fork(agent.name(), &norm.native_id, &|t| agent.family_key(t), &|t, k| agent.in_family(t, k))
    };
    if norm.kind == Kind::Prompt {
        if let Err(e) = link(&mut store) {
            log(paths, &format!("hook {event}: linking fork: {e:#}"));
        }
    }
    let run = match store.ingest_from(agent.name(), &norm, &kept.to_string(), now, claim.as_deref(), &me) {
        Ok(run) => run,
        Err(e) => return defer(paths, agent.name(), event, now, claim.as_deref(), &me, &kept, e),
    };
    let after = || -> Result<Option<Value>> {
        if norm.kind == Kind::Started(Source::Fork) {
            link(&mut store)?;
        }
        let Some(run) = run else { return Ok(None) };
        // Where the session runs, so a client can go to it rather than
        // fork it, and for a session varde did not launch, its process:
        // once that is gone the session is over, whatever its last event
        // said. Learnt once per run, at its start or first prompt.
        if matches!(norm.kind, Kind::Started(_) | Kind::Prompt) {
            if claim.as_deref() == Some(run.as_str()) {
                adopt_agent(&store, &run, &me)?;
            }
            if let Some(a) = found() {
                // With the terminal's ctime, so its number handed to
                // another once it closes does not keep the run alive.
                let ctime = a.tty.as_deref().and_then(varde_core::store::tty_ctime);
                store.conn.execute(
                    "UPDATE run SET tty = coalesce(tty, ?2),
                       tty_ctime = CASE WHEN coalesce(tty, ?2) IS ?2 THEN coalesce(tty_ctime, ?3) ELSE tty_ctime END
                     WHERE id = ?1",
                    (&run, &a.tty, ctime),
                )?;
            }
        }
        crate::job::maybe_fold(paths, &store, &run, norm.kind.name(), now)?;
        // What the session is given: guidance and memory as it starts,
        // and memory with the prompt that made an ad-hoc session's task.
        // Memory that cannot be read leaves the guidance.
        let updates = crate::job::updates_on(paths);
        let context = match norm.kind {
            Kind::Started(source) => {
                match crate::deliver::on_start(&store.conn, &run, source, now, updates) {
                    Ok(c) => c,
                    Err(e) => {
                        log(paths, &format!("hook {event}: memory: {e:#}"));
                        crate::deliver::guidance_for(&store.conn, &run, updates)?
                    }
                }
            }
            Kind::Prompt => crate::deliver::on_prompt(&store.conn, &run, norm.prompt.as_deref(), now)?,
            _ => None,
        };
        Ok(context.map(|c| agent.hook_reply(event, &c)))
    };
    Ok(after().unwrap_or_else(|e| {
        log(paths, &format!("hook {event}: after recording: {e:#}"));
        None
    }))
}

/// `varde run` records the process it starts, which is a wrapper when the
/// configured command does not `exec` the agent. The claimed session's
/// own hook knows the agent process: below the wrapper, it takes its
/// place, so `/clear` and `/resume` are told apart from a session started
/// inside this one, and stopping the task stops the agent itself.
fn adopt_agent(
    store: &varde_core::store::Store,
    run: &str,
    me: &dyn Fn() -> Option<(i64, i64)>,
) -> Result<()> {
    let recorded: Option<(i64, i64)> = store
        .conn
        .query_row(
            "SELECT pid, coalesce(pid_start, 0) FROM run WHERE id = ?1 AND launch = 'managed' AND pid > 0",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (Some(wrapper), Some(agent)) = (recorded, me()) else { return Ok(()) };
    if wrapper.0 == agent.0 || !crate::process::descends(agent.0 as u32, wrapper) {
        return Ok(());
    }
    store.conn.execute(
        "UPDATE run SET pid = ?3, pid_start = ?4
         WHERE exit_code IS NULL AND pid = ?1 AND coalesce(pid_start, 0) = ?2",
        (wrapper.0, wrapper.1, agent.0, agent.1),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn defer(
    paths: &Paths,
    agent: &str,
    event: &str,
    now: i64,
    claim: Option<&str>,
    me: &dyn Fn() -> Option<(i64, i64)>,
    kept: &Value,
    e: anyhow::Error,
) -> Result<Option<Value>> {
    // The process goes with the event: it tells a claimed session from
    // one started inside it, and is what ends a session varde did not
    // launch.
    let process = me();
    pending::append(paths, agent, event, now, claim, process, kept)
        .with_context(|| format!("store failed ({e:#}) and so did the pending file"))?;
    log(paths, &format!("hook {event}: deferred to pending: {e:#}"));
    Ok(None)
}

/// Best effort: a log that cannot be written must not fail the hook.
fn log(paths: &Paths, line: &str) {
    let path = paths.log();
    // The very first hook may come before anything made the store.
    let _ = paths.make_home();
    // Keep the log from growing without bound.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", now_ms(), line);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_event_is_the_last_word_whatever_flags_come_before() {
        let args = |a: &[&str]| super::args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(args(&["Stop"]), ("claude".into(), "Stop".into()));
        assert_eq!(args(&["--agent", "pi", "agent_settled"]), ("pi".into(), "agent_settled".into()));
        assert_eq!(args(&["--agent=pi", "x"]), ("pi".into(), "x".into()));
        // A flag a newer plugin passes, with its value apart.
        assert_eq!(args(&["--source", "plugin", "UserPromptSubmit"]).1, "UserPromptSubmit");
        assert_eq!(args(&["--new-flag", "Stop"]).1, "Stop");
        assert_eq!(args(&["--agent"]), (String::new(), String::new()));
    }
}
