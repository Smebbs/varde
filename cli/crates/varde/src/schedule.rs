//! Scheduled tasks. A schedule makes varde start a task at a time, as
//! you would, but in the background: `start --fresh`, then `varde run`
//! with no terminal, the agent in its print mode. It is tracked as any
//! session is, on the board and by its hooks; once it is done, RET
//! resumes it as any task, and while it works, RET takes it over
//! (`start --take-over`).
//!
//! There is no daemon. `varde tick` fires what is due and exits; the
//! heartbeat (`varde schedule --install`, a systemd timer or a
//! LaunchAgent) runs it every minute, and a board's refresh starts one
//! when a row is due. A timer missed while the machine slept fires on
//! wake, so a start due at 09:00 with the lid opened at 09:30 happens at
//! 09:30, once.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use varde_core::board::Board;
use varde_core::plan::Want;
use varde_core::schedule::{self, Rule};
use varde_core::status::Status;
use varde_core::store::Store;
use varde_core::task;

use crate::{config, git, now_ms, start, Paths};

const BUSY: Duration = Duration::from_secs(2);

/// A session left at its prompt this long is the last occurrence's,
/// finished: the next one stops it. One answered more recently may be
/// you, reading it.
const IDLE_MS: i64 = 15 * 60 * 1000;

/// A schedule that failed to fire is tried again after this long.
const RETRY_MS: i64 = 10 * 60 * 1000;

/// A board's refresh starts a tick at most this often.
const KICK_MS: i64 = 30 * 1000;

/// `varde schedule <task> RULE`, and `varde new --when`: give the task
/// its schedule, or change it. What a first start would ask is settled
/// now, since nobody is there to answer when it fires.
pub fn arm(store: &Store, task_id: &str, text: &str, worktree: Option<bool>) -> Result<String> {
    let now = now_ms();
    let local = schedule::from_ms(now);
    let rule = schedule::parse(text, local).map_err(anyhow::Error::msg)?;
    let first = rule.first(local).context("that schedule has no time in range")?;
    let (project, origin, started): (Option<String>, Option<String>, bool) = store.conn.query_row(
        "SELECT project, origin, EXISTS (SELECT 1 FROM run WHERE task = t.id) FROM task t WHERE id = ?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let short = task::short(task_id);
    if !started {
        if start::project_unsure(store, origin.as_deref(), project.as_deref())? {
            bail!("{short}'s project is unsure, and nobody is there to say when it starts: varde set {short} --project <dir> first");
        }
        if worktree.is_none() && would_ask(Path::new(project.as_deref().unwrap_or_default())) {
            bail!("nobody is there to say whether {short} gets a worktree when it starts: pass --worktree always or --worktree never");
        }
    }
    store.conn.execute(
        "INSERT INTO schedule (task, rule, next_due, worktree, created) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (task) DO UPDATE SET rule = ?2, next_due = ?3, worktree = coalesce(?4, worktree),
           tried = NULL, error = NULL",
        params![task_id, text.trim(), schedule::to_ms(first), worktree, now],
    )?;
    // Scheduling a task is taking it on.
    store.conn.execute("UPDATE task SET inbox = 0 WHERE id = ?1", [task_id])?;
    let when = schedule::when(first, local);
    // Without the heartbeat it starts only while a board is open.
    let alone =
        if heartbeat().is_none() { "; nothing wakes varde yet: varde schedule --install" } else { "" };
    Ok(if rule.repeats() {
        // A series that has run goes on, from its next start.
        let ran: bool = store.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1 AND background)",
            [task_id],
            |r| r.get(0),
        )?;
        format!("{short} repeats ({}); {} {when}{alone}", text.trim(), if ran { "next" } else { "first" })
    } else {
        format!("{short} starts {when}{alone}")
    })
}

/// What `arm` would refuse for a task not made yet, in `project`.
pub fn check(project: &Path, text: &str, worktree: Option<bool>) -> Result<()> {
    schedule::parse(text, schedule::from_ms(now_ms())).map_err(anyhow::Error::msg)?;
    if worktree.is_none() && would_ask(project) {
        bail!("nobody is there to say whether it gets a worktree when it starts: pass --worktree always or --worktree never");
    }
    Ok(())
}

