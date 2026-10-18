//! What the integration tests share: a store of their own, and fakes in
//! place of `claude`, `pi`, `gh` and tmux, so no test touches the real
//! store or config, or runs a model.

#![allow(dead_code)] // each test crate uses only some of it

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// A fake `claude`: it records its arguments and fires the hooks the real
/// CLI would, so claims, liveness and resume are all exercised. Headless
/// (`-p`), it answers as the model would.
pub const AGENT: &str = r#"#!/bin/sh
# Headless: answer as the model would, keeping the prompt for inspection.
# A session its schedule started in the background is a session.
if [ "$1" = "-p" ] && [ -z "$VARDE_BACKGROUND" ]; then
  # Naming a task: kept apart from the cairn updates. It answers
  # $FAKE_TITLE, and without one it fails.
  case "$*" in *"You name tasks"*)
    printf '%s\n' "$*" >> "$FAKE_HOME/name-argv"
    cat > "$FAKE_HOME/name-prompt.$$"
    [ -n "$FAKE_TITLE" ] || exit 1
    printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}' "$FAKE_TITLE"
    exit 0 ;;
  esac
  printf '%s\n' "$*" >> "$FAKE_HOME/model-argv"
  env | grep -E '^(CLAUDE_EFFORT|MAX_THINKING_TOKENS)=' >> "$FAKE_HOME/model-env"
  cat > "$FAKE_HOME/model-prompt.$$"
  # The call fails, saying why on stderr.
  if [ -n "$FAKE_FAIL" ]; then printf 'starting up\n%s\n' "$FAKE_FAIL" >&2; exit 1; fi
  # The update is killed mid-way, as by a shutdown.
  if [ -n "$FAKE_KILL_JOB" ]; then kill -9 "$PPID"; exit 1; fi
  # A chatty model: far more output than a pipe holds, before the answer.
  if [ -n "$FAKE_BIG" ]; then head -c "$FAKE_BIG" /dev/zero | tr '\0' x; echo; fi
  # A reply as plain text, as a wrapper may print it; else as the CLI
  # prints it with --output-format json, tokens and all.
  if [ -n "$FAKE_REPLY" ]; then printf '%s\n' "$FAKE_REPLY"; else
    printf '%s' '{"type":"result","subtype":"success","is_error":false,"result":"STATUS: Cause found; fix pending.\nADD findings: The cache key omits the locale.\nADD open: Add the locale to the key.","usage":{"input_tokens":900,"cache_read_input_tokens":100,"output_tokens":40}}'
  fi
  exit 0
fi
# One line per launch; a prompt's newlines become "~".
printf '%s' "$*" | tr '\n' '~' >> "$FAKE_LOG"; echo >> "$FAKE_LOG"
echo "$PWD" >> "$FAKE_HOME/cwd.log"
sid=""; prev=""; fork=""
while [ $# -gt 0 ]; do
  case $1 in
    --session-id) sid=$2; shift 2 ;;
    --resume) prev=$2; shift 2 ;;
    --fork-session) fork=1; shift ;;
    -n) shift 2 ;;
    *) shift ;;
  esac
done
source=startup
if [ -n "$prev" ]; then
  if [ -n "$fork" ]; then source=fork; else source=resume; sid=$prev; fi
fi
t="$FAKE_HOME/$sid.jsonl"; : >> "$t"
if [ -n "$FAKE_TALK" ]; then
  printf '{"type":"user","message":{"role":"user","content":"Why is the page in English?"}}\n' >> "$t"
  printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"%s"}]}}\n' "$FAKE_TALK" >> "$t"
fi
[ -n "$FAKE_NOTE" ] && "$VARDE_BIN" note "$FAKE_NOTE" >/dev/null
[ -n "$FAKE_DEFER" ] && "$VARDE_BIN" new --from "$VARDE_RUN" "$FAKE_DEFER" --body "Asked to leave it for later." >/dev/null
p() { printf '{"session_id":"%s","cwd":"%s","transcript_path":"%s","source":"%s","prompt":"hello there"}' "$sid" "$PWD" "$t" "$source"; }
ms() { perl -MTime::HiRes=time -e 'printf "%d", time * 1000'; }
# The line Esc writes, stamped with its time as the CLI stamps every line.
esc() { printf '{"type":"user","timestamp":%s,"message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}\n' "$(ms)"; }
# What the session is handed, one hook reply per line.
p | "$VARDE_BIN" hook SessionStart >> "$FAKE_HOME/context.log"
p | "$VARDE_BIN" hook UserPromptSubmit >/dev/null
# The agent closes its own task mid-turn, as a tool call would.
if [ -n "$FAKE_DONE" ]; then
  "$VARDE_BIN" $FAKE_DONE >> "$FAKE_HOME/done.log" 2>&1
  sleep 0.5; echo "survived" >> "$FAKE_HOME/done.log"
