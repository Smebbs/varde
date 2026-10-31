//! `varde run <run>`: the process in the terminal. It starts the agent as
//! its child, sharing the terminal, and waits — so whether a session is
//! alive is observed, not inferred from hooks.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use varde_agent::Launch;
use varde_core::status::Status;
use varde_core::store::{self, Store};

use crate::{git, now_ms, process, tmux, Paths};

pub fn run(paths: &Paths, run_id: &str) -> Result<i32> {
    let mut store = paths.open(Duration::from_secs(2))?;
    // The board types the six characters `varde show` prints.
    let run_id = &crate::memory_cmd::resolve_run(&store.conn, run_id)
        .map_err(|e| anyhow!("{e}; runs come from `varde start`"))?;
    let row = store.conn.query_row(
        "SELECT r.agent, r.native_id, r.relation, r.cwd, r.prompt, r.pid, r.launch, t.title, p.native_id, r.task,
                r.background
         FROM run r JOIN task t ON t.id = r.task LEFT JOIN run p ON p.id = r.parent_run
         WHERE r.id = ?1",
        [run_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, bool>(10)?,
            ))
        },
    );
    let (agent, native, relation, cwd, prompt, pid, launch, title, parent_native, task, background) =
        row.with_context(|| format!("no run {run_id}; runs come from `varde start`"))?;
    // Said for the task, not the run: the user typed `varde attach`, and a
    // second one racing the first lands here.
    let already = || {
        let short = varde_core::task::short(&task);
        anyhow!("{short} is already running, or starting; `varde attach {short}` goes to it")
    };
    if launch != "managed" || pid.is_some() {
        return Err(already());
    }
    let adapter = varde_agent::by_name(&agent).with_context(|| format!("unknown agent {agent}"))?;
    // Empty when the agent picks its own id.
    let preset = (!native.is_empty()).then_some(native.as_str());
    let how = match relation.as_str() {
        "resume" => Launch::Resume { id: &native, prompt: prompt.as_deref() },
        "fork" => {
            Launch::Fork { from: parent_native.as_deref().context("fork without a parent")?, id: preset }
        }
        _ => Launch::New { id: preset, name: &title, prompt: prompt.as_deref() },
    };
    let (argv, env) = adapter.launch(how);
    // Started by a schedule, with nobody at it: to the end of its reply,
    // its output kept in a log beside the store.
    let config = crate::config::get();
    let argv = if background { config.background(&agent, adapter.background(argv)) } else { argv };
    let argv = config.launch(&agent, argv);
    let log = if background { Some(background_log(paths, run_id)?) } else { None };
    crate::job::sync_memory(paths);

    // Claim the run before anything starts, so two `varde run`s of it
    // cannot both launch the agent.
    let claimed = store.conn.execute("UPDATE run SET pid = 0 WHERE id = ?1 AND pid IS NULL", [run_id])?;
    if claimed == 0 {
        return Err(already());
    }
    // Else spawning fails as if the agent were missing.
    if let Some(dir) = cwd.as_deref().filter(|d| !Path::new(d).is_dir()) {
        store.record_exit(run_id, 127, now_ms())?;
        return Err(anyhow!("the session's directory {dir} is gone"));
    }

    // Inside tmux the board can go to this pane while the session runs. A
    // window the board opened is the task's already; a pane of your own
    // is tagged only until the agent exits, so nothing ever closes it.
    // A background run has no pane of its own.
    let here = if background { None } else { tmux::this_pane() };
    let pane = here.as_ref().map(|(p, _)| p.clone());
    let lent = here.filter(|(_, was)| *was != task);
    if let Some((pane, _)) = &lent {
        let _ = tmux::server().tag(pane, &task);
    }

    // A new worktree's setup runs here, in the terminal, where its output
    // can be read and ^C stops it. It runs once, whatever its outcome, at
    // the worktree's root, whichever directory the session resumes in.
    if let Some((setup, root)) = cwd.as_deref().and_then(|d| git::take_setup(Path::new(d))) {
        eprintln!("varde: setting up the worktree: {setup}");
        let mut sh = Command::new("sh");
        sh.args(["-c", &setup]).current_dir(root);
        match &log {
            Some(log) => to_log(&mut sh, log)?,
            None => foreground(&mut sh),
        }
        // ^C stops the setup, not us: the run is claimed and must launch.
        match ignoring_terminal_signals(|| sh.status()) {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("varde: the setup failed ({s}); the session starts anyway"),
            Err(e) => eprintln!("varde: the setup did not run ({e}); the session starts anyway"),
        }
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).envs(env).env("VARDE_RUN", run_id);
    if let Some(dir) = &cwd {
        cmd.current_dir(dir);
    }
    match &log {
        // Its hooks track it, though its CLI says it is scripted.
        Some(log) => to_log(cmd.env(BACKGROUND, "1"), log)?,
        None => foreground(&mut cmd),
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            give_back(lent);
            // Nothing ran: say so, rather than leave a launch that never was.
            store.record_exit(run_id, 127, now_ms())?;
            return Err(e).with_context(|| format!("starting {}", argv[0]));
        }
    };
    let pid = child.id();
    // Without its pid the run only looks gone early; the agent is still
    // waited for, so its exit is recorded.
    let _ = store.conn.execute(
        "UPDATE run SET pid = ?2, pid_start = ?3 WHERE id = ?1",
        params![run_id, pid, process::start_time(pid).unwrap_or(0)],
    );
    drop(store);

    // The agent shares the terminal: ^C and ^\ are for it, not for us.
    // A closed terminal hangs both of us up; staying alive until the agent
    // exits is what lets the exit be recorded. Ignored after the spawn.
    // SIGTERM, sent to us rather than the terminal, is passed on to the
    // agent, and we go on waiting: dying first would orphan it.
    let (stop, stopped) = mpsc::channel::<()>();
    let status = ignoring_terminal_signals(|| {
        passing_on_sigterm(pid, || {
            std::thread::scope(|s| {
                s.spawn(|| watch_interrupts(paths, run_id, &task, &agent, pane.as_deref(), stopped));
                let status = child.wait();
                drop(stop);
                status
            })
        })
    });
    give_back(lent);
    let status = status?;
    let code = status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    });

    let now = now_ms();
    let recorded = paths.open(Duration::from_secs(5)).and_then(|mut store| {
        for run in store.record_exit(run_id, code, now)? {
            crate::job::maybe_fold(paths, &store, &run, "ended", now)?;
        }
        Ok(())
    });
    if recorded.is_err() {
        // The end must not be lost: leave it for the next command.
        crate::pending::append_exit(paths, run_id, code, now)?;
    }
    Ok(code)
}