/// Whether a first start in `project` would ask about a worktree: a
/// repository with commits, and no setting that decides.
fn would_ask(project: &Path) -> bool {
    let decided =
        config::Repo::read(project).ok().and_then(|r| r.worktree).or(config::get().worktree.default);
    decided.and_then(|p| p.want()).is_none()
        && varde_core::project::root(project).join(".git").exists()
        && git::has_commits(project)
}

/// `varde schedule <task> --now`: its next run starts now, as its time
/// would, unless something holds it up, which it says.
pub fn now(paths: &Paths, reference: &str) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    let Some(shown) = of(&store, &id)? else { bail!("{short} has no schedule") };
    let state: String = store.conn.query_row("SELECT state FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
    if state != "open" {
        bail!("{short} is {state}: varde reopen {short} first");
    }
    let runs = |s: &Store| -> Result<i64> {
        Ok(s.conn.query_row("SELECT count(*) FROM run WHERE task = ?1", [&id], |r| r.get(0))?)
    };
    let before = runs(&store)?;
    store.conn.execute(
        "UPDATE schedule SET next_due = ?2, tried = NULL, error = NULL WHERE task = ?1",
        params![id, now_ms()],
    )?;
    drop(store);
    tick(paths)?;
    let store = paths.open(BUSY)?;
    if runs(&store)? > before {
        println!(
            "{short} started in the background; {}",
            of(&store, &id)?.map_or(shown.rule, |s| s.text(&store, paths, &id).unwrap_or(s.rule))
        );
    } else {
        let why = match of(&store, &id)?.and_then(|s| s.error) {
            Some(e) => format!("failed to start: {e}"),
            None => held(&store, paths, &id)?,
        };
        println!("{short} is due now, {why}");
    }
    Ok(())
}

/// `varde schedule <task> [RULE | --off]`.
pub fn set(
    paths: &Paths,
    reference: &str,
    rule: Option<String>,
    off: bool,
    worktree: Option<bool>,
) -> Result<()> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let short = task::short(&id);
    if off {
        let gone = store.conn.execute("DELETE FROM schedule WHERE task = ?1", [&id])?;
        let state: String =
            store.conn.query_row("SELECT state FROM task WHERE id = ?1", [&id], |r| r.get(0))?;
        println!(
            "{}",
            if gone > 0 && state == "open" {
                format!("{short} is no longer scheduled; varde done {short} closes it")
            } else if gone > 0 {
                format!("{short} is no longer scheduled")
            } else {
                format!("{short} had no schedule")
            }
        );
        return Ok(());
    }
    match rule {
        Some(rule) => println!("{}", arm(&store, &id, &rule, worktree)?),
        None => match of(&store, &id)? {
            Some(s) => println!("{}", s.text(&store, paths, &id)?),
            None => println!("{short} has no schedule"),
        },
    }
    Ok(())
}

/// A task's schedule, for `show`: its rule, when it is next due, and why
/// it failed to fire, if it did.
pub struct Shown {
    pub rule: String,
    pub due: i64,
    pub error: Option<String>,
}

pub fn of(store: &Store, id: &str) -> Result<Option<Shown>> {
    Ok(store
        .conn
        .query_row("SELECT rule, next_due, error FROM schedule WHERE task = ?1", [id], |r| {
            Ok(Shown { rule: r.get(0)?, due: r.get(1)?, error: r.get(2)? })
        })
        .optional()?)
}

/// A task's schedule, if it repeats: `done` then ends the run, not the
/// series.
pub fn repeating(store: &Store, id: &str) -> Result<Option<Shown>> {
    Ok(of(store, id)?
        .filter(|s| schedule::parse(&s.rule, schedule::from_ms(now_ms())).is_ok_and(|r| r.repeats())))
}

