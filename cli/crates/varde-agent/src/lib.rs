//! Agent adapters. Everything specific to one agent CLI — its hook
//! names, payload fields, flags and file layout — lives behind `Agent`,
//! so the core never names an agent.

use std::path::{Path, PathBuf};

use serde_json::Value;
use varde_core::event::{Kind, Norm};

mod claude;
mod pi;
mod tools;
mod turns;

pub use claude::Claude;
pub use pi::Pi;
pub use tools::{iso_ms, ToolCall};

/// How a run begins.
#[derive(Debug, Clone, Copy)]
pub enum Launch<'a> {
    /// `id` is `None` for an agent that picks its own.
    New {
        id: Option<&'a str>,
        name: &'a str,
        prompt: Option<&'a str>,
    },
    /// `prompt`, when given, is the turn the conversation carries on
    /// with (`varde say`).
    Resume {
        id: &'a str,
        prompt: Option<&'a str>,
    },
    Fork {
        from: &'a str,
        id: Option<&'a str>,
    },
}

/// What a session asks the user that the steward may answer for them
/// (§6.6): a permission, or a question.
#[derive(Debug, Clone)]
pub struct Asked {
    pub kind: varde_core::steward::Kind,
    /// The session's own id, as its hooks give it.
    pub session: String,
    pub tool: String,
    pub input: Value,
    /// The session's directory, whose worktree bounds what may be handed over.
    pub cwd: String,
}

pub trait Agent {
    /// The name stored on every run this agent produces.
    fn name(&self) -> &'static str;

    /// Translate one hook call. `None` means the event is not tracked.
    fn normalize(&self, event: &str, payload: &Value) -> Option<Norm>;

    /// A fresh native session id, chosen before launch so the run is
    /// known before its first event. `None` for an agent that picks its
    /// own: the run then takes the id of the first event that claims it.
    fn new_native_id(&self) -> Option<String>;

    /// The program that runs the agent: its own name, unless
    /// `$VARDE_<AGENT>` names another.
    fn program(&self) -> String;

    /// The command line and extra environment that start a run.
    fn launch(&self, how: Launch) -> (Vec<String>, Vec<(String, String)>);

    /// `argv`, a launch's, made to run with nobody at it, in the
    /// background: to the end of its reply, then exit, its conversation
    /// kept to resume. Claude's and pi's print mode.
    fn background(&self, mut argv: Vec<String>) -> Vec<String> {
        argv.insert(1, "-p".into());
        argv
    }

    /// `argv`, a background launch's, kept to `grant`: what it may do
    /// with nobody there to ask. `Err` says why this agent cannot keep to
    /// it, and the session does not start.
    fn unattended(&self, argv: Vec<String>, grant: &Grant) -> Result<Vec<String>, String>;

    /// Where a session of the agent in `dir` writes as it works (its
    /// transcript, its login refreshed), which a sandbox must leave it.
    fn writes(&self, dir: &Path) -> Vec<PathBuf>;

    /// What else under its directory every session shares and writes:
    /// a sandbox gives a session its own, empty, where it can, so it reads
    /// and leaves nothing of another's. `true` for what the agent also
    /// runs (a shell script it sources): read-only where it cannot.
    fn private(&self) -> Vec<(PathBuf, bool)> {
        Vec::new()
    }

    /// What the agent reads to reach its model that a sandbox hides, as
    /// the environment says: a cloud login, and whether it writes there.
    fn reads(&self) -> Vec<(PathBuf, bool)> {
        Vec::new()
    }

    /// The variables the agent reaches its model with, by name or prefix:
    /// what the heartbeat carries, and what a sandbox keeps of what it
    /// would drop as a login (keys among them).
    fn env(&self) -> Vec<String>;

