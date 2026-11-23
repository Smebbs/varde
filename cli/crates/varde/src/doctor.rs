//! `varde doctor`: is varde installed and healthy on this machine? Each
//! check says what it found and, when it fails, how to fix it. The report
//! is printed whatever the outcome; the exit code says whether all passed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};
use varde_core::memory;

use crate::{now_ms, Paths};

pub fn run(paths: &Paths, as_json: bool) -> Result<()> {
    // A broken config is one of the checks; it is not also a warning.
    crate::config::quiet();
    let store = paths.open(Duration::from_secs(2));
    let this = std::env::current_exe().ok();
    let on_path = which("varde");

    let mut checks = vec![
        match &store {
            Ok(s) => {
                ok("store", format!("{} (schema {})", paths.db().display(), s.schema_version().unwrap_or(0)))
            }
            Err(e) => fail("store", format!("{e:#}"), "a store newer than this binary needs a newer varde"),
        },
        match (&on_path, &this) {
            (Some(p), Some(t)) if same_file(p, t) => ok("on PATH", p.display().to_string()),
            (Some(p), _) => fail(
                "on PATH",
                format!("{} is not this binary", p.display()),
                "the hooks run the varde on PATH: put this one there, or remove the other",
            ),
            (None, _) => {
                fail("on PATH", "varde is not on PATH".into(), "the hooks call varde by name: put it on PATH")
            }
        },
        match crate::config::load(&crate::config::path()) {
            Ok(_) if !crate::config::path().exists() => ok("config", "none; the defaults".into()),
            Ok(_) => ok("config", crate::config::path().display().to_string()),
            Err(e) => fail("config", format!("{e:#}"), "fix the file, or remove it to use the defaults"),
        },
    ];
    checks.push(agent());
    checks.push(profiles());
    checks.push(sandbox(paths));
    checks.push(bare_repositories());
    checks.extend(agents());
    checks.push(pending(paths));
    if let Ok(s) = &store {
        checks.push(jobs(&s.conn)?);
        checks.push(stale(&s.conn)?);
        checks.push(worktrees(&s.conn)?);
        checks.push(schedules(paths, &s.conn)?);
    }

    if as_json {
        println!("{}", json!({"v": 1, "checks": checks}));
    } else {
        for c in &checks {
            let passed = c["ok"] == true;
            println!(
                "{} {:<9} {}",
                if passed { "ok " } else { "!! " },
                c["name"].as_str().unwrap_or(""),
                c["detail"].as_str().unwrap_or("")
            );
            if let (false, Some(fix)) = (passed, c["fix"].as_str()) {
                println!("    {:<9} fix: {fix}", "");
            }
        }
    }
    let failed: Vec<&str> =
        checks.iter().filter(|c| c["ok"] != true).filter_map(|c| c["name"].as_str()).collect();
    if !failed.is_empty() {
        let n = failed.len();
        anyhow::bail!("{n} check{} failed: {}", if n == 1 { "" } else { "s" }, failed.join(", "));
    }
    Ok(())
}

fn ok(name: &str, detail: String) -> Value {
    json!({"name": name, "ok": true, "detail": detail})
}

fn fail(name: &str, detail: String, fix: &str) -> Value {
    json!({"name": name, "ok": false, "detail": detail, "fix": fix})
}

/// The hooks this varde's plugin subscribes to.
const SHIPPED_HOOKS: &str = include_str!("../../../plugin/hooks/hooks.json");