impl Shown {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({"rule": self.rule, "due": self.due, "error": self.error})
    }

    /// `every weekday 09:00; next Mon 09:00`, and a failure, or what
    /// holds up a start that is due.
    pub fn text(&self, store: &Store, paths: &Paths, id: &str) -> Result<String> {
        let now = now_ms();
        let at = schedule::when(schedule::from_ms(self.due), schedule::from_ms(now));
        let state: String =
            store.conn.query_row("SELECT state FROM task WHERE id = ?1", [id], |r| r.get(0))?;
        match state.as_str() {
            "open" => {}
            "parked" => return Ok(format!("{}; paused while it is parked", self.rule)),
            _ => return Ok(format!("{}; not while it is {state}, and again if it is reopened", self.rule)),
        }
        if let Some(e) = &self.error {
            return Ok(format!("{}; due {at}, failed to start: {e}", self.rule));
        }
        if self.due > now {
            return Ok(format!("{}; next {at}", self.rule));
        }
        Ok(match held(store, paths, id)? {
            // Nothing holds it: what happens comes first.
            h if h == STARTING => format!("{}; {h} (due {at})", self.rule),
            h => format!("{}; due {at}, {h}", self.rule),
        })
    }
}

/// A tick this recent starts what is due within a minute, as the
/// heartbeat runs every minute; past due by this much, the board says
/// `late`.
const LATE_MS: i64 = 5 * 60 * 1000;

/// What a start nothing holds up does.
const STARTING: &str = "starts within a minute";

/// Why a start that is due has not happened, as [`fire`] decides.
fn held(store: &Store, paths: &Paths, id: &str) -> Result<String> {
    if updating(store, id)? {
        return Ok("waiting for its cairn update".into());
    }
    if let Some(r) = start::run_facts(store, id)?.into_iter().find(|r| r.live()) {
        let ours = crate::done::live_runs(store, id)?.iter().any(|(run, _, _)| *run == r.id);
        let what = match r.status {
            Some(Status::Waiting) if ours => {
                let last = varde_core::store::view(&store.conn, &r.id)?.last.unwrap_or(0);
                let at = schedule::when(schedule::from_ms(last + IDLE_MS), schedule::from_ms(now_ms()));
                format!(", which is stopped at {at} if it is still idle")
            }
            _ => String::new(),
        };
        return Ok(format!("waiting for session {} to end{what}", task::short(&r.id)));
    }
    Ok(match since_tick(paths) {
        Some(d) if d.as_millis() as i64 <= LATE_MS => STARTING.into(),
        _ => "but varde tick is not running: varde doctor".into(),
    })
}

/// Whether a cairn update of `task` is queued or running.
fn updating(store: &Store, task: &str) -> Result<bool> {
    Ok(store.conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM job j JOIN run r ON r.id = j.run
                        WHERE r.task = ?1 AND j.state IN ('queued', 'running'))",
        [task],
        |r| r.get(0),
    )?)
}

/// The tick's lock: one at a time, and its file's age says when one last
/// ran. `None` while another holds it.
fn lock(paths: &Paths) -> Result<Option<File>> {
    paths.make_home()?;
    let file = File::create(lock_path(paths))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    Ok(Some(file))
}

fn lock_path(paths: &Paths) -> PathBuf {
    paths.home.join("tick.lock")
}

/// How long ago a tick last ran, if one ever did.
pub fn since_tick(paths: &Paths) -> Option<Duration> {
    std::fs::metadata(lock_path(paths)).ok()?.modified().ok()?.elapsed().ok()
}

/// Start a detached tick when a row the board shows is due, at most every
/// `KICK_MS`: the heartbeat's job, done while a board is open.
pub fn kick(board: &Board, now: i64, paths: &Paths) -> Result<()> {
    let due =
        board.groups.iter().flat_map(|g| &g.rows).any(|r| r.schedule.as_ref().is_some_and(|s| s.due <= now));
    if due && since_tick(paths).is_none_or(|d| d.as_millis() as i64 >= KICK_MS) {
        crate::detach(&["tick"], "the schedule tick")?;
    }
    Ok(())
}