/// Set for a session a schedule started in the background: its hooks
/// track it, though its agent's print mode says it is run by a script.
pub const BACKGROUND: &str = "VARDE_BACKGROUND";

/// Where a background run's output goes: what the agent printed, and any
/// failure to start.
pub fn background_log(paths: &Paths, run: &str) -> Result<PathBuf> {
    let dir = paths.home.join("background");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{run}.log")))
}

/// `cmd` with no terminal: nothing to read, its output appended to `log`.
fn to_log(cmd: &mut Command, log: &Path) -> Result<()> {
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    cmd.stdin(std::process::Stdio::null()).stdout(out.try_clone()?).stderr(out);
    Ok(())
}

/// The child gets the terminal's signals whatever we ignore: the TUI
/// runs one session after another in one process.
fn foreground(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            for sig in TERMINAL_SIGNALS {
                libc::signal(sig, libc::SIG_DFL);
            }
            Ok(())
        });
    }
}

/// The signals a terminal sends its foreground job.
const TERMINAL_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP];

/// `f`, with the terminal's signals ignored until it returns.
fn ignoring_terminal_signals<T>(f: impl FnOnce() -> T) -> T {
    let before = TERMINAL_SIGNALS.map(|sig| unsafe { libc::signal(sig, libc::SIG_IGN) });
    let out = f();
    for (sig, was) in TERMINAL_SIGNALS.into_iter().zip(before) {
        unsafe { libc::signal(sig, was) };
    }
    out
}

/// The agent a SIGTERM to `varde run` is passed on to; 0 for none.
static AGENT: AtomicI32 = AtomicI32::new(0);

extern "C" fn pass_on(sig: libc::c_int) {
    let pid = AGENT.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe { libc::kill(pid, sig) };
    }
}

/// `f`, with a SIGTERM to this process passed on to `agent`.
fn passing_on_sigterm<T>(agent: u32, f: impl FnOnce() -> T) -> T {
    AGENT.store(agent as i32, Ordering::SeqCst);
    let handler: extern "C" fn(libc::c_int) = pass_on;
    let before = unsafe { libc::signal(libc::SIGTERM, handler as libc::sighandler_t) };
    let out = f();
    unsafe { libc::signal(libc::SIGTERM, before) };
    AGENT.store(0, Ordering::SeqCst);
    out
}

/// Put the pane `varde run` tagged back as it was.
fn give_back(lent: Option<(String, String)>) {
    if let Some((pane, was)) = lent {
        let _ = tmux::server().tag(&pane, &was);
    }
}

/// How often the transcript is checked for an interrupt.
const POLL: Duration = Duration::from_secs(1);