    /// The variables its hooks are told who they run for by, which a
    /// sandboxed session's varde passes on to the varde outside.
    fn hook_env(&self) -> &'static [&'static str] {
        &[]
    }

    /// The model it runs for one of the profiles varde ships (`quick`,
    /// `build`, `deep`, `review`): `None` for its own default.
    fn tier(&self, _profile: &str) -> Option<&'static str> {
        None
    }

    /// What is wrong with running on `model`, as the environment says:
    /// an alias a cloud provider resolves to a model it does not serve.
    fn model_warning(&self, _model: &str) -> Option<String> {
        None
    }

    /// `argv`, a launch's, on `model` at `effort`, as a profile asks:
    /// Claude's `--model` and `--effort`, pi's `--model` and `--thinking`.
    fn tuned(&self, mut argv: Vec<String>, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
        let mut flags = vec![];
        if let Some(m) = model {
            flags.extend(["--model".to_string(), m.to_string()]);
        }
        if let Some(e) = effort {
            flags.extend([self.effort_flag().to_string(), e.to_string()]);
        }
        if !argv.is_empty() {
            argv.splice(1..1, flags);
        }
        argv
    }

    /// The flag that sets how hard the agent thinks.
    fn effort_flag(&self) -> &'static str {
        "--effort"
    }

    /// What identifies the conversation a transcript belongs to: a key
    /// every fork of it copies. `None` if the transcript cannot say.
    fn family_key(&self, transcript: &Path) -> Option<String>;

    /// Whether `transcript` belongs to the conversation with `key`.
    fn in_family(&self, transcript: &Path, key: &str) -> bool;

    /// What happened in a transcript after byte `from`, as turns of what
    /// was asked and what was reported back, within about `budget`
    /// characters. Returns the digest (empty when there was no
    /// conversation) and the position it read up to.
    fn digest(&self, transcript: &Path, from: u64, budget: usize) -> std::io::Result<(String, u64)>;

    /// A transcript line as `varde follow` shows it: what the session
    /// said, the tools it called and what it was asked; nothing for a
    /// line that is none of those.
    fn follow(&self, line: &str) -> Vec<String> {
        let _ = line;
        Vec::new()
    }

    /// Whether a transcript line records the user interrupting a turn.
    /// Agents send no event for that, so `varde run` watches for it.
    fn interrupted(&self, line: &str) -> bool;

    /// When a transcript line was written, in unix milliseconds, if it
    /// says: an interrupt is recorded as of then, not when it was seen.
    fn line_time(&self, line: &str) -> Option<i64> {
        tools::when(&serde_json::from_str::<Value>(line).ok()?["timestamp"])
    }

    /// Hand `text` to the live session `native_id` in process `pid` as a
    /// message to it now: it starts a turn if idle, and joins the turn if
    /// working; `urgent`, it cuts into that turn where the agent allows.
    /// `Ok(false)` when this agent offers no way in, or the session is not
    /// where its process says; varde then waits for the session's next
    /// hook that can carry it (`varde say`).
    fn push(&self, _pid: i64, _native_id: &str, _text: &str, _urgent: bool) -> std::io::Result<bool> {
        Ok(false)
    }

    /// What hook `event` asks the user, when the steward may answer it in
    /// the hook: `None` for one it may not, or an agent whose hooks cannot
    /// carry an answer.
    fn asked(&self, _event: &str, _payload: &Value) -> Option<Asked> {
        None
    }

    /// The hook's reply carrying `answer` (as `varde answer` keeps it) to
    /// what `asked` was.
    fn answer_reply(&self, _asked: &Asked, _answer: &Value) -> Option<Value> {
        None
    }

    /// The tool a session asks the user a question with, when the steward
    /// can answer it from a hook.
    fn question_tool(&self) -> Option<&'static str> {
        None
    }

    /// When the session `native_id` in process `pid` went idle, if the
    /// agent says so outside its hooks and transcript: an interrupt that
    /// leaves no trace in either. `None` when it cannot say.
    fn idle_since(&self, _pid: i64, _native_id: &str) -> Option<i64> {
        None
    }

    /// Every tool call in a transcript, for `varde eval`: none for an
    /// agent whose transcripts cannot say.
    fn tool_calls(&self, _transcript: &Path) -> std::io::Result<Vec<ToolCall>> {
        Ok(Vec::new())
    }

    /// The command line and environment for a one-shot, tool-less call
    /// that reads its prompt from stdin, on `model` if given, with `system`
    /// as its whole system prompt and nothing of the user's loaded: no
    /// settings, context files, hooks, tools or saved session. A variable
    /// given an empty value is removed from the call's environment.
    fn headless(&self, model: Option<&str>, system: &str) -> (Vec<String>, Vec<(String, String)>);

    /// What a headless call printed: its reply, and the tokens it cost
    /// when the agent says.
    fn headless_reply(&self, stdout: &str) -> Result<Answer, String> {
        Ok(Answer { text: stdout.to_string(), input_tokens: None, output_tokens: None })
    }

    /// What a hook prints to hand the session `context`. The agent reads
    /// it, so its shape is the agent's.
    fn hook_reply(&self, event: &str, context: &str) -> Value;

    /// The payload as it should be kept: without the parts that are
    /// large and already recorded in the agent's own transcript.
    fn keep(&self, payload: Value) -> Value;

    /// The command names the agent's own process goes by, so a hook can
    /// tell it from whatever runs between them: a shell, `timeout`, a
    /// version manager's shim.
    fn process_names(&self) -> &'static [&'static str];

    /// Whether the hook running now comes from a one-shot session run by
    /// a script or a program, which nobody sits at or comes back to, as
    /// the hook's environment tells. Such a session is not tracked.
    fn scripted(&self) -> bool {
        false
    }
}

