//! Claude Code.

use std::io::Read;
use std::path::Path;

use serde_json::Value;
use varde_core::event::{Kind, Norm, Source};

use crate::{Agent, Answer, Launch};

pub struct Claude;

/// Thinking tokens one headless call may spend.
const HEADLESS_THINKING: &str = "4096";
/// A headless call's effort, whatever the user's settings say.
const HEADLESS_EFFORT: &str = "medium";

fn bin() -> String {
    std::env::var("VARDE_CLAUDE").unwrap_or_else(|_| "claude".into())
}

/// A fork copies its parent's history, message ids included, so every
/// session descended from one conversation starts with the same first
/// message id, near the top of its transcript.
const HEAD_BYTES: u64 = 16 * 1024;

fn head(transcript: &Path) -> Option<String> {
    // The cut can fall inside a character.
    let mut buf = Vec::new();
    std::fs::File::open(transcript).ok()?.take(HEAD_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// User-role lines that are the CLI talking, not the user: slash-command
/// echoes, their output, interrupt markers.
fn cli_noise(text: &str) -> bool {
    ["<local-command-", "<command-name>", "<command-message>", "[Request interrupted"]
        .iter()
        .any(|p| text.starts_with(p))
}

/// (role, text) for one transcript line, or `None` if it is not
/// conversation. Tolerant: transcript shapes change between versions.
fn line_text(line: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    // A message another session sent in (`varde say`, through the
    // session's inbox) is written as meta, but starts a turn as a prompt
    // does. Other meta lines are the CLI's own.
    let peer = v["origin"]["kind"] == "peer";
    if v.get("isMeta").and_then(Value::as_bool) == Some(true) && !peer {
        return None;
    }
    let role = v.get("type").and_then(Value::as_str)?;
    if role != "user" && role != "assistant" {
        return None;
    }
    let text = crate::texts(v.get("message")?.get("content")?)?.join("\n");
    let text = if peer { unwrap_peer(&text) } else { text.trim() };
    (!text.is_empty() && !cli_noise(text)).then(|| (role.to_string(), text.to_string()))
}

/// A peer message as it was sent, without what the CLI wraps it in: a
/// line saying another session sent it, and a paragraph on what a peer
/// may ask. Seen in 2.1.280; text it does not recognise is left as is.
fn unwrap_peer(text: &str) -> &str {
    let text = text.trim();
    let text = text.strip_prefix("Another Claude session sent a message:").unwrap_or(text);
    let text = text.split("\n\nThis came from another Claude session").next().unwrap_or(text);
    text.trim()
}

/// How the CLI was started; see `scripted`.
const ENTRYPOINT: &str = "CLAUDE_CODE_ENTRYPOINT";

/// The tool a Claude session asks the user a question with.
const QUESTION: &str = "AskUserQuestion";

/// Fields that can be large and are in the transcript anyway.
const DROP: &[&str] = &["tool_input", "tool_response", "last_assistant_message"];

impl Agent for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn normalize(&self, event: &str, p: &Value) -> Option<Norm> {
        let s = |k: &str| p.get(k).and_then(Value::as_str);
        let kind = match event {
            "SessionStart" => Kind::Started(match s("source") {
                Some("startup") => Source::Startup,
                Some("resume") => Source::Resume,
                Some("fork") => Source::Fork,
                Some("clear") => Source::Clear,
                Some("compact") => Source::Compact,
                _ => Source::Other,
            }),
            "UserPromptSubmit" => Kind::Prompt,
            // A turn ended on an API error is ended all the same.
            "Stop" | "StopFailure" => {
                // What the turn left running. Each sub-agent wakes the
                // session with a prompt of its own when it finishes.
                let running = |ty: &str| {
                    p.get("background_tasks").and_then(Value::as_array).map_or(0, |ts| {
                        ts.iter()
                            .filter(|t| {
                                t.get("type").and_then(Value::as_str) == Some(ty)
                                    && t.get("status").and_then(Value::as_str) == Some("running")
                            })
                            .count() as u32
                    })
                };
                Kind::TurnEnd { agents: running("subagent"), shells: running("shell") }
            }
            "PermissionRequest" => Kind::Blocked,
            // An approved tool that failed ran all the same.
            "PostToolUse" | "PostToolUseFailure" => Kind::ToolRan,
            // Only `idle_prompt` is subscribed to, but the matcher is the
            // user's to change; everything else is noise.
            "Notification" if s("notification_type") == Some("idle_prompt") => Kind::Idle,
            "SessionEnd" => Kind::Ended,
            "PreCompact" => Kind::Compacting,
            _ => return None,
        };
        crate::norm(p, kind, "transcript_path")
    }

    fn new_native_id(&self) -> Option<String> {
        // The CLI accepts only a version-4 UUID.
        Some(uuid::Uuid::new_v4().to_string())
    }

    fn program(&self) -> String {
        bin()
    }

    fn launch(&self, how: Launch) -> (Vec<String>, Vec<(String, String)>) {
        let mut argv = vec![bin()];
        let s = |x: &str| x.to_string();
        match how {
            Launch::New { id, name, prompt } => {
                argv.extend(id.into_iter().flat_map(|id| [s("--session-id"), s(id)]));
                argv.extend([s("-n"), s(name)]);
                // `--`: a prompt may begin with `-`. One beginning with `/`
                // would run as a slash command; with a space in front it is
                // read as text.
                argv.extend(prompt.into_iter().flat_map(|p| [s("--"), crate::as_text(p, &['/'])]));
            }
            Launch::Resume { id, prompt } => {
                argv.extend([s("--resume"), s(id)]);
                argv.extend(prompt.into_iter().flat_map(|p| [s("--"), crate::as_text(p, &['/'])]));
            }
            Launch::Fork { from, id } => {
                argv.extend([s("--resume"), s(from), s("--fork-session")]);
                argv.extend(id.into_iter().flat_map(|id| [s("--session-id"), s(id)]));
            }
        }
        // A window varde opened holds one task's session; the CLI's own
        // agent view would turn it into a dashboard that starts sessions
        // behind varde's back. The CLI keeps an entry point it inherits,
        // and a session varde launches is one someone sits at.
        let env = [("CLAUDE_CODE_DISABLE_AGENT_VIEW", "1"), (ENTRYPOINT, "cli")];
        (argv, env.map(|(k, v)| (s(k), s(v))).to_vec())
    }

    fn family_key(&self, transcript: &Path) -> Option<String> {
        head(transcript)?
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find_map(|v| v.get("uuid").and_then(Value::as_str).map(String::from))
    }

    fn in_family(&self, transcript: &Path, key: &str) -> bool {
        head(transcript).is_some_and(|h| h.contains(&format!("\"uuid\":\"{key}\"")))
    }

    fn digest(&self, transcript: &Path, from: u64, budget: usize) -> std::io::Result<(String, u64)> {
        crate::turns::digest(transcript, from, budget, line_text)
    }

    /// `{"type":"assistant","timestamp":…,"message":{"content":[{"type":
    /// "tool_use","name":"Read","input":{…}}]}}`.
    fn tool_calls(&self, transcript: &Path) -> std::io::Result<Vec<crate::ToolCall>> {
        crate::tools::read(transcript, "\"tool_use\"", &|v| {
            if v["type"] != "assistant" {
                return Vec::new();
            }
            let edits = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
            crate::calls(
                &v["message"]["content"],
                "tool_use",
                "input",
                &edits,
                crate::tools::when(&v["timestamp"]),
            )
        })
    }

    fn interrupted(&self, line: &str) -> bool {
        // Every line the session writes comes through here: rule most out
        // before parsing.
        const MARK: &str = "[Request interrupted by user";
        if !line.contains(MARK) {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
        if v.get("type").and_then(Value::as_str) != Some("user") {
            return false;
        }
        v.get("message")
            .and_then(|m| m.get("content"))
            .and_then(crate::texts)
            .is_some_and(|t| t.iter().any(|t| t.starts_with(MARK)))
    }

    /// `--safe-mode` leaves out `CLAUDE.md`, plugins, hooks, skills and
    /// MCP servers while keeping the user's sign-in, which `--bare` does
    /// not (it reads only `ANTHROPIC_API_KEY`); `--system-prompt` replaces
    /// the agent's own. Checked in the 2.1.280 help.
    ///
    /// The thinking budget is varde's, not the user's: settings chosen for
    /// interactive work (`effortLevel`, adaptive thinking off) reach the
    /// call despite `--safe-mode`, and through the hook's environment, and
    /// gave haiku 1 000 to 10 000 thinking tokens, up to 98 s, for a reply
    /// of about 200. Adaptive thinking is off so the budget holds on any
    /// model; with no thinking at all, haiku broke the format.
    fn headless(&self, model: Option<&str>, system: &str) -> (Vec<String>, Vec<(String, String)>) {
        let bin = bin();
        let argv = [
            bin.as_str(),
            "-p",
            "--model",
            model.unwrap_or("haiku"),
            "--system-prompt",
            system,
            "--safe-mode",
            "--tools",
            "",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--output-format",
            "json",
            "--effort",
            HEADLESS_EFFORT,
        ]
        .map(String::from)
        .to_vec();
        let env = [
            // Keep the call out of varde's own tracking.
            ("VARDE_INTERNAL", "1"),
            ("MAX_THINKING_TOKENS", HEADLESS_THINKING),
            ("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "1"),
            // The session's effort, inherited through the hook: removed.
            ("CLAUDE_EFFORT", ""),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .to_vec();
        (argv, env)
    }

    /// `--output-format json` prints one object: the reply in `result`,
    /// and `usage`, whose input is split by what the cache held. Plain
    /// text, from a wrapper or an older CLI, is the reply itself.
    fn headless_reply(&self, stdout: &str) -> Result<Answer, String> {
        // Whatever a wrapper printed before the object is not the reply.
        let object = |s: &str| serde_json::from_str::<Value>(s).ok().filter(Value::is_object);
        let trimmed = stdout.trim();
        let Some(v) = object(trimmed).or_else(|| object(trimmed.lines().last()?)) else {
            return Ok(Answer { text: stdout.to_string(), input_tokens: None, output_tokens: None });
        };
        let Some(text) = v.get("result").and_then(Value::as_str) else {
            return Err(format!("no result in the reply: {}", stdout.chars().take(200).collect::<String>()));
        };
        if v.get("is_error").and_then(Value::as_bool) == Some(true) {
            return Err(text.chars().take(200).collect());
        }
        let usage = v.get("usage");
        let n = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_i64);
        let input = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
            .iter()
            .filter_map(|k| n(k))
            .reduce(|a, b| a + b);
        Ok(Answer { text: text.to_string(), input_tokens: input, output_tokens: n("output_tokens") })
    }

    fn hook_reply(&self, event: &str, context: &str) -> Value {
        serde_json::json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": context}})
    }

    /// Claude Code sets `CLAUDE_CODE_ENTRYPOINT` for itself and every
    /// process it starts, hooks included: `cli` for a session in a
    /// terminal, `sdk-cli` for `claude -p` (even one started inside a
    /// terminal session, whose `cli` it overrides), and `sdk-ts` or
    /// `sdk-py` for a program using the Agent SDK. Checked in the 2.1.280
    /// binary.
    fn scripted(&self) -> bool {
        std::env::var(ENTRYPOINT).is_ok_and(|e| e.starts_with("sdk-"))
    }

    /// Esc before the first token of a reply cancels the turn with no
    /// hook and no transcript line. Claude does keep each session's state
    /// in `sessions/<pid>.json` under its config directory, `status`
    /// going `idle` at the cancel. The file is Claude's own and
    /// undocumented (seen in 2.1.280): anything unexpected says nothing.
    fn idle_since(&self, pid: i64, native_id: &str) -> Option<i64> {
        let v = registered(pid)?;
        (v["status"] == "idle" && v["sessionId"] == native_id)
            .then(|| v["statusUpdatedAt"].as_i64())
            .flatten()
    }

    /// A permission comes as `PermissionRequest`; a question as the
    /// `PreToolUse` of `AskUserQuestion`, which the plugin hooks alone, and
    /// which never counts as a permission too.
    fn asked(&self, event: &str, p: &Value) -> Option<crate::Asked> {
        use varde_core::steward::Kind;
        let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
        let tool = s("tool_name")?;
        let kind = match (event, tool == QUESTION) {
            ("PermissionRequest", false) => Kind::Permission,
            ("PreToolUse", true) => Kind::Question,
            _ => return None,
        };
        Some(crate::Asked {
            kind,
            session: s("session_id")?,
            tool,
            input: p.get("tool_input").cloned().unwrap_or(Value::Null),
            cwd: s("cwd")?,
        })
    }

    /// A permission's decision as `PermissionRequest` takes it; a
    /// question's answers in the tool's own input, which Claude shows as
    /// the user's.
    fn answer_reply(&self, asked: &crate::Asked, answer: &Value) -> Option<Value> {
        use varde_core::steward::Kind;
        Some(match asked.kind {
            Kind::Permission => {
                serde_json::json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": answer}})
            }
            Kind::Question => {
                let mut input = asked.input.clone();
                input["answers"] = answer.get("answers")?.clone();
                serde_json::json!({"hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "updatedInput": input,
                }})
            }
        })
    }

    fn question_tool(&self) -> Option<&'static str> {
        Some(QUESTION)
    }

    /// Claude's own inbox for its session: the socket its registry entry
    /// names, which takes a `user` message and starts a turn with it when
    /// idle, or queues it into the turn under way (`now`, urgent, cuts in
    /// at once). The auth line carries
    /// the key the session publishes beside its entry, when it does.
    /// Undocumented, seen in 2.1.280 (`peerProtocol` 1): anything
    /// unexpected is no way in.
    fn push(&self, pid: i64, native_id: &str, text: &str, urgent: bool) -> std::io::Result<bool> {
        use std::io::Write;
        let Some(v) = registered(pid) else { return Ok(false) };
        // The protocol this was written against, for the session it was meant for.
        let (Some(socket), true, true) =
            (v["messagingSocketPath"].as_str(), v["sessionId"] == native_id, v["peerProtocol"] == 1)
        else {
            return Ok(false);
        };
        let key = std::fs::read_dir(sessions_dir().unwrap_or_default())
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(&format!("{pid}.")) && n.ends_with(".key")
            })
            .and_then(|e| std::fs::read_to_string(e.path()).ok());
        let mut s = std::os::unix::net::UnixStream::connect(socket)?;
        let mut lines = String::new();
        if let Some(key) = key {
            lines.push_str(&serde_json::json!({"type": "auth", "token": key.trim()}).to_string());
            lines.push('\n');
        }
        let message = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": text},
            "session_id": native_id,
            "from": "varde",
            "priority": if urgent { "now" } else { "next" },
        });
        lines.push_str(&message.to_string());
        lines.push('\n');
        s.write_all(lines.as_bytes())?;
        Ok(true)
    }

    /// The native build runs as `claude`; the npm one is a node script,
    /// `node` until it names itself.
    fn process_names(&self) -> &'static [&'static str] {
        &["claude", "node"]
    }

    fn keep(&self, mut payload: Value) -> Value {
        if let Some(obj) = payload.as_object_mut() {
            for k in DROP {
                obj.remove(*k);
            }
        }
        payload
    }
}

