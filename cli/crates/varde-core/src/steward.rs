//! The steward (§6.6): the one task whose agent manages varde with you,
//! the tasks you hand it, and what their sessions ask it.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::store::Store;

/// The steward's task, when it is on.
pub fn task(conn: &Connection) -> Result<Option<String>> {
    Ok(conn.prepare_cached("SELECT task FROM steward")?.query_row([], |r| r.get(0)).optional()?)
}

/// Turn the steward on with `task` as its task. One at a time.
pub fn enable(store: &Store, task: &str, now: i64) -> Result<()> {
    if let Some(had) = self::task(&store.conn)? {
        bail!("the steward is already on: task {}", crate::task::short(&had));
    }
    store.conn.execute("INSERT INTO steward (task, since) VALUES (?1, ?2)", params![task, now])?;
    Ok(())
}

/// Turn it off: its task stays as an ordinary one, and every hand-over
/// ends, so no session waits on a steward that is gone.
pub fn disable(store: &Store, now: i64) -> Result<Option<String>> {
    let had = task(&store.conn)?;
    store.conn.execute("DELETE FROM steward", [])?;
    store.conn.execute("UPDATE trust SET ended = ?1 WHERE ended IS NULL", [now])?;
    Ok(had)
}

/// A task handed to the steward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trust {
    pub id: i64,
    pub task: String,
    pub brief: Option<String>,
    pub until: i64,
    /// Rules the hook answers by itself.
    pub allow: Vec<String>,
}

/// Hand `task` to the steward until `until`, replacing any hand-over
/// still live.
pub fn trust(
    store: &Store,
    task: &str,
    brief: Option<&str>,
    rules: &[String],
    until: i64,
    now: i64,
) -> Result<()> {
    for r in rules {
        rule(r)?;
    }
    if self::task(&store.conn)?.is_none() {
        bail!("the steward is off: varde steward on");
    }
    if self::task(&store.conn)?.as_deref() == Some(task) {
        bail!("the steward cannot be handed its own task");
    }
    let tx = store.conn.unchecked_transaction()?;
    tx.execute("UPDATE trust SET ended = ?2 WHERE task = ?1 AND ended IS NULL", params![task, now])?;
    tx.execute(
        "INSERT INTO trust (task, brief, created, until, allow) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            task,
            brief,
            now,
            until,
            (!rules.is_empty()).then(|| serde_json::to_string(rules)).transpose()?
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Take `task` back. Says whether it was handed over.
pub fn untrust(store: &Store, task: &str, now: i64) -> Result<bool> {
    let n = store.conn.execute(
        "UPDATE trust SET ended = ?2 WHERE task = ?1 AND ended IS NULL AND until > ?2",
        params![task, now],
    )?;
    Ok(n > 0)
}

/// The live hand-over of `task`, if any: one indexed read, cheap enough
/// for every hook.
pub fn trusted(conn: &Connection, task: &str, now: i64) -> Result<Option<Trust>> {
    Ok(conn
        .prepare_cached(
            "SELECT id, task, brief, until, allow FROM trust
             WHERE task = ?1 AND ended IS NULL AND until > ?2
               AND EXISTS (SELECT 1 FROM steward)
             ORDER BY created DESC LIMIT 1",
        )?
        .query_row(params![task, now], trust_row)
        .optional()?)
}

fn trust_row(r: &rusqlite::Row) -> rusqlite::Result<Trust> {
    let allow: Option<String> = r.get(4)?;
    Ok(Trust {
        id: r.get(0)?,
        task: r.get(1)?,
        brief: r.get(2)?,
        until: r.get(3)?,
        allow: allow.and_then(|a| serde_json::from_str(&a).ok()).unwrap_or_default(),
    })
}

/// Add `rules` to `task`'s live hand-over. Says whether it had one.
pub fn allow(store: &Store, task: &str, rules: &[String], now: i64) -> Result<bool> {
    for r in rules {
        rule(r)?;
    }
    let Some(t) = trusted(&store.conn, task, now)? else { return Ok(false) };
    let mut all = t.allow;
    all.extend(rules.iter().filter(|r| !all.contains(r)).cloned().collect::<Vec<_>>());
    store
        .conn
        .execute("UPDATE trust SET allow = ?2 WHERE id = ?1", params![t.id, serde_json::to_string(&all)?])?;
    Ok(true)
}

/// A rule as typed: a tool, and what it is for, which `*` stands in for
/// any of: `Bash(cargo test*)`, `Edit(src/*)`, `WebFetch`.
fn rule(text: &str) -> Result<(&str, Option<&str>)> {
    let text = text.trim();
    let (tool, pattern) = match text.split_once('(') {
        Some((tool, rest)) => {
            let Some(p) = rest.strip_suffix(')') else {
                bail!("'{text}' is not a rule like Bash(cargo test*)")
            };
            (tool, Some(p))
        }
        None => (text, None),
    };
    if tool.is_empty() || !tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("'{text}' is not a rule like Bash(cargo test*)");
    }
    Ok((tool, pattern))
}

/// Whether `rules` allow `tool` with `input` in the worktree at `root`.
/// A Bash command needs every command of it allowed, so one rule never
/// lets through what is chained after it. A path is matched as it
/// resolves, whole and relative to `root`.
pub fn allowed(rules: &[String], tool: &str, input: &serde_json::Value, root: &str) -> bool {
    let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
    let ours: Vec<Option<&str>> =
        rules.iter().filter_map(|r| rule(r).ok()).filter(|(t, _)| *t == tool).map(|(_, p)| p).collect();
    let fits = |text: &str| ours.iter().any(|p| p.is_none_or(|p| glob(p, text)));
    match tool {
        "Bash" => {
            let parts: Vec<&str> =
                s("command").split(['&', '|', ';', '\n']).map(str::trim).filter(|p| !p.is_empty()).collect();
            // Substitutions and redirections are past what a rule can say.
            !parts.is_empty() && !s("command").contains(['`', '$', '>', '<']) && parts.iter().all(|p| fits(p))
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            // Matched as it resolves, never as written: `src/../x` is `x`.
            let path = [s("file_path"), s("notebook_path")].into_iter().find(|p| !p.is_empty()).unwrap_or("");
            let full = crate::project::normalize(&std::path::Path::new(root).join(expand(path)));
            let Ok(relative) = full.strip_prefix(root) else { return false };
            fits(&full.to_string_lossy()) || fits(&relative.to_string_lossy())
        }
        _ => ours.iter().any(Option::is_none) || ours.iter().flatten().any(|p| glob(p, &input.to_string())),
    }
}

/// `pattern` against all of `text`, `*` standing in for any run of
/// characters.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return text == pattern;
    }
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for p in &parts[1..parts.len() - 1] {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    true
}