/// What a session with nobody at it may do without asking (design
/// §6.8). varde decides it; each adapter says it in its agent's terms.
#[derive(Debug, Clone, Default)]
pub struct Grant {
    /// It runs inside varde's sandbox, which bounds what it can reach:
    /// it may use every tool, and asks nothing.
    pub fenced: bool,
    /// The user turned the sandbox off for it: it runs as they let it.
    pub trusted: bool,
    /// Outside one, the commands it may run, besides changing files in its
    /// directory and `git add` and `git commit`: the check's.
    pub commands: Vec<String>,
    /// Commands it must never run, sandbox or not: patterns of the whole
    /// command line, `*` standing for any text.
    pub deny: Vec<String>,
}

/// A transcript, opened to read: a regular file, never through a link a
/// session in a sandbox put in its place (to another's transcript).
pub fn open_transcript(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::other("not a file"));
    }
    Ok(f)
}

/// `$HOME`, joined with `rel`.
pub(crate) fn home(rel: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rel)
}

/// A headless call's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// Everything the model read, cached or not.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

/// A hook's payload as an event of `kind`, its transcript under the key
/// `transcript`. `None` without a session id.
fn norm(p: &Value, kind: Kind, transcript: &str) -> Option<Norm> {
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    Some(Norm {
        native_id: s("session_id").filter(|id| !id.is_empty())?.to_string(),
        kind,
        cwd: s("cwd").map(Into::into),
        transcript: s(transcript).map(Into::into),
        prompt: s("prompt").map(Into::into),
    })
}

/// The text of a message's `content`: a string, or its text blocks.
fn texts(content: &Value) -> Option<Vec<&str>> {
    match content {
        Value::String(s) => Some(vec![s.as_str()]),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect(),
        ),
        _ => None,
    }
}

/// The tool calls among a message's content blocks of type `ty`, their
/// input under `input`; one named in `edits` changed a file.
fn calls(blocks: &Value, ty: &str, input: &str, edits: &[&str], ts: Option<i64>) -> Vec<ToolCall> {
    blocks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == ty)
        .map(|b| {
            let name = b["name"].as_str().unwrap_or_default().to_string();
            ToolCall { ts, edit: edits.contains(&name.as_str()), input: b[input].to_string(), name }
        })
        .collect()
}

/// `prompt` as the agent reads it as text: with a space in front when it
/// begins with one of `marks`, which the agent reads as something else.
fn as_text(prompt: &str, marks: &[char]) -> String {
    if prompt.starts_with(marks) {
        format!(" {prompt}")
    } else {
        prompt.to_string()
    }
}

/// Every agent varde knows.
pub fn all() -> Vec<Box<dyn Agent>> {
    ["claude", "pi"].iter().filter_map(|n| by_name(n)).collect()
}

pub fn by_name(name: &str) -> Option<Box<dyn Agent>> {
    match name {
        "claude" => Some(Box::new(Claude)),
        "pi" => Some(Box::new(Pi)),
        _ => None,
    }
}

/// One message's `content` as `varde follow` shows it: its text, each
/// tool call (`tool` is how the agent tags one, `input` where its
/// arguments are) as `→ Name what`, and for `user` what it asked.
pub(crate) fn shown(role: &str, content: &Value, tool: &str, input: &str) -> Vec<String> {
    let clip = |s: &str, n: usize| -> String {
        let s = s.trim();
        if s.chars().count() <= n {
            s.to_string()
        } else {
            s.chars().take(n - 1).chain(['…']).collect()
        }
    };
    let mut out = Vec::new();
    let blocks: Vec<Value> = match content {
        Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
        Value::Array(a) => a.clone(),
        _ => return out,
    };
    for b in &blocks {
        match b["type"].as_str() {
            Some("text") => {
                let t = b["text"].as_str().unwrap_or("").trim();
                // What the CLI writes as the user, of its own: not asked.
                if t.is_empty() || (role == "user" && t.starts_with('<')) {
                    continue;
                }
                if role == "user" {
                    out.push(format!("» {}", clip(t, 300)));
                } else {
                    out.extend(t.lines().map(|l| format!("  {l}")));
                }
            }
            Some(t) if t == tool => {
                let args = &b[input];
                let what = ["command", "file_path", "path", "pattern", "description", "url", "query"]
                    .iter()
                    .find_map(|k| args[*k].as_str())
                    .unwrap_or("");
                out.push(format!("→ {} {}", b["name"].as_str().unwrap_or("?"), clip(what, 120)));
            }
            _ => {}
        }
    }
    out
}
