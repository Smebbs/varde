//! pi (earendil-works/pi). pi has no shell hooks: the extension in
//! `cli/pi/varde.ts` forwards its session events to `varde hook --agent
//! pi <event>`, with a payload of its own making (`session_id`,
//! `session_file`, `cwd`, and `reason`, `prompt` or `aborted`; at
//! `session_start`, also `created`, `parent_session` and `last_message`).
//! Events, session files and flags are as of pi 1.1.0.

use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;
use varde_core::event::{Kind, Norm, Source};

use crate::{Agent, Launch};

pub struct Pi;

fn bin() -> String {
    std::env::var("VARDE_PI").unwrap_or_else(|_| "pi".into())
}

/// A session file's first line: `{"type":"session", "id", "cwd",
/// "parentSession"?}`.
fn header(transcript: &Path) -> Option<Value> {
    let mut line = String::new();
    BufReader::new(std::fs::File::open(transcript).ok()?).read_line(&mut line).ok()?;
    let v: Value = serde_json::from_str(&line).ok()?;
    (v.get("type").and_then(Value::as_str) == Some("session")).then_some(v)
}

/// (role, text) for a session line that is conversation. Tool results,
/// system prompts and extension messages (varde's guidance among them)
/// are not.
fn line_text(line: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = v.get("message")?;
    let role = message.get("role").and_then(Value::as_str)?;
    if role != "user" && role != "assistant" {
        return None;
    }
    // A reply the user stopped was not reported back.
    if message.get("stopReason").and_then(Value::as_str) == Some("aborted") {
        return None;
    }
    let text = crate::texts(message.get("content")?)?.join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| (role.to_string(), text.to_string()))
}