fi
if [ -n "$FAKE_INTERRUPT" ]; then
  # Esc mid-turn: no hook fires; only the transcript says so.
  esc >> "$t"
  # And the next prompt, typed before `varde run` looks.
  [ -n "$FAKE_REPROMPT" ] && sleep 0.3 && p | "$VARDE_BIN" hook UserPromptSubmit >/dev/null
elif [ -n "$FAKE_CANCEL" ]; then
  # Esc before the reply's first token: no hook, nothing in the
  # transcript; only the CLI's own session file says it went idle.
  mkdir -p "$CLAUDE_CONFIG_DIR/sessions"
  printf '{"pid":%s,"sessionId":"%s","status":"idle","statusUpdatedAt":%s}' $$ "$sid" "$(ms)" > "$CLAUDE_CONFIG_DIR/sessions/$$.json"
else
  if [ -n "$FAKE_IDLE_FIRST" ]; then
    # A turn that ends normally, its session file going idle a moment
    # before its Stop hook is recorded.
    mkdir -p "$CLAUDE_CONFIG_DIR/sessions"
    printf '{"pid":%s,"sessionId":"%s","status":"idle","statusUpdatedAt":%s}' $$ "$sid" "$(ms)" > "$CLAUDE_CONFIG_DIR/sessions/$$.json"
    sleep 1.2
  fi
  p | "$VARDE_BIN" hook Stop >/dev/null
fi
if [ -n "$FAKE_SWITCH" ]; then
  # `/resume` or `/clear` in the session: its conversation ends and
  # another starts, in this same process, which is then interrupted.
  p | "$VARDE_BIN" hook SessionEnd >/dev/null
  sid=$FAKE_SWITCH; source=${FAKE_SWITCH_SOURCE:-resume}; t="$FAKE_HOME/$sid.jsonl"; : >> "$t"
  p | "$VARDE_BIN" hook SessionStart >/dev/null
  p | "$VARDE_BIN" hook UserPromptSubmit >/dev/null
  printf '{"type":"user","message":{"role":"user","content":"Now the other thing"}}\n' >> "$t"
  printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."}]}}\n' >> "$t"
  [ -n "$FAKE_SLEEP" ] && sleep 1.5
  esc >> "$t"
fi
if [ -n "$FAKE_SLEEP" ]; then
  # A session that ignores SIGTERM: only SIGKILL stops it.
  if [ -n "$FAKE_TRAP" ]; then trap '' TERM; sleep "$FAKE_SLEEP" & wait; exit 0; fi
  # A session that writes a file in its directory as it stops.
  if [ -n "$FAKE_LAST_WRITE" ]; then
    trap 'sleep 0.3; echo late > "$FAKE_LAST_WRITE"; exit 0' TERM; sleep "$FAKE_SLEEP" & wait; exit 0
  fi
  exec sleep "$FAKE_SLEEP"
fi
exit "${FAKE_EXIT:-0}"
"#;

/// What the shell or agent session a test is run from may set, which
/// would change what varde does: unset for every command a test runs.
const UNSET: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "VARDE_RUN",
    "VARDE_INTERNAL",
    "VARDE_AGENT",
    "VARDE_BACKGROUND",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDECODE",
    // What a git hook running the tests sets.
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
];

pub struct Env {
    pub dir: tempfile::TempDir,
    /// Where commands run.
    pub cwd: PathBuf,
}

impl Env {
    /// A store of its own, and no agent.
    pub fn new() -> Env {
        // Resolved, as varde records paths: macOS's temp dir is a link.
        let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let cwd = dir.path().to_path_buf();
        Env { dir, cwd }
    }

    /// `agent` as `claude`.
    pub fn with_agent(agent: &str) -> Env {
        let e = Env::new();
        e.script("claude", agent);
        e
    }