/// `varde tick`: start every open task whose schedule is due.
pub fn tick(paths: &Paths) -> Result<()> {
    let Some(_lock) = lock(paths)? else { return Ok(()) };
    let mut store = paths.open(BUSY)?;
    let now = now_ms();
    let due: Vec<(String, String, Option<bool>)> = {
        let mut stmt = store.conn.prepare(
            "SELECT s.task, s.rule, s.worktree FROM schedule s JOIN task t ON t.id = s.task
             WHERE t.state = 'open' AND s.next_due <= ?1 AND (s.tried IS NULL OR s.tried <= ?2)
             ORDER BY s.next_due",
        )?;
        let rows = stmt.query_map(params![now, now - RETRY_MS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (task, rule, worktree) in due {
        if let Err(e) = fire(&mut store, &task, &rule, worktree, now) {
            store.conn.execute(
                "UPDATE schedule SET tried = ?2, error = ?3 WHERE task = ?1",
                params![task, now, format!("{e:#}")],
            )?;
        }
    }
    Ok(())
}

/// Start `task` for its schedule, unless it must wait: for a cairn update
/// of its last session, so this one starts from what that one learned, or
/// for a session of it still at work. A session left at its prompt is
/// stopped, as `done` stops one, and this one starts on a later tick.
fn fire(store: &mut Store, task: &str, rule: &str, worktree: Option<bool>, now: i64) -> Result<()> {
    if updating(store, task)? {
        return Ok(());
    }
    let live: Vec<_> = start::run_facts(store, task)?.into_iter().filter(|r| r.live()).collect();
    if !live.is_empty() {
        let ours = crate::done::live_runs(store, task)?;
        for r in &live {
            let last = varde_core::store::view(&store.conn, &r.id)?.last.unwrap_or(0);
            let launched = ours.iter().find(|(run, _, _)| *run == r.id);
            if let (Some(Status::Waiting), Some((_, pid, _))) = (r.status, launched) {
                if now - last >= IDLE_MS {
                    unsafe { libc::kill(*pid as libc::pid_t, libc::SIGTERM) };
                }
            }
        }
        return Ok(());
    }

    let opts = start::Options {
        want: Want::Fresh,
        worktree,
        context: None,
        agent: None,
        project: None,
        take_over: false,
        run: None,
    };
    let (run, cwd) = match start::prepare(store, task, opts, false)? {
        start::Outcome::Ready { run, cwd, .. } => (run, cwd),
        // Live after all: a session started in between.
        start::Outcome::Focus { .. } => return Ok(()),
        start::Outcome::Ask(ask) => bail!(ask.message()),
    };
    store.conn.execute("UPDATE run SET background = 1 WHERE id = ?1", [&run])?;
    if let Err(e) = launch(&run, &cwd) {
        // Nothing ran: say so, rather than leave a launch that never was.
        store.record_exit(&run, 127, now)?;
        return Err(e);
    }
    let local = schedule::from_ms(now);
    match schedule::parse(rule, local).ok().and_then(|r: Rule| r.next(local, local)) {
        Some(next) => store.conn.execute(
            "UPDATE schedule SET next_due = ?2, fired = ?3, tried = NULL, error = NULL WHERE task = ?1",
            params![task, schedule::to_ms(next), now],
        )?,
        None => store.conn.execute("DELETE FROM schedule WHERE task = ?1", [task])?,
    };
    Ok(())
}

/// `varde run <run>` in a session of its own, with no terminal, so it
/// outlives the tick. Not marked internal, as the tick may be: its
/// agent's hooks must track it.
pub(crate) fn launch(run: &str, cwd: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(crate::exe());
    cmd.args(["run", run])
        .current_dir(cwd)
        .env_remove("VARDE_INTERNAL")
        .env_remove("VARDE_RUN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("starting its session")?;
    Ok(())
}

const UNIT: &str = "varde-tick";
const LABEL: &str = "varde.tick";

/// `varde schedule --install` and `--uninstall`: the heartbeat that runs
/// `varde tick` every minute. On Linux a systemd user timer, which fires
/// on wake for a minute missed in sleep (`Persistent`); on macOS a
/// LaunchAgent, which launchd runs on wake for an interval missed. Either
/// way the sessions a tick starts must outlive it (`KillMode=process`,
/// `AbandonProcessGroup`). It runs this binary with this `PATH`, and the
/// store and settings varde has here.
pub fn install(paths: &Paths, on: bool) -> Result<()> {
    let exe = crate::exe();
    let mut env: Vec<(String, String)> = vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())];
    for (k, v) in std::env::vars() {
        if k.starts_with("VARDE_") && !["VARDE_RUN", "VARDE_INTERNAL"].contains(&k.as_str()) {
            env.push((k, v));
        }
    }
    if std::env::var_os("VARDE_HOME").is_none() {
        env.push(("VARDE_HOME".into(), paths.home.display().to_string()));
    }
    if cfg!(target_os = "macos") {
        launchd(&exe, &env, on)
    } else {
        systemd(&exe, &env, on)
    }
}

fn systemctl(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("running systemctl")?;
    if !out.status.success() {
        bail!("systemctl --user {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn systemd(exe: &Path, env: &[(String, String)], on: bool) -> Result<()> {
    let dir = systemd_dir();
    let (service, timer) = (dir.join(format!("{UNIT}.service")), dir.join(format!("{UNIT}.timer")));
    if !on {
        let _ = systemctl(&["disable", "--now", &format!("{UNIT}.timer")]);
        let _ = std::fs::remove_file(&service);
        let _ = std::fs::remove_file(&timer);
        let _ = systemctl(&["daemon-reload"]);
        println!("removed the heartbeat; scheduled tasks start only while a board is open");
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    // systemd reads `%` as a specifier and a value with spaces whole only quoted.
    let quote = |v: &str| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"));
    let mut unit = String::from(
        "[Unit]\nDescription=varde: start the scheduled tasks that are due\n\n[Service]\nType=oneshot\n",
    );
    unit.push_str(&format!("ExecStart={} tick\n", quote(&exe.to_string_lossy())));
    for (k, v) in env {
        unit.push_str(&format!("Environment={}\n", quote(&format!("{k}={v}"))));
    }
    unit.push_str("# The sessions a tick starts outlive it.\nKillMode=process\n");
    std::fs::write(&service, unit)?;
    std::fs::write(
        &timer,
        "[Unit]\nDescription=varde: start scheduled tasks every minute\n\n[Timer]\nOnCalendar=minutely\nPersistent=true\nAccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n",
    )?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", &format!("{UNIT}.timer")])?;
    println!("installed {}: varde tick runs every minute", timer.display());
    Ok(())
}

fn launchd(exe: &Path, env: &[(String, String)], on: bool) -> Result<()> {
    let plist = plist();
    let uid = unsafe { libc::getuid() };
    let launchctl = |args: &[&str]| std::process::Command::new("launchctl").args(args).output();
    let _ = launchctl(&["bootout", &format!("gui/{uid}/{LABEL}")]);
    if !on {
        let _ = std::fs::remove_file(&plist);
        println!("removed the heartbeat; scheduled tasks start only while a board is open");
        return Ok(());
    }
    let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let vars: String =
        env.iter().map(|(k, v)| format!("    <key>{}</key><string>{}</string>\n", xml(k), xml(v))).collect();
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>tick</string></array>
  <key>StartInterval</key><integer>60</integer>
  <key>RunAtLoad</key><true/>
  <key>AbandonProcessGroup</key><true/>
  <key>EnvironmentVariables</key>
  <dict>
{vars}  </dict>
</dict>
</plist>
"#,
        xml(&exe.to_string_lossy())
    );
    if let Some(d) = plist.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&plist, body)?;
    let out = launchctl(&["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()])
        .context("running launchctl")?;
    if !out.status.success() {
        bail!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    println!("installed {}: varde tick runs every minute", plist.display());
    Ok(())
}

/// Where the heartbeat is installed, if it is.
pub fn heartbeat() -> Option<PathBuf> {
    let p = if cfg!(target_os = "macos") { plist() } else { systemd_dir().join(format!("{UNIT}.timer")) };
    p.exists().then_some(p)
}

fn systemd_dir() -> PathBuf {
    config::home().join("systemd/user")
}

fn plist() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(format!("Library/LaunchAgents/{LABEL}.plist"))
}