/// Each agent on PATH must have its hooks installed, for every event its
/// plugin has; at least one agent must be there.
fn agents() -> Vec<Value> {
    let mut out = Vec::new();
    if which("claude").is_some() {
        let found = claude_hooks();
        // A plugin copy installed by an older varde lacks the events added since.
        let shipped: Value = serde_json::from_str(SHIPPED_HOOKS).unwrap_or_default();
        let missing = |p: &PathBuf| -> Vec<String> {
            let have = read_json(p)["hooks"].clone();
            shipped["hooks"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(event, _)| event)
                .filter(|event| have.get(event.as_str()).is_none())
                .cloned()
                .collect()
        };
        let complete = found.iter().find(|p| missing(p).is_empty());
        out.push(match (complete, found.first()) {
            (Some(p), _) => ok("claude", format!("hooks in {}", p.display())),
            (None, Some(p)) if p.ends_with("hooks/hooks.json") => {
                // `cache/<marketplace>/<plugin>/<version>/hooks/hooks.json`:
                // the names it was installed under.
                let names: Vec<String> = p
                    .ancestors()
                    .skip(3)
                    .take(2)
                    .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                let (plugin, market) = match names.as_slice() {
                    [plugin, market] => (plugin.as_str(), market.as_str()),
                    _ => ("varde", "varde"),
                };
                fail(
                    "claude",
                    format!("the plugin in {} has no hook for {}", p.display(), missing(p).join(", ")),
                    &format!(
                        "the plugin is older than this varde: claude plugin marketplace update {market} && \
                         claude plugin update {plugin}@{market}, then restart Claude Code"
                    ),
                )
            }
            (None, Some(p)) => fail(
                "claude",
                format!("the hooks in {} have none for {}", p.display(), missing(p).join(", ")),
                "add them as https://github.com/Smebbs/varde/blob/main/cli/plugin/hooks/hooks.json has them, or install the plugin",
            ),
            (None, None) => fail(
                "claude",
                format!(
                    "no enabled plugin or settings file under {} calls `varde hook`",
                    claude_dir().display()
                ),
                "in Claude Code: /plugin marketplace add Smebbs/varde, then /plugin install varde@varde \
                 (or /plugin to enable it)",
            ),
        });
    }
    if which("pi").is_some() {
        let ext = pi_dir().join("extensions");
        out.push(match pi_extension() {
            Some(p) => ok("pi", format!("extension {}", p.display())),
            None => fail(
                "pi",
                format!(
                    "no extension in {} or listed in {} calls `varde hook`",
                    ext.display(),
                    pi_dir().join("settings.json").display()
                ),
                &format!(
                    "mkdir -p {0} && ln -s <the varde repository>/cli/pi/varde.ts {0}/varde.ts",
                    ext.display()
                ),
            ),
        });
    }
    if out.is_empty() {
        out.push(fail("agents", "neither claude nor pi is on PATH".into(), "install Claude Code or pi"));
    }
    out
}

/// Said where a session starts when `agent` is on PATH but nothing hands
/// its events to varde, which then never sees the session: the most
/// likely thing to be missing on a new machine.
pub fn unhooked(agent: &str) -> Option<String> {
    agents().iter().any(|c| c["name"] == agent && c["ok"] == false).then(|| {
        format!("{agent}'s varde hooks are missing or out of date, so varde cannot follow its sessions: varde doctor says how to fix it")
    })
}

/// The agent a new conversation uses must be one varde knows, and its
/// command must run.
fn agent() -> Value {
    let name = crate::start::default_agent();
    let Some(adapter) = varde_agent::by_name(&name) else {
        return fail(
            "agent",
            format!("unknown agent {name:?}"),
            "set agent to \"claude\" or \"pi\" in the config (or $VARDE_AGENT)",
        );
    };
    // As `varde run` picks it.
    let command = crate::start::agent_command(&*adapter);
    let program = command[0].clone();
    let found = if program.contains('/') {
        Some(PathBuf::from(&program)).filter(|p| runnable(p))
    } else {
        which(&program)
    };
    match found {
        Some(p) => ok(
            "agent",
            format!(
                "{name} runs {}",
                std::iter::once(p.display().to_string())
                    .chain(command[1..].iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ),
        None if program.contains('/') && Path::new(&program).is_file() => fail(
            "agent",
            format!("{name} runs {program}, which is not executable"),
            &format!("chmod +x {program}"),
        ),
        None => fail(
            "agent",
            format!("{name} runs {program}, which is not installed or not on PATH"),
            &format!("install {name}, set agents.{name}.command in the config, or choose another agent"),
        ),
    }
}

fn pending(paths: &Paths) -> Value {
    let count_in = |pred: &dyn Fn(&str) -> bool| -> usize {
        std::fs::read_dir(&paths.home)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter(|e| pred(&e.file_name().to_string_lossy()))
                    // Bytes, not text: a torn write need not be UTF-8.
                    .map(|e| {
                        std::fs::read(e.path())
                            .map(|t| t.split(|b| *b == b'\n').filter(|l| !l.is_empty()).count())
                            .unwrap_or(0)
                    })
                    .sum()
            })
            .unwrap_or(0)
    };
    let waiting = count_in(&|n: &str| n.starts_with("pending.") && !n.starts_with("pending.failed"));
    let failed = count_in(&|n: &str| n == "pending.failed.jsonl");
    match (waiting, failed) {
        (0, 0) => ok("pending", "no events waiting".into()),
        (n, 0) => fail(
            "pending",
            format!("{n} events waiting"),
            "any varde command applies them; if they stay, something holds the store",
        ),
        (n, f) => fail(
            "pending",
            format!("{n} events waiting, {f} could not be applied"),
            &format!("see why in {}, then delete it", paths.home.join("pending.failed.jsonl").display()),
        ),
    }
}