/// The agent sends no event when the user interrupts a turn, so the run
/// would stay `working` (or `blocked`) until its next prompt. The agent
/// does write the interrupt to its transcript: watch that, until `stop`
/// is dropped. The session may move to another conversation (`/clear`,
/// `/resume`), a run of its own in the same process: the newest is
/// watched. A turn that ends while its tmux `pane` is on screen is read
/// there, whether or not a board is running to see it.
fn watch_interrupts(
    paths: &Paths,
    run_id: &str,
    task: &str,
    agent: &str,
    pane: Option<&str>,
    stop: mpsc::Receiver<()>,
) {
    let Some(adapter) = varde_agent::by_name(agent) else { return };
    let mut store = None;
    // The run watched, its transcript, and how far into it has been read.
    let mut at: Option<(String, PathBuf, u64)> = None;
    // When the transcript was last read: what it shows now came after.
    let mut read_at = now_ms();
    while let Err(RecvTimeoutError::Timeout) = stop.recv_timeout(POLL) {
        let now = now_ms();
        if store.is_none() {
            store = paths.open(Duration::from_millis(100)).ok();
        }
        let Some(s) = &store else { continue };
        // A run learns its transcript from its first event. A resume or
        // fork starts with history older than the run: skip it.
        if let Some((run, path, relation)) =
            newest(s, run_id).filter(|(r, ..)| at.as_ref().is_none_or(|a| a.0 != *r))
        {
            let len = if relation == "fresh" { 0 } else { std::fs::metadata(&path).map_or(0, |m| m.len()) };
            at = Some((run, path, len));
        }
        let Some((run, path, from)) = at.as_mut() else { continue };
        if let Some(pane) = pane {
            read_if_shown(s, task, run, pane, now);
        }
        if let Some(idle) = cancelled(s, adapter.as_ref(), run, now) {
            let _ = s.record_interrupt(run, idle);
        }
        let Ok(new) = read_from(path, *from) else { continue };
        // Only whole lines: the last may still be being written.
        let Some(end) = new.iter().rposition(|b| *b == b'\n') else { continue };
        let text = String::from_utf8_lossy(&new[..end]);
        // Events count in the order they happened: the interrupt as of the
        // line's own time, so a prompt typed right after it still wins.
        // A line that does not say is counted now, unless a prompt came
        // since the last read, which it may have been before.
        let when =
            text.lines().filter(|l| adapter.interrupted(l)).last().and_then(|l| match adapter.line_time(l) {
                Some(t) => Some(t.min(now)),
                None => (!prompted_since(s, run, read_at)).then_some(now),
            });
        if when.is_some_and(|t| s.record_interrupt(run, t).is_err()) {
            continue; // read it again next time
        }
        *from += end as u64 + 1;
        read_at = now;
    }
}

/// Mark `task` read when `run` is unread and `pane`, the one it runs in,
/// is on screen: you are in it. tmux is asked only then.
fn read_if_shown(store: &Store, task: &str, run: &str, pane: &str, now: i64) {
    let unread = store::view(&store.conn, run).is_ok_and(|v| v.unread);
    if unread && tmux::server().shown(pane) {
        let _ = store::mark_seen(&store.conn, task, now);
    }
}

/// How long the agent must have said it is idle before a run still
/// `working` counts as cancelled. A turn that ends normally goes idle as
/// its turn-end hook fires; by then that hook has long been recorded.
const IDLE_SETTLED_MS: i64 = 1500;

/// When `run`'s turn was cancelled with nothing in its transcript, as the
/// agent tells outside it: the run is `working` by its events, and its
/// agent has said it is idle since, for long enough that no turn end is
/// still on its way.
fn cancelled(store: &Store, adapter: &dyn varde_agent::Agent, run: &str, now: i64) -> Option<i64> {
    let v = store::view(&store.conn, run).ok()?;
    if v.status != Some(Status::Working) {
        return None;
    }
    let (pid, native): (i64, String) = store
        .conn
        .query_row("SELECT pid, native_id FROM run WHERE id = ?1 AND pid > 0", [run], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .ok()?;
    adapter
        .idle_since(pid, &native)
        .filter(|t| v.why.at.is_some_and(|at| *t > at) && now - t >= IDLE_SETTLED_MS)
}

fn prompted_since(store: &Store, run: &str, ts: i64) -> bool {
    store
        .conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM event WHERE run = ?1 AND kind = 'prompt' AND ts > ?2)",
            params![run, ts],
            |r| r.get(0),
        )
        .unwrap_or(false)
}

/// The newest run of `run_id`'s process that knows its transcript, with
/// its relation.
fn newest(store: &Store, run_id: &str) -> Option<(String, PathBuf, String)> {
    store
        .conn
        .query_row(
            "SELECT id, transcript, relation FROM run
             WHERE transcript IS NOT NULL AND (id = ?1 OR (pid > 0
               AND (pid, coalesce(pid_start, 0)) = (SELECT pid, coalesce(pid_start, 0) FROM run WHERE id = ?1)))
             ORDER BY started DESC, rowid DESC LIMIT 1",
            [run_id],
            |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get(2)?)),
        )
        .ok()
}

fn read_from(path: &Path, from: u64) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}