/// Where Claude keeps its live sessions' entries.
fn sessions_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".claude")))?;
    Some(dir.join("sessions"))
}

/// The session process `pid` registered, as Claude keeps it in
/// `sessions/<pid>.json`.
fn registered(pid: i64) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(sessions_dir()?.join(format!("{pid}.json"))).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real payloads captured from Claude Code 2.1.280.
    fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude").join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn norm(event: &str, file: &str) -> Norm {
        Claude.normalize(event, &fixture(file)).unwrap()
    }

    #[test]
    fn a_headless_call_loads_nothing_of_the_users_and_reports_its_tokens() {
        let (argv, env) = Claude.headless(Some("sonnet"), "You keep a cairn.");
        let at = |flag: &str| argv.iter().position(|a| a == flag).map(|i| argv[i + 1].as_str());
        assert_eq!(at("--system-prompt"), Some("You keep a cairn."));
        assert_eq!(at("--tools"), Some(""));
        assert_eq!(at("--output-format"), Some("json"));
        assert!(argv.iter().any(|a| a == "--safe-mode"));
        // What it costs is varde's to set, not the user's settings'.
        assert_eq!(at("--effort"), Some(HEADLESS_EFFORT));
        let var = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(var("VARDE_INTERNAL"), Some("1"));
        assert_eq!(var("MAX_THINKING_TOKENS"), Some(HEADLESS_THINKING));
        assert_eq!(var("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING"), Some("1"));
        assert_eq!(var("CLAUDE_EFFORT"), Some(""), "removed");

        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"NO EDITS",
            "usage":{"input_tokens":12,"cache_creation_input_tokens":2000,"cache_read_input_tokens":300,"output_tokens":5}}"#;
        let a = Claude.headless_reply(out).unwrap();
        assert_eq!(a, Answer { text: "NO EDITS".into(), input_tokens: Some(2312), output_tokens: Some(5) });
        // Plain text, from a wrapper, is the reply.
        assert_eq!(Claude.headless_reply("NO EDITS\n").unwrap().input_tokens, None);
        let failed = r#"{"type":"result","is_error":true,"result":"Credit balance is too low"}"#;
        assert!(Claude.headless_reply(failed).unwrap_err().contains("Credit"));
    }

    #[test]
    fn an_interrupt_is_read_from_its_transcript_line() {
        // Esc mid-reply, then Esc on a permission dialog, then a prompt
        // that only quotes the marker, then the rejected tool's result.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude/transcript-interrupts.jsonl");
        let lines = std::fs::read_to_string(path).unwrap();
        let seen: Vec<bool> = lines.lines().map(|l| Claude.interrupted(l)).collect();
        assert_eq!(seen, [true, true, false, false]);
    }

    #[test]
    fn session_start_carries_its_source() {
        assert_eq!(norm("SessionStart", "session-start-startup.json").kind, Kind::Started(Source::Startup));
        assert_eq!(norm("SessionStart", "session-start-resume.json").kind, Kind::Started(Source::Resume));
        assert_eq!(norm("SessionStart", "session-start-fork.json").kind, Kind::Started(Source::Fork));
        assert_eq!(norm("SessionStart", "session-start-compact.json").kind, Kind::Started(Source::Compact));
        assert_eq!(norm("SessionStart", "session-start-clear.json").kind, Kind::Started(Source::Clear));
    }

    #[test]
    fn a_resume_keeps_the_id_and_a_fork_does_not() {
        let start = norm("SessionStart", "session-start-startup.json");
        assert_eq!(norm("SessionStart", "session-start-resume.json").native_id, start.native_id);
        assert_ne!(norm("SessionStart", "session-start-fork.json").native_id, start.native_id);
    }

    #[test]
    fn a_prompt_carries_its_text_and_directory() {
        let n = norm("UserPromptSubmit", "user-prompt-submit.json");
        assert_eq!(n.kind, Kind::Prompt);
        assert!(n.prompt.unwrap().starts_with("Run ls once"));
        assert!(n.cwd.is_some() && n.transcript.is_some());
    }

    #[test]
    fn the_rest_of_a_turn() {
        assert_eq!(norm("PostToolUse", "post-tool-use.json").kind, Kind::ToolRan);
        assert_eq!(norm("Stop", "stop.json").kind, Kind::TurnEnd { agents: 0, shells: 0 });
        assert_eq!(norm("SessionEnd", "session-end.json").kind, Kind::Ended);
    }

    #[test]
    fn a_failed_tool_or_turn_is_the_same_step_as_one_that_worked() {
        // Shaped as 2.1.280 builds them; no real capture yet.
        let p =
            serde_json::json!({"session_id": "s", "hook_event_name": "StopFailure", "error": "overloaded"});
        assert_eq!(Claude.normalize("StopFailure", &p).unwrap().kind, Kind::TurnEnd { agents: 0, shells: 0 });
        let p = serde_json::json!({"session_id": "s", "tool_name": "Bash", "error": "exit 1"});
        assert_eq!(Claude.normalize("PostToolUseFailure", &p).unwrap().kind, Kind::ToolRan);
    }

    #[test]
    fn a_turn_end_counts_what_it_left_running() {
        assert_eq!(norm("Stop", "stop-background.json").kind, Kind::TurnEnd { agents: 2, shells: 1 });
    }

    #[test]
    fn only_idle_notifications_count() {
        let p = |t: &str| serde_json::json!({"session_id": "s", "notification_type": t});
        assert_eq!(Claude.normalize("Notification", &p("idle_prompt")).unwrap().kind, Kind::Idle);
        assert!(Claude.normalize("Notification", &p("auth_success")).is_none());
    }

    #[test]
    fn untracked_and_malformed_events_are_ignored() {
        assert!(Claude.normalize("PreToolUse", &fixture("post-tool-use.json")).is_none());
        assert!(Claude.normalize("Stop", &serde_json::json!({"cwd": "/x"})).is_none());
        assert!(Claude.normalize("Stop", &serde_json::json!({"session_id": ""})).is_none());
        assert!(Claude.normalize("Stop", &serde_json::json!([1, 2])).is_none());
    }

    #[test]
    fn launches_name_the_session_before_it_exists() {
        std::env::remove_var("VARDE_CLAUDE");
        let (argv, env) = Claude.launch(Launch::New { id: Some("u1"), name: "Fix it", prompt: Some("go") });
        assert_eq!(argv, ["claude", "--session-id", "u1", "-n", "Fix it", "--", "go"]);
        // Read as text, not as a flag or a slash command: checked in the
        // 2.1.280 terminal UI.
        let prompt = |p| Claude.launch(Launch::New { id: None, name: "n", prompt: Some(p) }).0;
        assert_eq!(prompt("-v regression")[3..], ["--", "-v regression"]);
        assert_eq!(prompt("/cost is wrong")[3..], ["--", " /cost is wrong"]);
        assert_eq!(prompt("@alice please review")[3..], ["--", "@alice please review"]);
        assert!(env.contains(&("CLAUDE_CODE_DISABLE_AGENT_VIEW".into(), "1".into())));
        let (argv, _) = Claude.launch(Launch::Fork { from: "u1", id: Some("u2") });
        assert_eq!(argv, ["claude", "--resume", "u1", "--fork-session", "--session-id", "u2"]);
        let (argv, _) = Claude.launch(Launch::Resume { id: "u1", prompt: None });
        assert_eq!(argv, ["claude", "--resume", "u1"]);
    }

    #[test]
    fn context_is_handed_back_as_claude_reads_it() {
        let reply = Claude.hook_reply("SessionStart", "hello");
        assert_eq!(reply["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(reply["hookSpecificOutput"]["additionalContext"], "hello");
    }

    #[test]
    fn native_ids_are_v4_uuids() {
        let id = Claude.new_native_id().unwrap();
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
    }

    #[test]
    fn forks_are_recognised_by_the_history_they_copy() {
        let d = tempfile::tempdir().unwrap();
        // The shape of a real transcript: bookkeeping lines without ids first.
        let lines = |ids: &[&str]| {
            let mut s = String::from("{\"type\":\"mode\",\"sessionId\":\"x\"}\n");
            for id in ids {
                s.push_str(&format!("{{\"type\":\"user\",\"uuid\":\"{id}\",\"sessionId\":\"x\"}}\n"));
            }
            s
        };
        let parent = d.path().join("parent.jsonl");
        let fork = d.path().join("fork.jsonl");
        let other = d.path().join("other.jsonl");
        std::fs::write(&parent, lines(&["m1", "m2"])).unwrap();
        std::fs::write(&fork, lines(&["m1", "m2", "m3"])).unwrap();
        std::fs::write(&other, lines(&["z1"])).unwrap();
        let key = Claude.family_key(&fork).unwrap();
        assert_eq!(key, "m1");
        assert!(Claude.in_family(&parent, &key));
        assert!(!Claude.in_family(&other, &key));
        assert!(Claude.family_key(&d.path().join("missing.jsonl")).is_none());

        // The head's cut falling inside a character loses nothing before it.
        let mut long = lines(&["m1"]);
        long.push_str(&" ".repeat(HEAD_BYTES as usize - long.len() - 1));
        long.push_str("ø\n");
        std::fs::write(&fork, long).unwrap();
        assert_eq!(Claude.family_key(&fork).as_deref(), Some("m1"));
    }

    fn transcript_line(role: &str, content: Value) -> String {
        serde_json::json!({"type": role, "message": {"role": role, "content": content}}).to_string()
    }

    /// A digest of `transcript` from byte `from`, and where it stopped.
    fn digest_of(transcript: &str, from: u64, budget: usize) -> (String, u64) {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("t.jsonl");
        std::fs::write(&path, transcript).unwrap();
        Claude.digest(&path, from, budget).unwrap()
    }

    #[test]
    fn a_digest_pairs_each_prompt_with_its_report() {
        let lines = [
            transcript_line("user", "Fix the flaky test".into()),
            transcript_line("assistant", serde_json::json!([{"type": "text", "text": "Now the fixture:"}])),
            transcript_line("user", serde_json::json!([{"type": "tool_result", "content": "ok"}])),
            transcript_line(
                "assistant",
                serde_json::json!([{"type": "text", "text": "Root cause: shared fixture."}]),
            ),
            transcript_line("user", "<command-name>/model</command-name>".into()),
            serde_json::json!({"type": "user", "isMeta": true, "message": {"content": "skill text"}})
                .to_string(),
            transcript_line("user", "Now merge it".into()),
        ];
        let transcript = lines.join("\n") + "\n";
        let (text, end) = digest_of(&transcript, 0, 8000);
        assert_eq!(end, transcript.len() as u64);
        assert!(text.contains("Asked:\nFix the flaky test\n\nReported back:\nRoot cause: shared fixture."));
        assert!(
            !text.contains("Now the fixture:") && !text.contains("/model") && !text.contains("skill text")
        );
        assert!(text.contains("Asked:\nNow merge it\n\nReported back:\n(nothing yet"));
        assert!(text.contains("Turn 2 of 2"));

        // From the cursor on, only what is new.
        let (again, same_end) = digest_of(&transcript, end, 8000);
        assert!(again.is_empty());
        assert_eq!(same_end, end);
    }

    #[test]
    fn a_digest_stays_within_its_budget_however_long_the_session() {
        let mut lines = String::new();
        for i in 0..300 {
            lines += &(transcript_line("user", format!("question {i} {}", "x".repeat(1500)).into()) + "\n");
            lines +=
                &(transcript_line("assistant", format!("answer {i} {}", "y".repeat(3000)).into()) + "\n");
        }
        let (text, _) = digest_of(&lines, 0, 12_000);
        assert!(text.chars().count() < 16_000, "{} chars", text.chars().count());
        // The newest turns are the ones kept.
        assert!(text.contains("question 299") && !text.contains("question 0 "));
        assert!(text.starts_with("(") && text.contains("earlier turns left out"));
    }

    #[test]
    fn a_message_another_session_sent_starts_a_turn_of_its_own() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("t.jsonl");
        let peer = |text: &str| {
            json!({"type": "user", "isMeta": true, "origin": {"kind": "peer", "from": "varde"},
                   "message": {"role": "user", "content": format!(
                       "Another Claude session sent a message:\n{text}\n\nThis came from another Claude session \
                        — not typed by your user.")}})
        };
        let said = |text: &str| {
            json!({"type": "assistant", "message": {"role": "assistant",
                                       "content": [{"type": "text", "text": text}]}})
        };
        // The CLI's own meta lines stay out.
        let meta = json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": "<system-reminder>x"}});
        let lines = [peer("Check the branch."), said("All pass."), meta, peer("Say hello."), said("Hello!")];
        std::fs::write(&t, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let (digest, _) = Claude.digest(&t, 0, 4000).unwrap();
        assert!(digest.contains("Turn 1 of 2") && digest.contains("Turn 2 of 2"), "{digest}");
        assert!(digest.contains("Check the branch.") && digest.contains("Say hello."), "{digest}");
        assert!(
            !digest.contains("continued from before") && !digest.contains("Another Claude session"),
            "{digest}"
        );
        assert!(!digest.contains("system-reminder"), "{digest}");
    }

    #[test]
    fn a_long_pasted_prompt_keeps_its_turn() {
        let log = "x".repeat(30_000);
        let lines = [
            transcript_line("user", "fix X".into()),
            transcript_line("assistant", serde_json::json!([{"type": "text", "text": "R1"}])),
            transcript_line("user", format!("here is the log: {log}").into()),
            transcript_line(
                "user",
                serde_json::json!([{"type": "tool_result", "content": format!("big output {log}")}]),
            ),
            transcript_line("assistant", serde_json::json!([{"type": "text", "text": "R2"}])),
        ];
        let (text, _) = digest_of(&(lines.join("\n") + "\n"), 0, 8000);
        assert!(text.contains("Asked:\nfix X\n\nReported back:\nR1"), "{text}");
        assert!(text.contains("Asked:\nhere is the log: xxx"), "{text}");
        assert!(text.contains("Reported back:\nR2"), "{text}");
        assert!(!text.contains("big output"));
        assert!(text.chars().count() < 8000);
    }

    #[test]
    fn a_digest_stops_before_a_line_still_being_written() {
        let first = transcript_line("user", "One".into()) + "\n";
        let (_, end) = digest_of(&format!("{first}{{\"type\":\"assist"), 0, 8000);
        assert_eq!(end, first.len() as u64);
    }

    #[test]
    fn a_digest_starting_mid_turn_keeps_the_report() {
        let (text, _) =
            digest_of(&(transcript_line("assistant", "Done: it was DNS.".into()) + "\n"), 0, 8000);
        assert!(text.contains("(continued from before)") && text.contains("it was DNS"));
    }

    #[test]
    fn large_fields_are_not_kept() {
        let kept = Claude.keep(fixture("post-tool-use.json"));
        assert!(kept.get("tool_response").is_none() && kept.get("tool_input").is_none());
        assert!(kept.get("session_id").is_some());
    }
}
