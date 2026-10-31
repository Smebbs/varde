//! Agent adapters. Everything specific to one agent CLI — its hook
//! names, payload fields, flags and file layout — lives behind `Agent`,
//! so the core never names an agent.

use std::path::Path;

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