/// Every live hand-over.
pub fn trusts(conn: &Connection, now: i64) -> Result<Vec<Trust>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, task, brief, max(until), allow FROM trust
         WHERE ended IS NULL AND until > ?1 AND EXISTS (SELECT 1 FROM steward)
         GROUP BY task ORDER BY until",
    )?;
    let rows = stmt.query_map([now], trust_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether `run` is yet to be told of hand-over `trust`; it counts as
/// told from here on.
pub fn tell(conn: &Connection, trust: i64, run: &str) -> Result<bool> {
    let n = conn
        .prepare_cached("INSERT OR IGNORE INTO trust_told (trust, run) VALUES (?1, ?2)")?
        .execute(params![trust, run])?;
    Ok(n > 0)
}

/// What a delegated session asks: a permission or a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Question,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
        }
    }
}

/// An ask, as `varde watch` and `varde asks` print it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ask {
    pub id: String,
    pub task: String,
    pub run: String,
    pub kind: String,
    pub tool: String,
    pub input: serde_json::Value,
    pub created: i64,
}

/// Record what a delegated session asks; returns its id.
pub fn ask(
    conn: &Connection,
    task: &str,
    run: &str,
    kind: Kind,
    tool: &str,
    input: &str,
    now: i64,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO ask (id, task, run, kind, tool, input, created) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![id, task, run, kind.name(), tool, input, now],
    )?;
    Ok(id)
}

/// Ask `id` once settled: `Some` with its answer, which is `None` when
/// it was passed to the user or given up.
pub fn settled(conn: &Connection, id: &str) -> Result<Option<Option<String>>> {
    Ok(conn
        .prepare_cached("SELECT answer FROM ask WHERE id = ?1 AND settled IS NOT NULL")?
        .query_row([id], |r| r.get::<_, Option<String>>(0))
        .optional()?)
}

/// Settle ask `id` with `answer` (JSON), or with none when `by` is
/// `gone`. Only the first settles it: says whether this one did.
pub fn settle(
    conn: &Connection,
    id: &str,
    answer: Option<&str>,
    why: Option<&str>,
    by: &str,
    now: i64,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE ask SET answer = ?2, why = ?3, by = ?4, settled = ?5 WHERE id = ?1 AND settled IS NULL",
        params![id, answer, why, by, now],
    )?;
    Ok(n > 0)
}

/// Settle asks whose hook can no longer be waiting: older than `max` ms,
/// or whose session has gone on (the user answered at the keyboard, the
/// turn moved, the session ended). A hook that died leaves its ask open.
pub fn expire(conn: &Connection, now: i64, max: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE ask SET by = 'gone', settled = ?1 WHERE settled IS NULL AND (created < ?1 - ?2
           OR EXISTS (SELECT 1 FROM event e WHERE e.run = ask.run AND e.ts > ask.created
             AND e.kind IN ('unblocked','prompt','turn_end','delegated','interrupted','ended')))",
        params![now, max],
    )?)
}