/// Cairn updates: a failed one leaves its cairn as it was, and is tried
/// again a few times before it is given up on. One given up on is shown
/// for a week: nothing will retry it, so it is news only while recent.
fn jobs(conn: &rusqlite::Connection) -> Result<Value> {
    const RECENT_MS: i64 = 7 * 24 * 3600 * 1000;
    let now = now_ms();
    // ?1 is the attempts allowed, ?2 a week ago.
    let shown = "state = 'failed' AND (attempts < ?1 OR finished >= ?2)";
    let (failed, retrying, stuck, last, agent): (i64, i64, i64, Option<String>, Option<String>) = conn
        .query_row(
            &format!(
                "SELECT count(*) FILTER (WHERE state = 'failed' AND attempts >= ?1 AND finished >= ?2),
                    count(*) FILTER (WHERE state = 'failed' AND attempts < ?1),
                    count(*) FILTER (WHERE state IN ('queued', 'running') AND coalesce(beat, created) < ?3),
                    (SELECT error FROM job WHERE {shown} ORDER BY finished DESC LIMIT 1),
                    (SELECT agent FROM run WHERE id =
                       (SELECT run FROM job WHERE {shown} ORDER BY finished DESC LIMIT 1))
             FROM job"
            ),
            rusqlite::params![memory::MAX_ATTEMPTS, now - RECENT_MS, now - memory::STUCK_AFTER_MS],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
    let tasks: Vec<String> = conn
        .prepare(&format!(
            "SELECT DISTINCT r.task FROM (SELECT run, finished FROM job WHERE {shown}) j JOIN run r ON r.id = j.run
             WHERE r.task IS NOT NULL ORDER BY j.finished DESC LIMIT 5"
        ))?
        .query_map(rusqlite::params![memory::MAX_ATTEMPTS, now - RECENT_MS], |r| r.get::<_, String>(0))?
        .filter_map(|t| t.ok().map(|t| varde_core::task::short(&t)))
        .collect();
    // An update is made by its run's agent.
    let agent = agent.unwrap_or_else(crate::start::default_agent);
    let headless = format!(
        "one that keeps failing usually means the agent cannot run headless (try `{agent} -p hello`)"
    );
    let fix = if failed > 0 {
        format!(
            "{headless}; what one given up on would have kept is not in memory, and it is shown for a week"
        )
    } else {
        format!("a board or `varde list` retries them within minutes; {headless}")
    };
    Ok(match (failed, retrying, stuck) {
        (0, 0, 0) if !crate::config::get().memory.enabled => {
            ok("memory", "cairn updates are off (memory.enabled = false)".into())
        }
        (0, 0, 0) => ok("memory", "cairn updates ok".into()),
        _ => fail(
            "memory",
            format!(
                "{failed} cairn update(s) given up on this week, {retrying} to be retried, {stuck} stuck{}{}",
                if tasks.is_empty() { String::new() } else { format!(" (tasks {})", tasks.join(", ")) },
                last.map(|e| format!("; last error: {e}")).unwrap_or_default()
            ),
            &fix,
        ),
    })
}

/// Cairn bullets whose anchor no longer resolves in the code. Counted,
/// not failed: the next update of each cairn is told to fix or drop them.
fn stale(conn: &rusqlite::Connection) -> Result<Value> {
    let (bullets, tasks) = memory::stale_count(conn)?;
    Ok(ok(
        "stale",
        match bullets {
            0 => "no bullet names code that is gone".into(),
            n => format!("{n} bullet(s) in {tasks} cairn(s) name code that is gone, marked (stale?)"),
        },
    ))
}

/// Worktrees the store has as active whose directory is gone.
fn worktrees(conn: &rusqlite::Connection) -> Result<Value> {
    let gone: Vec<(String, String)> = conn
        .prepare("SELECT path, task FROM worktree WHERE state = 'active'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .filter(|(path, _)| !Path::new(path).exists())
        .collect();
    Ok(match gone.as_slice() {
        [] => ok("worktrees", "every active worktree is there".into()),
        _ => fail(
            "worktrees",
            format!(
                "gone: {}",
                gone.iter()
                    .map(|(p, t)| format!("{p} (task {})", varde_core::task::short(t)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "varde start <task> restores a worktree from its branch; varde drop <task> lets it go",
        ),
    })
}

/// With a task scheduled, something must run `varde tick`: the heartbeat,
/// installed and running.
/// Whether every profile can work in the background as it is set up.
fn profiles() -> Value {
    let c = crate::config::get();
    let warnings: Vec<String> = c.profiles().into_iter().filter_map(|(n, _)| c.profile_warning(&n)).collect();
    match warnings.first() {
        None => ok("profiles", format!("{} profiles; varde profiles lists them", c.profiles().len())),
        Some(_) => fail(
            "profiles",
            warnings.join("; "),
            "do as it says; a variable goes in your shell's profile, then varde schedule --install again so the heartbeat has it",
        ),
    }
}

fn schedules(paths: &Paths, conn: &rusqlite::Connection) -> Result<Value> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM schedule s JOIN task t ON t.id = s.task WHERE t.state = 'open'",
        [],
        |r| r.get(0),
    )?;
    let failed: Vec<String> = conn
        .prepare("SELECT task, error FROM schedule WHERE error IS NOT NULL")?
        .query_map([], |r| {
            Ok(format!("{}: {}", varde_core::task::short(&r.get::<_, String>(0)?), r.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    // A trail or loop set going waits on the heartbeat too, for what it
    // could not start at once.
    let going: i64 = conn.query_row(
        "SELECT count(*) FROM task WHERE coalesce(auto, 0) = 1 AND state = 'open'",
        [],
        |r| r.get(0),
    )?;
    let n = n + going;
    let ticked = crate::schedule::since_tick(paths);
    let missing = crate::schedule::heartbeat_missing();
    if !missing.is_empty() {
        let main = schedule_state(n, ticked, &failed);
        let gap = format!(
            "the heartbeat starts sessions without your shell's {}: they may reach another account or model",
            missing.join(", ")
        );
        let detail = match main["ok"].as_bool() {
            Some(true) => format!("{}; but {gap}", main["detail"].as_str().unwrap_or("")),
            _ => format!("{}; and {gap}", main["detail"].as_str().unwrap_or("")),
        };
        return Ok(fail("schedule", detail, "varde schedule --install, from the shell your agent works in"));
    }
    Ok(schedule_state(n, ticked, &failed))
}

fn schedule_state(n: i64, ticked: Option<Duration>, failed: &[String]) -> Value {
    if n == 0 {
        ok("schedule", "no task is scheduled".into())
    } else if crate::schedule::heartbeat().is_none() {
        fail(
            "schedule",
            format!(
                "{n} task(s) scheduled, but nothing runs varde tick: they start only while a board is open"
            ),
            "varde schedule --install",
        )
    } else if ticked.is_none_or(|d| d > Duration::from_secs(300)) {
        fail(
            "schedule",
            "the heartbeat is installed but varde tick has not run in the last 5 minutes".into(),
            "systemctl --user status varde-tick.timer, or on macOS launchctl print gui/$(id -u)/varde.tick",
        )
    } else if !failed.is_empty() {
        fail(
            "schedule",
            format!("failed to start: {}", failed.join("; ")),
            "fix what it says; it is tried again every 10 minutes",
        )
    } else {
        ok(
            "schedule",
            format!(
                "{n} task(s) scheduled or going; varde tick ran {}s ago",
                ticked.map_or(0, |d| d.as_secs())
            ),
        )
    }
}

/// Whether git would run a bare repository it comes upon: one a
/// session leaves in a checkout, committed or not, has git run what its
/// config names when you look there. `explicit` is git's own guard.
fn bare_repositories() -> Value {
    let set = std::process::Command::new("git")
        .args(["config", "safe.bareRepository"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if set == "explicit" {
        ok("git", "safe.bareRepository = explicit".into())
    } else {
        fail(
            "git",
            "git runs a bare repository it finds in a checkout, which a session can leave there".into(),
            "git config --global safe.bareRepository explicit (a bare repository of yours then needs --git-dir)",
        )
    }
}

/// Whether background sessions run in a sandbox here, and whether one
/// starts and keeps a write out.
fn sandbox(paths: &Paths) -> Value {
    use crate::sandbox;
    let fix = "set sandbox.on = false in the config to run without one, on what each agent is granted";
    match (sandbox::enabled(), sandbox::backend()) {
        (Err(e), _) => fail("sandbox", format!("{e:#}"), sandbox::install_hint()),
        (Ok(false), None) if cfg!(target_os = "macos") && !sandbox::turned_off() => fail(
            "sandbox",
            "sandbox-exec does not start here, so background sessions run unsandboxed or not at all".into(),
            "run varde outside another sandbox (an agent's own, say), or set sandbox.on = false to go without",
        ),
        (Ok(false), None) => ok(
            "sandbox",
            format!(
                "none here ({}): background sessions run on what varde grants their agent, \
                 and not at all on an agent that can keep to no grant",
                sandbox::install_hint()
            ),
        ),
        (Ok(false), Some(_)) => ok(
            "sandbox",
            "off (sandbox.on = false): background sessions run on what their agent is granted".into(),
        ),
        (Ok(true), b) => {
            let name = b.map_or("?", |b| b.name());
            let probe = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".varde-sandbox-probe"));
            let adapter = varde_agent::by_name(&crate::start::default_agent());
            // A directory of its own to start in, as a checkout is.
            let probe_dir = std::env::temp_dir().join(format!("varde-doctor-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&probe_dir);
            let _cleanup = Cleanup(probe_dir.clone());
            let run_as =
                |script: &str, agent: Option<&dyn varde_agent::Agent>| -> Result<std::process::Output> {
                    let p = sandbox::policy(None)?.ok_or_else(|| anyhow::anyhow!("no default sandbox"))?;
                    let (mut argv, env) = sandbox::wrap(&p, &probe_dir, agent, &paths.home, None)?;
                    argv.extend(["sh", "-c", script].map(String::from));
                    Ok(std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .env_clear()
                        .envs(env)
                        .current_dir(&probe_dir)
                        .stdin(std::process::Stdio::null())
                        .output()?)
                };
            let run = |script: &str| run_as(script, None);
            match run("true") {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let why = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    return fail("sandbox", format!("{name} does not start: {why}"), fix);
                }
                Err(e) => return fail("sandbox", format!("{name} does not start: {e:#}"), fix),
            }
            if let Some(p) = &probe {
                let kept_out = run(&format!("touch '{}'", p.display())).is_ok_and(|o| !o.status.success());
                if !kept_out || p.exists() {
                    let _ = std::fs::remove_file(p);
                    return fail(
                        "sandbox",
                        format!("{name} starts, but let a write to your home through"),
                        fix,
                    );
                }
            }
            // The agent itself, as a round would start it.
            if let Some(a) = &adapter {
                let program = crate::start::agent_command(&**a)[0].clone();
                match run_as(&format!("'{program}' --version"), Some(&**a)) {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => {
                        let why: String =
                            String::from_utf8_lossy(&o.stderr).trim().chars().take(300).collect();
                        return fail(
                            "sandbox",
                            format!("{name} starts, but {} does not in it: {why}", a.name()),
                            fix,
                        );
                    }
                    Err(e) => {
                        return fail("sandbox", format!("{} does not start in {name}: {e:#}", a.name()), fix)
                    }
                }
            }
            ok(
                "sandbox",
                format!(
                    "{name}: background sessions and checks run in it; varde sandbox shows what it allows"
                ),
            )
        }
    }
}

/// The program PATH runs as `name`: as the shell finds it, skipping a
/// file that is not executable.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| runnable(p))
}

fn runnable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where Claude Code keeps its settings and plugins.
fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
}

fn read_json(p: &Path) -> Value {
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Settings files whose hooks call varde, and installed plugins whose
/// hooks do and that are enabled. A plugin's copy stays in the cache
/// after it is disabled or uninstalled, so the copy alone says nothing.
fn claude_hooks() -> Vec<PathBuf> {
    let dir = claude_dir();
    let calls = |v: &Value| v.to_string().contains("varde hook");
    let settings: Vec<(PathBuf, Value)> = ["settings.json", "settings.local.json"]
        .iter()
        .map(|f| dir.join(f))
        .map(|p| {
            let v = read_json(&p);
            (p, v)
        })
        .collect();
    let mut found: Vec<PathBuf> =
        settings.iter().filter(|(_, v)| calls(&v["hooks"])).map(|(p, _)| p.clone()).collect();
    // `{"plugins": {"varde@varde": [{"scope": "user", "installPath": …}]}}`.
    // Disabling one sets it false in `enabledPlugins`, and the later
    // settings file wins; a project's own settings are not read.
    let enabled = |name: &str| {
        settings.iter().filter_map(|(_, v)| v["enabledPlugins"][name].as_bool()).next_back() != Some(false)
    };
    let installed = read_json(&dir.join("plugins/installed_plugins.json"));
    for (name, installs) in installed["plugins"].as_object().into_iter().flatten() {
        for install in installs.as_array().into_iter().flatten() {
            let hooks =
                Path::new(install["installPath"].as_str().unwrap_or_default()).join("hooks/hooks.json");
            if enabled(name) && calls(&read_json(&hooks)) {
                found.push(hooks);
            }
        }
    }
    found
}

/// Where pi keeps its settings and extensions: `$PI_CODING_AGENT_DIR`,
/// else `~/.pi/agent`.
fn pi_dir() -> PathBuf {
    match std::env::var("PI_CODING_AGENT_DIR").ok().filter(|d| !d.is_empty()) {
        Some(d) => tilde(&d),
        None => home().join(".pi/agent"),
    }
}

/// `~` and `~/…` as pi expands them; anything else as it is.
fn tilde(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}

/// pi's extension that forwards to `varde hook`: in its extensions
/// directory, or a file or directory its settings list under
/// `extensions` (relative to its directory). Project extensions
/// (`.pi/extensions` in a repository) are not looked for.
fn pi_extension() -> Option<PathBuf> {
    let dir = pi_dir();
    let listed = read_json(&dir.join("settings.json"))["extensions"].clone();
    let places = std::iter::once(dir.join("extensions")).chain(
        listed
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            // `!glob` and `-path` leave out; `builtin:` ones are pi's own.
            .filter(|e| !e.starts_with(['!', '-']) && !e.starts_with("builtin:"))
            .map(|e| dir.join(tilde(e.strip_prefix('+').unwrap_or(e)))),
    );
    let files = places.flat_map(|p| match std::fs::read_dir(&p) {
        Ok(it) => it.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => vec![p],
    });
    files
        .into_iter()
        .find(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("\"hook\"") && t.contains("varde")))
}

/// A directory removed when it goes out of scope.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