    /// `agent` as `claude`, and a repository `repo` with one commit.
    pub fn with_repo(agent: &str) -> Env {
        let e = Env::with_agent(agent);
        std::fs::create_dir(e.repo()).unwrap();
        git(&e.repo(), &["init", "-q", "-b", "main"]);
        git(&e.repo(), &["commit", "-q", "--allow-empty", "-m", "init"]);
        e
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn home(&self) -> PathBuf {
        self.path("state")
    }

    pub fn repo(&self) -> PathBuf {
        self.path("repo")
    }

    /// Write an executable `name` in the test's directory.
    pub fn script(&self, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The variables that sandbox varde, but for tmux: a test with a tmux
    /// server of its own needs to reach it.
    pub fn vars(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("VARDE_HOME", self.home()),
            ("VARDE_CONFIG", self.path("config.toml")),
            ("VARDE_CLAUDE", self.path("claude")),
            ("VARDE_PI", self.path("no-pi")),
            ("VARDE_GH", self.path("no-gh")),
            ("VARDE_BIN", PathBuf::from(env!("CARGO_BIN_EXE_varde"))),
            ("FAKE_HOME", self.dir.path().to_path_buf()),
            ("FAKE_LOG", self.path("argv.log")),
            ("CLAUDE_CONFIG_DIR", self.path("claude-config")),
            // git as on a machine of its own: the user's identity, signing
            // or status settings must not decide a test.
            ("GIT_CONFIG_GLOBAL", PathBuf::from(GITCONFIG)),
            ("GIT_CONFIG_NOSYSTEM", PathBuf::from("1")),
        ]
    }

    /// `command` as typed into a shell, sandboxed as `vars` sandboxes it.
    pub fn typed(&self, command: &str) -> String {
        let vars: Vec<String> = self.vars().iter().map(|(k, v)| format!("{k}='{}'", v.display())).collect();
        format!("env {} {command}", vars.join(" "))
    }

    /// `c`, sandboxed.
    pub fn env<'c>(&self, c: &'c mut Command) -> &'c mut Command {
        c.current_dir(&self.cwd)
            .envs(self.vars())
            .env("VARDE_TMUX", self.path("no-tmux"))
            .stdin(Stdio::null());
        for k in UNSET {
            c.env_remove(k);
        }
        c
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_varde"));
        self.env(c.args(args));
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> String {
        ok(args, self.run(args))
    }

    /// `ok`, with `stdin` as its input.
    pub fn ok_in(&self, args: &[&str], stdin: &str) -> String {
        ok(args, feed(&mut self.cmd(args), stdin))
    }

    /// A task in `repo`.
    pub fn new_task(&self, title: &str) -> String {
        self.ok(&["new", title, "--project", &self.repo().to_string_lossy(), "--body", "Details here."])
    }

    /// The fake agent's arguments, one line per launch.
    pub fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("argv.log")).unwrap_or_default().lines().map(String::from).collect()
    }

    pub fn show(&self, task: &str) -> Value {
        serde_json::from_str(&self.ok(&["show", task, "--json", "--no-git"])).unwrap()
    }

    pub fn board(&self) -> Value {
        serde_json::from_str(&self.ok(&["list", "--json"])).unwrap()
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.home().join("varde.db")).unwrap()
    }

    pub fn sql(&self, statements: &str) {
        self.db().execute_batch(statements).unwrap();
    }

    /// The first column of the first row of `query`, as text.
    pub fn query(&self, query: &str) -> String {
        self.db().query_row(query, [], |r| r.get_ref(0).map(sql_text)).unwrap()
    }
}

fn sql_text(v: rusqlite::types::ValueRef) -> String {
    use rusqlite::types::ValueRef::*;
    match v {
        Null => String::new(),
        Integer(i) => i.to_string(),
        Real(f) => f.to_string(),
        Text(t) | Blob(t) => String::from_utf8_lossy(t).into_owned(),
    }
}