impl Agent for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn normalize(&self, event: &str, p: &Value) -> Option<Norm> {
        let s = |k: &str| p.get(k).and_then(Value::as_str);
        let kind = match (event, s("reason")) {
            // A reload swaps extensions inside the same session.
            ("session_start" | "session_shutdown", Some("reload")) => return None,
            // Whatever session pi opens as it starts is a `startup`, a
            // `--session` or `--fork` one too. A fork copies its parent's
            // conversation, all older than the fork's own header; a
            // session that already had a conversation of its own is
            // resumed.
            ("session_start", Some("startup")) => match (s("last_message"), s("parent_session")) {
                (None, _) => Kind::Started(Source::Startup),
                (Some(last), Some(_)) if s("created").is_some_and(|c| last < c) => {
                    Kind::Started(Source::Fork)
                }
                (Some(_), _) => Kind::Started(Source::Resume),
            },
            ("session_start", Some("resume")) => Kind::Started(Source::Resume),
            ("session_start", Some("fork")) => Kind::Started(Source::Fork),
            // `/new` in a running pi: a new session, as `/clear` is in Claude.
            ("session_start", Some("new")) => Kind::Started(Source::Clear),
            ("session_start", _) => Kind::Started(Source::Other),
            ("before_agent_start", _) => Kind::Prompt,
            // Not `agent_end`: pi may still retry, compact or run queued
            // work after it.
            ("agent_settled", _) if p.get("aborted").and_then(Value::as_bool) == Some(true) => {
                Kind::Interrupted
            }
            ("agent_settled", _) => Kind::TurnEnd { agents: 0, shells: 0 },
            // pi asks nothing itself; an extension's dialog (a permission
            // gate, say) is what blocks a session.
            ("ui_prompt_start", _) => Kind::Blocked,
            ("ui_prompt_end", _) => Kind::Answered,
            ("session_before_compact", _) => Kind::Compacting,
            ("session_compact", _) => Kind::Started(Source::Compact),
            ("session_shutdown", _) => Kind::Ended,
            _ => return None,
        };
        let native_id = s("session_id").filter(|id| !id.is_empty())?.to_string();
        Some(Norm {
            native_id,
            kind,
            cwd: s("cwd").map(Into::into),
            transcript: s("session_file").map(Into::into),
            prompt: s("prompt").map(Into::into),
        })
    }

    fn new_native_id(&self) -> Option<String> {
        // pi takes any id of letters, digits, `.`, `_` and `-`.
        Some(uuid::Uuid::new_v4().to_string())
    }

    fn launch(&self, how: Launch) -> (Vec<String>, Vec<(String, String)>) {
        let mut argv = vec![bin()];
        let s = |x: &str| x.to_string();
        match how {
            Launch::New { id, name, prompt } => {
                argv.extend(id.into_iter().flat_map(|id| [s("--session-id"), s(id)]));
                argv.extend([s("-n"), s(name)]);
                // `--`: a prompt may begin with `-`. After it, one beginning
                // with `@` names a file to include, and one beginning with
                // `/` runs a prompt template or a command; with a space in
                // front it is read as text.
                argv.extend(prompt.into_iter().flat_map(|p| [s("--"), crate::as_text(p, &['@', '/'])]));
            }
            // By exact id: `--session-id` would make a new session if the
            // id were gone.
            Launch::Resume { id } => argv.extend([s("--session"), s(id)]),
            Launch::Fork { from, id } => {
                argv.extend([s("--fork"), s(from)]);
                argv.extend(id.into_iter().flat_map(|id| [s("--session-id"), s(id)]));
            }
        }
        (argv, Vec::new())
    }

    /// A fork's session file names the file it was forked from.
    fn family_key(&self, transcript: &Path) -> Option<String> {
        header(transcript)?.get("parentSession").and_then(Value::as_str).map(String::from)
    }

    fn in_family(&self, transcript: &Path, key: &str) -> bool {
        transcript == Path::new(key)
    }

    fn digest(&self, transcript: &Path, from: u64, budget: usize) -> std::io::Result<(String, u64)> {
        crate::turns::digest(transcript, from, budget, line_text)
    }

    /// `{"type":"message","timestamp":…,"message":{"role":"assistant",
    /// "content":[{"type":"toolCall","name":"read","arguments":{…}}]}}`.
    fn tool_calls(&self, transcript: &Path) -> std::io::Result<Vec<crate::ToolCall>> {
        crate::tools::read(transcript, "\"toolCall\"", &|v| {
            let m = &v["message"];
            if v["type"] != "message" || m["role"] != "assistant" {
                return Vec::new();
            }
            let ts = crate::tools::when(&v["timestamp"]).or_else(|| crate::tools::when(&m["timestamp"]));
            crate::calls(&m["content"], "toolCall", "arguments", &["edit", "write"], ts)
        })
    }

    fn interrupted(&self, line: &str) -> bool {
        // Every line the session writes comes through here: rule most out
        // before parsing.
        if !line.contains("\"aborted\"") {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
        let m = &v["message"];
        v["type"] == "message" && m["role"] == "assistant" && m["stopReason"] == "aborted"
    }

    fn headless(&self, model: Option<&str>, system: &str) -> (Vec<String>, Vec<(String, String)>) {
        // The prompt comes on stdin, which pi reads as the first message.
        // No tools, no saved session, and nothing of the user's loaded.
        // Without a model, the user's default.
        let argv = [
            "-p",
            "--system-prompt",
            system,
            "--no-tools",
            "--no-session",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
        ];
        let model = model.into_iter().flat_map(|m| ["--model", m]);
        let argv = std::iter::once(bin()).chain(argv.into_iter().chain(model).map(String::from)).collect();
        (argv, vec![("VARDE_INTERNAL".to_string(), "1".to_string())])
    }

    fn hook_reply(&self, _event: &str, context: &str) -> Value {
        // The extension hands it to the session with its next prompt.
        serde_json::json!({ "context": context })
    }

    fn keep(&self, payload: Value) -> Value {
        payload
    }

    /// A node script, or a single binary built with bun; the extension
    /// runs the hook directly, with no shell between.
    fn process_names(&self) -> &'static [&'static str] {
        &["pi", "node", "bun"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(event: &str, extra: Value) -> Option<Norm> {
        let mut p = json!({"session_id": "s1", "session_file": "/p/s1.jsonl", "cwd": "/repo"});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        Pi.normalize(event, &p)
    }

    #[test]
    fn session_events_map_to_varde_kinds() {
        let kind = |e: &str, x: Value| ev(e, x).map(|n| n.kind);
        assert_eq!(kind("session_start", json!({"reason": "startup"})), Some(Kind::Started(Source::Startup)));
        assert_eq!(kind("session_start", json!({"reason": "resume"})), Some(Kind::Started(Source::Resume)));
        assert_eq!(kind("session_start", json!({"reason": "fork"})), Some(Kind::Started(Source::Fork)));
        assert_eq!(kind("session_start", json!({"reason": "new"})), Some(Kind::Started(Source::Clear)));
        assert_eq!(kind("session_start", json!({"reason": "reload"})), None);
        assert_eq!(kind("session_shutdown", json!({"reason": "reload"})), None);
        assert_eq!(kind("session_shutdown", json!({"reason": "quit"})), Some(Kind::Ended));
        assert_eq!(kind("agent_settled", json!({})), Some(Kind::TurnEnd { agents: 0, shells: 0 }));
        assert_eq!(
            kind("agent_settled", json!({"aborted": false})),
            Some(Kind::TurnEnd { agents: 0, shells: 0 })
        );
        assert_eq!(kind("agent_settled", json!({"aborted": true})), Some(Kind::Interrupted));
        assert_eq!(kind("ui_prompt_start", json!({})), Some(Kind::Blocked));
        assert_eq!(kind("ui_prompt_end", json!({})), Some(Kind::Answered));
        assert_eq!(kind("session_compact", json!({})), Some(Kind::Started(Source::Compact)));
        assert_eq!(kind("agent_end", json!({})), None);
    }

    /// What pi 1.1.0 sends at startup for `pi`, `pi --session X` and `pi
    /// --fork X`: always `startup`.
    #[test]
    fn a_session_opened_at_startup_is_told_by_what_it_holds() {
        let kind = |x: Value| ev("session_start", x).map(|n| n.kind);
        let at = |extra: Value| {
            let mut p = json!({"reason": "startup", "created": "2026-10-10T22:09:00.208Z"});
            p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            kind(p)
        };
        let parent = "/p/2026-10-10T22-08-28-521Z_a.jsonl";
        assert_eq!(at(json!({})), Some(Kind::Started(Source::Startup)));
        assert_eq!(
            at(json!({"last_message": "2026-10-10T22:08:42.434Z"})),
            Some(Kind::Started(Source::Resume))
        );
        assert_eq!(
            at(json!({"last_message": "2026-10-10T22:08:42.434Z", "parent_session": parent})),
            Some(Kind::Started(Source::Fork))
        );
        // A fork that has talked since is resumed.
        assert_eq!(
            at(json!({"last_message": "2026-10-10T22:09:30.000Z", "parent_session": parent})),
            Some(Kind::Started(Source::Resume))
        );
        // An extension from before these fields: as it was.
        assert_eq!(kind(json!({"reason": "startup"})), Some(Kind::Started(Source::Startup)));
    }

    #[test]
    fn a_prompt_carries_its_text_directory_and_session_file() {
        let n = ev("before_agent_start", json!({"prompt": "Fix the build"})).unwrap();
        assert_eq!(n.native_id, "s1");
        assert_eq!(n.prompt.as_deref(), Some("Fix the build"));
        assert_eq!(n.cwd.as_deref(), Some("/repo"));
        assert_eq!(n.transcript.as_deref(), Some("/p/s1.jsonl"));
        assert!(Pi.normalize("agent_settled", &json!({"session_id": ""})).is_none());
    }

    #[test]
    fn launches() {
        std::env::remove_var("VARDE_PI");
        let (argv, _) =
            Pi.launch(Launch::New { id: Some("u1"), name: "Fix it", prompt: Some("-v is broken") });
        assert_eq!(argv, ["pi", "--session-id", "u1", "-n", "Fix it", "--", "-v is broken"]);
        // Checked against pi 1.1.0: `@alice` failed as a missing file, and
        // `/review` expanded the user's prompt template.
        let prompt = |p| Pi.launch(Launch::New { id: None, name: "n", prompt: Some(p) }).0;
        assert_eq!(prompt("@alice please review")[3..], ["--", " @alice please review"]);
        assert_eq!(prompt("/review the login")[3..], ["--", " /review the login"]);
        let (argv, _) = Pi.launch(Launch::Resume { id: "u1" });
        assert_eq!(argv, ["pi", "--session", "u1"]);
        let (argv, _) = Pi.launch(Launch::Fork { from: "u1", id: Some("u2") });
        assert_eq!(argv, ["pi", "--fork", "u1", "--session-id", "u2"]);
    }

    #[test]
    fn context_goes_back_as_the_extension_reads_it() {
        assert_eq!(Pi.hook_reply("session_start", "hi"), json!({"context": "hi"}));
    }

    /// A session file in the shape pi 1.1.0 writes.
    fn session(d: &Path, name: &str, parent: Option<&str>, lines: &[Value]) -> std::path::PathBuf {
        let mut h = json!({"type": "session", "version": 3, "id": name, "cwd": "/repo"});
        if let Some(p) = parent {
            h["parentSession"] = json!(p);
        }
        let path = d.join(format!("{name}.jsonl"));
        let text: String = std::iter::once(&h).chain(lines).map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn msg(role: &str, content: Value) -> Value {
        json!({"type": "message", "id": "e1", "parentId": null, "message": {"role": role, "content": content}})
    }

    #[test]
    fn a_fork_names_the_session_it_came_from() {
        let d = tempfile::tempdir().unwrap();
        let parent = session(d.path(), "a", None, &[]);
        let fork = session(d.path(), "b", Some(&parent.to_string_lossy()), &[]);
        assert_eq!(Pi.family_key(&parent), None);
        let key = Pi.family_key(&fork).unwrap();
        assert!(Pi.in_family(&parent, &key));
        assert!(!Pi.in_family(&fork, &key));
    }

    #[test]
    fn a_digest_reads_prompts_and_replies_and_skips_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let t = session(
            d.path(),
            "a",
            None,
            &[
                msg("system", json!("You are an expert coding assistant")),
                json!({"type": "custom_message", "customType": "varde", "content": "varde tracks this session"}),
                msg("user", json!("Why is the page in English?")),
                msg(
                    "assistant",
                    json!([{"type": "thinking", "thinking": "hmm"}, {"type": "toolCall", "name": "read"}]),
                ),
                msg("toolResult", json!([{"type": "text", "text": "file contents"}])),
                msg("assistant", json!([{"type": "text", "text": "The cache key omits the locale."}])),
            ],
        );
        let (digest, end) = Pi.digest(&t, 0, 4000).unwrap();
        assert_eq!(end, std::fs::metadata(&t).unwrap().len());
        assert!(digest.contains("Asked:\nWhy is the page in English?"));
        assert!(digest.contains("Reported back:\nThe cache key omits the locale."));
        for absent in ["expert coding", "varde tracks", "file contents", "hmm"] {
            assert!(!digest.contains(absent), "{absent} in {digest}");
        }
    }

    #[test]
    fn a_reply_the_user_stopped_is_not_reported_back() {
        let d = tempfile::tempdir().unwrap();
        let mut stopped = msg("assistant", json!([{"type": "text", "text": "thinking very slowly"}]));
        stopped["message"]["stopReason"] = json!("aborted");
        let t = session(d.path(), "a", None, &[msg("user", json!("please be slow")), stopped]);
        let (digest, _) = Pi.digest(&t, 0, 4000).unwrap();
        assert!(digest.contains("Asked:\nplease be slow"), "{digest}");
        assert!(!digest.contains("thinking very slowly"), "{digest}");
    }

    #[test]
    fn an_aborted_reply_is_an_interrupt() {
        let aborted = json!({"type": "message", "message": {"role": "assistant", "content": [], "stopReason": "aborted"}});
        let done =
            json!({"type": "message", "message": {"role": "assistant", "content": [], "stopReason": "stop"}});
        let said = msg("user", json!("\"aborted\""));
        assert!(Pi.interrupted(&aborted.to_string()));
        assert!(!Pi.interrupted(&done.to_string()));
        assert!(!Pi.interrupted(&said.to_string()));
    }
}