/// Asks no one has answered yet, oldest first, from `after` (exclusive)
/// on.
pub fn open_asks(conn: &Connection, after: i64) -> Result<Vec<Ask>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, task, run, kind, tool, input, created FROM ask
         WHERE settled IS NULL AND created > ?1 ORDER BY created, id",
    )?;
    let rows = stmt.query_map([after], |r| {
        let input: String = r.get(5)?;
        Ok(Ask {
            id: r.get(0)?,
            task: r.get(1)?,
            run: r.get(2)?,
            kind: r.get(3)?,
            tool: r.get(4)?,
            input: serde_json::from_str(&input).unwrap_or(serde_json::Value::Null),
            created: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The ask `reference` names: its id, or its last six characters.
pub fn resolve_ask(conn: &Connection, reference: &str) -> Result<Ask> {
    let r = reference.trim().to_ascii_uppercase();
    let found: Vec<String> = conn
        .prepare("SELECT id FROM ask WHERE id = ?1 OR (length(?1) >= 4 AND id LIKE '%' || ?1)")?
        .query_map([&r], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let id = match found.as_slice() {
        [one] => one,
        [] => bail!("no ask '{reference}'"),
        _ => bail!("'{reference}' names more than one ask: use more of its id"),
    };
    let mut asks = open_asks(conn, i64::MIN)?;
    match asks.iter().position(|a| &a.id == id) {
        Some(i) => Ok(asks.swap_remove(i)),
        None => bail!("ask '{reference}' is already settled"),
    }
}

/// Why an ask may never go to the steward, whatever its brief: the
/// steps you keep (§6.6). `root` is the session's worktree (its git
/// checkout's top), which writes and deletes must stay under. Decided
/// here, before an ask is made, so nothing a session writes can talk the
/// steward into one. It errs towards you: a shell command is read as
/// words, wherever they stand (`env git push`, `sh -c '…'`), so a commit
/// message saying "push" comes to you too.
pub fn reserved(tool: &str, input: &serde_json::Value, root: &str) -> Option<&'static str> {
    let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
    match tool {
        "Bash" => {
            let cmd = s("command");
            let words: Vec<&str> = cmd
                .split(|c: char| c.is_whitespace() || ";&|(){}`'\"<>".contains(c))
                .filter(|w| !w.is_empty())
                .collect();
            let base = |w: &str| w.rsplit('/').next().unwrap_or(w).to_string();
            let has = |name: &str| words.iter().any(|w| base(w) == name);
            if has("git") && words.iter().any(|w| w.contains("push")) {
                return Some("pushing");
            }
            if has("gh") && words.iter().any(|w| w.contains("merge")) {
                return Some("merging a PR");
            }
            if has("varde")
                && words.iter().any(|w| matches!(*w, "done" | "drop" | "park" | "trust" | "answer"))
            {
                return Some("closing a task, or acting in the user's name");
            }
            if cmd.contains(".claude") {
                return Some("changing agent settings");
            }
            if cmd.contains(".git/hooks") || cmd.contains(".git/config") {
                return Some("changing git's hooks or settings");
            }
            let deleting =
                ["rm", "rmdir", "shred", "unlink"].iter().any(|d| has(d)) || words.contains(&"-delete");
            if deleting {
                let paths = words.iter().skip(1).filter(|w| !w.starts_with('-'));
                if paths.clone().any(|w| w.contains('$') || w.contains('*') && w.starts_with('/')) {
                    return Some("deleting what the shell decides");
                }
                if paths.clone().any(|w| !inside(root, w)) {
                    return Some("deleting outside the worktree");
                }
            }
            None
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = [s("file_path"), s("notebook_path")].into_iter().find(|p| !p.is_empty()).unwrap_or("");
            let p = crate::project::normalize(&std::path::Path::new(root).join(expand(path)));
            if p.components().any(|c| c.as_os_str() == ".claude") {
                Some("changing agent settings")
            } else if p.components().any(|c| c.as_os_str() == ".git") {
                Some("changing git's own files")
            } else if !inside(root, path) {
                Some("writing outside the worktree")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `~` and `$HOME` at the start of a path, as a shell would read them.
fn expand(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    for p in ["~", "$HOME", "${HOME}"] {
        if path == p {
            return home;
        }
        if let Some(rest) = path.strip_prefix(&format!("{p}/")) {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

/// Whether `path`, as a session wrote it, is under `root`. A root too
/// near the top of the filesystem to be a worktree holds nothing.
fn inside(root: &str, path: &str) -> bool {
    let root = std::path::Path::new(root);
    if root.components().count() < 3 {
        return false;
    }
    crate::project::normalize(&root.join(expand(path))).starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(cmd: &str) -> Option<&'static str> {
        reserved("Bash", &json!({ "command": cmd }), "/w/t/r")
    }

    #[test]
    fn what_you_keep_never_goes_to_the_steward() {
        assert_eq!(bash("git push"), Some("pushing"));
        assert_eq!(bash("cargo test && git -C /w/t/r push origin x"), Some("pushing"));
        assert_eq!(bash("gh pr merge 12 --squash"), Some("merging a PR"));
        assert!(bash("varde done abcdef").is_some());
        assert!(bash("varde trust abcdef --allow Bash").is_some(), "nor rules in your name");
        assert_eq!(bash("rm -rf /home/me"), Some("deleting outside the worktree"));
        assert_eq!(bash("rm -r ../other"), Some("deleting outside the worktree"));
        assert!(bash("cat x > ~/.claude/settings.json").is_some());
        let write = |p: &str| reserved("Write", &json!({ "file_path": p }), "/w/t/r");
        assert_eq!(write("/etc/hosts"), Some("writing outside the worktree"));
        assert_eq!(write("/w/t/r/../u/x"), Some("writing outside the worktree"));
        assert_eq!(write("/w/t/r/.claude/settings.local.json"), Some("changing agent settings"));
    }

    /// What the review found getting past it: every one comes to you.
    #[test]
    fn shell_forms_do_not_get_what_you_keep_past_it() {
        for cmd in [
            "env git push",
            "command git push",
            "GIT_DIR=. git push",
            "{ git push; }",
            "if x; then git push; fi",
            "sh -c 'git push'",
            "/usr/bin/git push",
            "git -c alias.p=push p",
            "gh -R o/r pr merge 1",
            "cd .claude && tee settings.json",
            "rm -rf ~",
            "rm -rf $HOME",
            "rm -rf \"$X\"",
            "find / -delete",
            "echo x > .git/hooks/pre-commit",
        ] {
            assert!(bash(cmd).is_some(), "{cmd}");
        }
        let write = |p: &str| reserved("Write", &json!({ "file_path": p }), "/w/t/r");
        assert!(write("/w/t/r/.claude/./settings.local.json").is_some());
        assert!(write("/w/t/r//.claude/settings.json").is_some());
        assert!(write("/w/t/r/src/../.git/hooks/pre-commit").is_some());
        assert!(write("~/x").is_some());
        // A root too near the top is no worktree: nothing is inside it.
        assert!(reserved("Write", &json!({ "file_path": "/etc/x" }), "/").is_some());
    }

    #[test]
    fn ordinary_work_may() {
        assert_eq!(bash("cargo test --release"), None);
        assert_eq!(bash("git commit -m 'fix the cache'"), None);
        assert_eq!(bash("git log --oneline | head"), None);
        assert_eq!(bash("rm -rf target"), None);
        assert_eq!(reserved("Write", &json!({ "file_path": "/w/t/r/src/a.rs" }), "/w/t/r"), None);
        assert_eq!(reserved("WebFetch", &json!({ "url": "https://x" }), "/w/t/r"), None);
    }

    #[test]
    fn a_rule_allows_what_it_names_and_nothing_chained_after() {
        let rules = vec!["Bash(cargo test*)".to_string(), "Bash(git status)".into(), "Edit(src/*)".into()];
        let ok = |tool: &str, input: serde_json::Value| allowed(&rules, tool, &input, "/w/t/r");
        assert!(ok("Bash", json!({ "command": "cargo test --release" })));
        assert!(ok("Bash", json!({ "command": "git status && cargo test -p x" })));
        assert!(!ok("Bash", json!({ "command": "git status --short" })), "a pattern is all of it");
        assert!(!ok("Bash", json!({ "command": "cargo test && rm -rf ." })));
        assert!(!ok("Bash", json!({ "command": "cargo test $(curl x)" })));
        assert!(!ok("Bash", json!({ "command": "cargo build" })));
        assert!(ok("Edit", json!({ "file_path": "/w/t/r/src/a.rs" })));
        assert!(ok("Edit", json!({ "file_path": "src/b.rs" })), "relative, as written");
        assert!(!ok("Edit", json!({ "file_path": "/w/t/r/Cargo.toml" })));
        assert!(!ok("Edit", json!({ "file_path": "/w/t/r/src/../.mcp.json" })), "as it resolves");
        assert!(!ok("Write", json!({ "file_path": "/w/t/r/src/a.rs" })), "a rule is for its tool");
        assert!(allowed(&["WebFetch".into()], "WebFetch", &json!({ "url": "https://x" }), "/w/t/r"));
        assert!(rule("Bash(cargo").is_err() && rule("").is_err() && rule("Ba sh").is_err());
    }
}