fn ok(args: &[&str], out: Output) -> String {
    assert!(out.status.success(), "varde {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run `c` with `stdin` as its input.
pub fn feed(c: &mut Command, stdin: &str) -> Output {
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// The global git config of every test: an identity, and no signing.
const GITCONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/gitconfig");

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_CONFIG_GLOBAL", GITCONFIG)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
}

/// A task started in a worktree of its own, with a commit there.
pub fn worktree_with_a_commit(e: &Env, title: &str) -> (String, PathBuf) {
    let t = e.new_task(title);
    e.ok(&["attach", &t, "--worktree", "always"]);
    let path = PathBuf::from(e.show(&t)["worktree"]["path"].as_str().unwrap());
    commit(&path, "work.txt", "work");
    (t, path)
}

/// A recorded Claude hook payload.
pub fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../varde-agent/fixtures/claude").join(name);
    std::fs::read_to_string(p).unwrap()
}

pub fn group<'a>(board: &'a Value, key: &str) -> &'a Vec<Value> {
    board["groups"].as_array().unwrap().iter().find(|g| g["key"] == key).unwrap()["rows"].as_array().unwrap()
}

/// Every row on `board`, in every group.
pub fn all_rows(board: &Value) -> Vec<&Value> {
    board["groups"].as_array().unwrap().iter().flat_map(|g| g["rows"].as_array().unwrap()).collect()
}

/// `varde <args>` reading `text`, refused: what it said, and what it kept
/// of `text` in a file of its own, which is then removed.
pub fn refused_edit(e: &Env, args: &[&str], text: &str) -> (String, String) {
    let out = feed(&mut e.cmd(args), text);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{args:?}: {err}");
    let kept = err.split("kept in ").nth(1).unwrap_or_else(|| panic!("nothing kept: {err}")).trim();
    let typed = std::fs::read_to_string(kept).unwrap();
    let _ = std::fs::remove_file(kept);
    (err, typed)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// Make `task`'s schedule due `ago` ms ago.
pub fn due(e: &Env, task: &str, ago: i64) {
    e.db()
        .execute("UPDATE schedule SET next_due = ?2 WHERE task LIKE '%' || upper(?1)", (task, now_ms() - ago))
        .unwrap();
}

pub fn due_now(e: &Env, task: &str) {
    due(e, task, 1000);
}

pub fn runs(e: &Env, task: &str) -> Vec<Value> {
    e.show(task)["runs"].as_array().unwrap().clone()
}

/// Fire `task`'s schedule now, with `env` for the tick, and wait for that
/// run to end. Its id.
pub fn fire(e: &Env, task: &str, env: &[(&str, &str)]) -> String {
    let before = runs(e, task).len();
    due_now(e, task);
    poll(200, || {
        let out = e.cmd(&["tick"]).envs(env.iter().copied()).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let runs = runs(e, task);
        runs.len() > before && runs.last().is_some_and(|r| r["status"] == "exited")
    });
    runs(e, task).last().unwrap()["id"].as_str().unwrap().to_string()
}

/// A tmux server of the test's own, with one window `t` running `sh`, or
/// `None` where tmux is missing. Killed when dropped.
pub struct Tmux {
    pub sock: PathBuf,
}

impl Tmux {
    pub fn start(dir: &Path) -> Option<Tmux> {
        let t = Tmux { sock: dir.join("tmux.sock") };
        t.tmux(&["-f", "/dev/null", "new-session", "-d", "-s", "t", "-c", "/", "sh"])?;
        // Windows varde opens run this, not the user's shell.
        t.tmux(&["set-option", "-g", "default-shell", "/bin/sh"])?;
        Some(t)
    }

    pub fn tmux(&self, args: &[&str]) -> Option<String> {
        let mut c = Command::new("tmux");
        for k in UNSET {
            c.env_remove(k);
        }
        let out = c.arg("-S").arg(&self.sock).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The task a pane is tagged with.
    pub fn tag(&self, pane: &str) -> String {
        self.tmux(&["display", "-p", "-t", pane, "#{@varde_task}"]).unwrap()
    }

    /// `line` typed into `pane`, and Enter.
    pub fn type_in(&self, pane: &str, line: &str) {
        self.tmux(&["send-keys", "-t", pane, "-l", line]).unwrap();
        self.tmux(&["send-keys", "-t", pane, "Enter"]).unwrap();
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

/// Wait for `done` to come true, looking every 50 ms, for `tries` looks
/// but at least 10 s: on a loaded machine processes start late, and every
/// caller is waiting for something that must happen. Fails the test when
/// it never does, rather than leaving a later assert to fail obscurely.
pub fn poll(tries: usize, mut done: impl FnMut() -> bool) -> bool {
    let budget = Duration::from_millis(50 * tries as u64).max(Duration::from_secs(10));
    let until = Instant::now() + budget;
    while !done() {
        assert!(Instant::now() < until, "waited {budget:?} for something that never happened");
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}
