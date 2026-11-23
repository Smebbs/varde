//! A sandboxed session's varde (design §6.8). Inside a sandbox varde has
//! no store: it is hidden, so a session can neither read other tasks'
//! records nor forge its own. Each `varde` the session runs, its agent's
//! hooks included, is handed over a socket to the `varde run` outside
//! that started it, which runs it there, marked as from inside, if it is
//! one a session may run. What it refuses from inside (changing a check,
//! a loop, a sandbox) it refuses on a mark the session cannot set.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Set in a sandboxed session: where its varde hands commands to.
pub const BROKER: &str = "VARDE_BROKER";

/// What a session in a sandbox may have run, by its first word: reading,
/// recording what it learned, and making tasks of its own.
const ALLOWED: &[&str] = &[
    "hook",
    "note",
    "recall",
    "new",
    "show",
    "list",
    "recipe",
    "profiles",
    "trail",
    "check",
    "sandbox",
    "agent",
    "projects",
    "help",
    "--help",
    "-h",
    "--version",
    "-V",
];

/// The `varde run` side: a socket the session's varde reaches it on,
/// served until dropped.
pub struct Broker {
    pub socket: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Broker {
    /// Serve the run `run` of an agent, in the sandbox `sandbox`, from `cwd`.
    pub fn start(run: &str, sandbox: &str, cwd: &Path, agent: &dyn varde_agent::Agent) -> Result<Broker> {
        // Its own, though another `varde run` of the run raced it.
        let socket =
            std::env::temp_dir().join(format!("varde-{}-{}.sock", task_short(run), std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener =
            UnixListener::bind(&socket).with_context(|| format!("listening on {}", socket.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Context_ {
            run: run.to_string(),
            sandbox: sandbox.to_string(),
            cwd: cwd.to_path_buf(),
            hook_env: agent.hook_env(),
        });
        let stopped = stop.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                match conn {
                    Ok(conn) => {
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let _ = serve(conn, &ctx);
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        });
        Ok(Broker { socket, stop })
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Woken from its wait to see it is done.
        let _ = UnixStream::connect(&self.socket);
        let _ = std::fs::remove_file(&self.socket);
    }
}

struct Context_ {
    run: String,
    sandbox: String,
    cwd: PathBuf,
    hook_env: &'static [&'static str],
}

/// One command from inside: run here, as from inside, and its output back.
fn serve(conn: UnixStream, ctx: &Context_) -> Result<()> {
    conn.set_nonblocking(false)?;
    let mut line = String::new();
    BufReader::new(conn.try_clone()?.take(16 << 20)).read_line(&mut line)?;
    let req: Value = serde_json::from_str(&line)?;
    let args: Vec<String> =
        req["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect();
    let reply = match allowed(&args) {
        true => {
            // From the run's own directory, whatever it asks: what it made
            // there (a repository of its own) must not become a project,
            // and git run outside must not run in it.
            let mut args = args;
            // A recipe read from a file in its checkout, and no other: the
            // file is read outside, and what it says is printed back.
            if args.first().map(String::as_str) == Some("recipe") {
                let root = ctx.cwd.canonicalize().unwrap_or(ctx.cwd.clone());
                let outside = |a: &String| {
                    let file = ctx.cwd.join(a).canonicalize();
                    let named =
                        a.starts_with('/') || a.starts_with('~') || a.contains("..") || a.ends_with(".toml");
                    match file {
                        Ok(f) => !f.starts_with(&root),
                        Err(_) => named,
                    }
                };
                if args.iter().skip(2).filter(|a| !a.starts_with('-')).any(outside) {
                    return send(conn, refused(&args));
                }
            }
            // Of the store, its own project's.
            if matches!(args.first().map(String::as_str), Some("list" | "recall")) {
                args.extend(["--project".to_string(), ctx.cwd.display().to_string()]);
            }
            let mut cmd = Command::new(crate::exe());
            cmd.args(&args)
                .current_dir(&ctx.cwd)
                .env_remove(BROKER)
                .env(crate::sandbox::FENCED, &ctx.sandbox)
                .env("VARDE_RUN", &ctx.run)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Only what says who a hook runs for: nothing else from inside
            // may steer what runs outside (a library to preload, an agent).
            let passed = [crate::run::BACKGROUND];
            for k in passed.iter().chain(ctx.hook_env.iter()) {
                cmd.env_remove(k);
                if let Some(v) = req["env"][*k].as_str() {
                    cmd.env(k, v);
                }
            }
            let mut child = cmd.spawn()?;
            let input = req["stdin"].as_str().unwrap_or("").to_string();
            let mut stdin = child.stdin.take().context("its stdin")?;
            std::thread::spawn(move || {
                let _ = stdin.write_all(input.as_bytes());
            });
            let out = child.wait_with_output()?;
            json!({
                "code": out.status.code().unwrap_or(1),
                "stdout": String::from_utf8_lossy(&out.stdout),
                "stderr": String::from_utf8_lossy(&out.stderr),
            })
        }
        false => refused(&args),
    };
    send(conn, reply)
}

fn send(mut conn: UnixStream, reply: Value) -> Result<()> {
    conn.write_all(format!("{reply}\n").as_bytes())?;
    Ok(())
}

/// Whether a session in a sandbox may have `args` run. Of the recipes,
/// only reading them: editing one writes yours.
fn allowed(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("recipe") => {
            matches!(args.get(1).map(String::as_str), Some("list" | "show" | "check" | "guide"))
        }
        // Of the store, its own project's: no reaching across.
        Some("list" | "recall" | "show" | "new" | "trail")
            if args.iter().any(|a| a == "--all" || a.starts_with("--project")) =>
        {
            false
        }
        Some("projects") => false,
        Some(first) => ALLOWED.contains(&first),
        None => false,
    }
}

/// A run's id, short, for a socket's name, which has little room.
fn task_short(run: &str) -> String {
    run.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<String>().to_ascii_lowercase()
}

/// What a session in a sandbox is told of a command it may not run.
fn refused(args: &[String]) -> Value {
    let what: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    json!({
        "code": 1,
        "stdout": "",
        "stderr": format!("varde: `varde {}` is not for a session in a sandbox: it is the user's to do\n", what.join(" ")),
    })
}

/// The session's side: hand `args` (the command line, without the
/// program) to the varde outside, and print what it printed. A hook that
/// cannot reach it still answers, as a hook must.
pub fn forward(socket: &str, args: &[String], hook_env: &[String]) -> i32 {
    let hook = args.first().map(String::as_str) == Some("hook");
    // Its input, when it reads any: a hook's payload, or `-` for stdin.
    let stdin = if hook || args.iter().any(|a| a == "-") {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    } else {
        String::new()
    };
    let env: serde_json::Map<String, Value> = [crate::run::BACKGROUND]
        .iter()
        .map(|k| k.to_string())
        .chain(hook_env.iter().cloned())
        .filter_map(|k| std::env::var(&k).ok().map(|v| (k, Value::String(v))))
        .collect();
    let req = json!({
        "args": args,
        "cwd": std::env::current_dir().ok().map(|d| d.display().to_string()),
        "env": env,
        "stdin": stdin,
    });
    let reply = (|| -> Result<Value> {
        let mut conn = UnixStream::connect(socket)?;
        conn.write_all(format!("{req}\n").as_bytes())?;
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    })();
    match reply {
        Ok(r) => {
            print!("{}", r["stdout"].as_str().unwrap_or(""));
            eprint!("{}", r["stderr"].as_str().unwrap_or(""));
            r["code"].as_i64().unwrap_or(1) as i32
        }
        Err(_) if hook => {
            println!("{{}}");
            0
        }
        Err(e) => {
            eprintln!("varde: the varde outside this sandbox does not answer: {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_session_may_read_record_and_make_tasks_and_nothing_else() {
        let a = |s: &str| super::allowed(&s.split_whitespace().map(String::from).collect::<Vec<_>>());
        assert!(a("hook Stop") && a("note --run r text") && a("recipe show bugfix") && a("new x"));
        assert!(!a("recipe edit bugfix") && !a("loop t") && !a("set t --sandbox off") && !a("run r"));
        assert!(!a("say t hi") && !a("schedule --install") && !a("--popup hook") && !a(""));
        assert!(!a("list --all") && !a("recall x --all") && !a("new x --project=/home"));
    }
}
