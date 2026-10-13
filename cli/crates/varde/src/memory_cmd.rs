//! `varde note`, `varde recall` and `varde edit`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension;
use serde_json::json;
use varde_core::cairn::{self, Cairn, Shape};
use varde_core::{memory, task};

use crate::{now_ms, Paths};

const BUSY: Duration = Duration::from_secs(2);

/// Pin a finding for the task's next cairn update. The run comes from
/// `--run`, else from `VARDE_RUN`, which sessions varde launched carry.
pub fn note(paths: &Paths, run: Option<String>, text: &[String]) -> Result<()> {
    let run = run
        .or_else(crate::run_env)
        .context("which run? pass --run <id>; the session's varde context names it")?;
    let store = paths.open(BUSY)?;
    let run = resolve_run(&store.conn, &run)?;
    let (_, redacted) = memory::add_note(&store, &run, &text.join(" "), now_ms())?;
    let state: Option<String> = store
        .conn
        .query_row("SELECT t.state FROM run r JOIN task t ON t.id = r.task WHERE r.id = ?1", [&run], |r| {
            r.get(0)
        })
        .optional()?;
    // Said as it is: no update may be coming to fold it in.
    let next = if !crate::config::get().memory.enabled {
        "memory updates are off (memory.enabled = false), so it stays a note, which recall finds".to_string()
    } else {
        match state.as_deref() {
            Some(s @ ("done" | "parked" | "dropped")) => {
                format!("the task is {s}, so no update folds it in until it is reopened; recall finds it")
            }
            _ => "it goes into the task's cairn at the next update".to_string(),
        }
    };
    if redacted > 0 {
        println!(
            "noted, with what read like a secret replaced by {} ({redacted} line(s)); never put secrets in a \
             note. {next}",
            cairn::REDACTED
        );
    } else {
        println!("noted; {next}");
    }
    Ok(())
}

/// How to name `run` to be typed back: the six characters `varde show`
/// prints, or the full id in the rare store where they name two runs.
pub fn short_run(conn: &rusqlite::Connection, run: &str) -> String {
    let short = task::short(run);
    if resolve_run(conn, &short).is_ok_and(|r| r == run) {
        short
    } else {
        run.to_string()
    }
}

/// A run by its full id, or by a unique ending of at least four
/// characters, such as the six `varde show` prints.
pub fn resolve_run(conn: &rusqlite::Connection, reference: &str) -> Result<String> {
    let r = reference.trim().to_ascii_uppercase();
    if r.len() < 4 || !r.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("no run {reference}: use its id, or the six characters varde show prints");
    }
    let ids: Vec<String> = conn
        .prepare(
            "SELECT id FROM run WHERE id = ?1 OR substr(id, -length(?1)) = ?1 ORDER BY id = ?1 DESC LIMIT 2",
        )?
        .query_map([&r], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    match ids.as_slice() {
        [] => bail!("no run {reference}"),
        [first, ..] if *first == r => Ok(first.clone()),
        [one] => Ok(one.clone()),
        _ => bail!("'{reference}' matches more than one run; use more characters"),
    }
}

pub fn recall(paths: &Paths, words: &[String], all: bool, dir: Option<PathBuf>, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    let scope = match dir {
        Some(dir) => Some(
            varde_core::project::of(&store.conn, &dir.canonicalize().unwrap_or(dir)).display().to_string(),
        ),
        None => crate::scope(&store, all)?,
    };
    // A session varde launched or tracks carries its run.
    let run = crate::run_env();
    let asker = memory::Asker { source: if run.is_some() { "agent" } else { "user" }, run: run.as_deref() };
    let found = memory::recall(&store, &words.join(" "), scope.as_deref(), None, 10, asker, now_ms())?;
    if as_json {
        println!("{}", json!({"v": 1, "hits": found.hits, "everywhere": found.everywhere}));
        return Ok(());
    }
    // Outside any repository the scope is the tasks made in this directory.
    let here = match &scope {
        Some(p) if !std::path::Path::new(p).join(".git").exists() => "the tasks made in this directory",
        _ => "this project",
    };
    if found.hits.is_empty() {
        println!(
            "nothing found{}\n{SEARCHES}",
            if all { String::new() } else { format!(" in {here} or any other project") }
        );
    } else if found.everywhere {
        println!("nothing in {here}; the best from every project:");
    }
    for h in &found.hits {
        // From every project, where each hit is from.
        let place = h
            .project
            .as_deref()
            .filter(|_| found.everywhere)
            .map(|p| format!("  ({})", crate::tasks::tilde(p)));
        let place = place.unwrap_or_default();
        if h.task.is_empty() {
            // A project's cairn is named by its project.
            println!("{} · project cairn{place}", crate::list::project_name(h.project.as_deref()));
        } else {
            println!("{}  {}{}{place}", h.short, h.title, closed(&h.state));
        }
        let more = match (h.more, h.task.is_empty()) {
            (0, _) => String::new(),
            (n, false) => format!("  (+{n} more in this task)"),
            (n, true) => format!("  (+{n} more in this project cairn)"),
        };
        println!("    {}: {}{}{more}", what_matched(&h.kind), clip(&h.snippet, 300), stale(h.stale));
    }
    Ok(())
}

/// What recall searches, said when it finds nothing: memory, not tasks.
pub const SEARCHES: &str =
    "recall searches what sessions learned (cairn lines, notes, project cairns), not a task's title or context";

/// How a hit's task is said to be over, if it is: ` (done)`.
pub fn closed(state: &str) -> String {
    match state {
        "done" | "parked" | "dropped" => format!(" ({state})"),
        _ => String::new(),
    }
}

/// The mark of a bullet whose code is gone, if it is.
pub fn stale(stale: bool) -> &'static str {
    if stale {
        varde_core::cairn::STALE_MARK
    } else {
        ""
    }
}

/// How a hit's kind is shown.
pub fn what_matched(kind: &str) -> &str {
    match kind {
        "findings" => "finding",
        "decisions" => "decision",
        "open" => "open",
        "history" => "earlier",
        "notes" => "note",
        "id" => "status",
        "conventions" => "convention",
        "gotchas" => "gotcha",
        "howto" => "how-to",
        other => other,
    }
}

/// At most `n` characters, ending in `…` when cut.
pub fn clip(text: &str, n: usize) -> String {
    let text = text.replace('\n', " ");
    match text.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", text[..i].trim_end()),
        None => text,
    }
}

/// Edit a task's cairn in `$EDITOR`. A client with its own editor, like
/// Emacs, prints the text to edit and hands it back on stdin. Lines you
/// add or change become yours, and updates must keep them.
/// Above the cairn is the task's context, its body, which is not capped:
/// a task linked elsewhere shows it only, since its link owns it.
/// Returns whether anything was saved.
pub fn edit(paths: &Paths, reference: &str, stdin: bool, print: bool) -> Result<bool> {
    let store = paths.open(BUSY)?;
    let id = task::resolve(&store, reference)?;
    let (body, linked) =
        store.conn.query_row("SELECT body, external FROM task WHERE id = ?1", [&id], |r| {
            Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get(1)?))
        })?;
    let context = TaskText { task: &id, body, linked };
    edit_doc(&store, memory::Owner::Task(&id), Some(context), stdin, print)
}

/// One line of a cairn to change, as `varde edit` takes it.
pub enum Line {
    /// A section's name, or `status`, and the text.
    Add(String, String),
    Set(i64, String),
    Rm(i64),
    /// `--rm status`: hand a Status the user set back to the updates.
    HandBack,
    Restore(i64),
}

/// What `--rm` takes: a line's id, or `status`.
#[derive(Clone, Copy, Debug)]
pub enum Rm {
    Line(i64),
    Status,
}

/// `--rm`'s word: `status`, or a line's id as [`line_id`] reads it.
pub fn rm_target(word: &str) -> std::result::Result<Rm, String> {
    if word.trim().eq_ignore_ascii_case("status") {
        return Ok(Rm::Status);
    }
    line_id(word).map(Rm::Line).map_err(|e| format!("{e}, or status"))
}

impl Line {
    /// From `--add SECTION TEXT…`, `--set ID TEXT…`, `--rm ID` or
    /// `--restore ID`, of which clap lets one through. TEXT `-` is stdin.
    pub fn of(
        add: Option<Vec<String>>,
        set: Option<Vec<String>>,
        rm: Option<Rm>,
        restore: Option<i64>,
    ) -> Result<Line> {
        let text = |words: &[String]| -> Result<String> {
            Ok(crate::arg_or_stdin(Some(words.join(" ")))?.unwrap_or_default())
        };
        Ok(match (add, set, rm, restore) {
            (Some(a), ..) => Line::Add(a[0].clone(), text(&a[1..])?),
            (_, Some(s), ..) => {
                let id = line_id(&s[0]).map_err(|e| anyhow::anyhow!("--set takes {e}"))?;
                Line::Set(id, text(&s[1..])?)
            }
            (_, _, Some(Rm::Line(id)), _) => Line::Rm(id),
            (_, _, Some(Rm::Status), _) => Line::HandBack,
            (_, _, _, Some(id)) => Line::Restore(id),
            _ => bail!("say which line: --add, --set or --rm"),
        })
    }
}

/// A line's id, as `varde show` prints it after the line (`#12`) or
/// without the `#`.
pub fn line_id(word: &str) -> std::result::Result<i64, String> {
    let w = word.trim();
    w.strip_prefix('#')
        .unwrap_or(w)
        .parse()
        .map_err(|_| format!("a line's id, the #12 varde show prints after it, not {word:?}"))
}

/// `varde edit --add`, `--set`, `--rm` or `--restore`: change one line of
/// a task's cairn, or with `project` of the cairn of the project that
/// directory is in.
pub fn edit_line(
    paths: &Paths,
    task: Option<&str>,
    project: Option<&Path>,
    line: Line,
    as_json: bool,
) -> Result<()> {
    let mut store = paths.open(BUSY)?;
    let key = match project {
        Some(dir) => crate::start::project_of(&store, dir)?.display().to_string(),
        None => task::resolve(&store, task.unwrap_or_default())?,
    };
    let owner = if project.is_some() { memory::Owner::Project(&key) } else { memory::Owner::Task(&key) };
    let change = match &line {
        Line::Add(s, text) if s.eq_ignore_ascii_case("status") => memory::Change::Status(text),
        Line::Add(s, text) => match cairn::Section::parse(s).filter(|s| owner.shape().sections().contains(s))
        {
            Some(s) => memory::Change::Add(s, text),
            None => bail!(
                "no section {s:?}: {}",
                match owner.shape() {
                    Shape::Task => "a task's cairn has status, findings, decisions and open",
                    Shape::Project => "a project cairn has conventions, gotchas and howto",
                }
            ),
        },
        Line::Set(id, text) => memory::Change::Set(*id, text),
        Line::Rm(id) => memory::Change::Remove(*id),
        Line::HandBack => memory::Change::HandBack,
        Line::Restore(id) => memory::Change::Restore(*id),
    };
    let done =
        memory::change(&mut store, owner, change, now_ms()).map_err(|e| anyhow::anyhow!("not saved: {e}"))?;
    if as_json {
        let out = json!({"v": 1, "version": done.version, "id": done.id, "changed": done.changed, "cairn": done.cairn});
        println!("{out}");
        return Ok(());
    }
    let id = done.id.unwrap_or_default();
    match line {
        _ if !done.changed => println!("unchanged"),
        Line::Add(..) if done.id.is_none() => {
            println!("Status set, yours now: updates won't change it; --rm status hands it back")
        }
        Line::HandBack => println!("Status handed back: the next update may change it"),
        Line::Add(..) => println!("added line #{id}"),
        Line::Set(..) => println!("changed line #{id}"),
        Line::Rm(_) => {
            println!("removed line #{id}; recall still finds it, and --restore {id} brings it back")
        }
        Line::Restore(_) => println!("brought back line #{id}"),
    }
    Ok(())
}

/// A task's own text, edited above its cairn.
struct TaskText<'a> {
    task: &'a str,
    body: String,
    /// Where the task is linked, which then owns the text.
    linked: Option<String>,
}

/// `varde show --project`: the cairn of the project `dir` is in, and
/// with `history` every earlier version of it.
pub fn show_project(paths: &Paths, dir: &Path, history: bool, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    let project = crate::start::project_of(&store, dir)?.display().to_string();
    let doc = memory::project_cairn(&store.conn, &project)?;
    let usage = memory::usage(&store.conn, memory::Owner::Project(&project))?;
    let versions =
        if history { memory::history_of(&store.conn, memory::Owner::Project(&project))? } else { Vec::new() };
    if as_json {
        let (cairn, version) = doc.map_or((None, 0), |(c, v)| (Some(c), v));
        let mut out = json!({"v": 1, "project": project, "version": version, "cairn": cairn, "usage": usage,
                             "limits": Shape::Project.limits()});
        if history {
            out["history"] = json!(versions);
        }
        println!("{out}");
        return Ok(());
    }
    let whose = crate::cairn_view::Whose::Project(&project);
    let add = "varde edit --project --add gotchas TEXT adds a line now";
    let lines =
        crate::cairn_view::lines(whose, doc.as_ref().map(|(c, _)| c), &usage, &[], add, false, now_ms());
    for (_, line) in crate::cairn_view::text(&lines) {
        println!("{line}");
    }
    crate::print_history(&versions);
    Ok(())
}

/// Edit the cairn of the project `dir` is in, as `edit` does a cairn.
pub fn edit_project(paths: &Paths, dir: &Path, stdin: bool, print: bool) -> Result<bool> {
    let store = paths.open(BUSY)?;
    let project = crate::start::project_of(&store, dir)?.display().to_string();
    edit_doc(&store, memory::Owner::Project(&project), None, stdin, print)
}

fn edit_doc(
    store: &varde_core::store::Store,
    owner: memory::Owner,
    context: Option<TaskText>,
    stdin: bool,
    print: bool,
) -> Result<bool> {
    let shape = owner.shape();
    let (before, version) =
        memory::document(&store.conn, owner)?.unwrap_or_else(|| (Cairn { shape, ..Default::default() }, 0));
    let before_text = before.render();
    // A linked task's context is shown as hint lines, which are not saved.
    let (shown, editable) = match &context {
        Some(TaskText { body, linked: Some(link), .. }) if !body.is_empty() => {
            let lines: String = body.lines().map(|l| format!("{HINT}{l}\n")).collect();
            (format!("{HINT}Context, from {link}: edit it there.\n{lines}"), String::new())
        }
        Some(TaskText { body, linked: None, .. }) if body.is_empty() => {
            (String::new(), "Context:\n\n".to_string())
        }
        Some(TaskText { body, linked: None, .. }) => (String::new(), format!("Context:\n{body}\n\n")),
        _ => (String::new(), String::new()),
    };
    // The version edited travels with the text, so a save after an
    // update that landed meanwhile is refused, not saved over it.
    let stamp = format!("{HINT}{VERSION}{version}\n");
    if print {
        print!("{stamp}{shown}{editable}{before_text}");
        return Ok(false);
    }
    let text = if stdin {
        std::io::read_to_string(std::io::stdin())?
    } else {
        let hint = hint(shape, context.as_ref().is_some_and(|c| c.linked.is_none()));
        edit_in_editor("cairn", &format!("{hint}{stamp}{shown}{editable}{before_text}"))?
    };
    // Text from before versions were stamped is checked against now.
    let edited_version = text
        .lines()
        .find_map(|l| l.strip_prefix(HINT)?.strip_prefix(VERSION)?.trim().parse::<i64>().ok())
        .unwrap_or(version);
    // The hint is not part of what was written.
    let text: String = text.split_inclusive('\n').filter(|l| !l.starts_with(HINT)).collect();
    let (new_body, cairn_text) = match &context {
        Some(_) => Cairn::split_context(&text),
        None => (None, text.clone()),
    };
    let linked = context.as_ref().and_then(|c| c.linked.as_deref());
    // Without a Context: line, the context is left as it is.
    let new_body =
        new_body.filter(|b| linked.is_some() || context.as_ref().is_some_and(|c| *b != c.body.trim()));
    let cairn_changed = cairn_text.trim_start_matches('\n') != before_text;
    if new_body.is_none() && !cairn_changed {
        println!("unchanged");
        return Ok(false);
    }
    // What was refused is kept, so nothing typed is lost.
    let refuse = |why: String| -> anyhow::Error {
        let kept = tempfile::Builder::new()
            .prefix("varde-edit-")
            .suffix(".md")
            .tempfile()
            .and_then(|mut f| f.write_all(text.as_bytes()).map(|_| f))
            .ok()
            .and_then(|f| f.keep().ok())
            .map(|(_, path)| format!("\nwhat you wrote is kept in {}", path.display()))
            .unwrap_or_default();
        anyhow::anyhow!("not saved: {why}{kept}")
    };
    if edited_version != version {
        return Err(refuse(
            "it was updated while you were editing: edit it again, and copy your changes in".into(),
        ));
    }
    if let (Some(link), Some(_)) = (linked, &new_body) {
        return Err(refuse(format!("this task's context is from {link}: edit it there")));
    }
    if cairn_changed {
        if let Err(e) = Cairn::check_edit(&cairn_text, shape) {
            return Err(refuse(match shape {
                Shape::Task => format!("{e}. The cairn is a Status: line and the Findings:, Decisions: and Open: headings, with a \"- \" before each line under them"),
                Shape::Project => format!("{e}. A project's cairn is the Conventions:, Gotchas: and How-to: headings, with a \"- \" before each line under them"),
            }));
        }
        let edited = Cairn::from_edit(&cairn_text, &before);
        if let Some(over) = edited.over_caps() {
            return Err(refuse(format!(
                "{over}. Every session is shown it, so it is kept short, your lines included: cut it down"
            )));
        }
        let w = memory::Write {
            author: "user",
            run: None,
            reason: Some("varde edit"),
            gone: &[],
            default_gone: "removed in varde edit",
            capped: &[],
            now: now_ms(),
        };
        if !memory::save_doc(&store.conn, owner, version, &edited, &w)? {
            return Err(refuse(
                "it was updated while you were editing: edit it again, and copy your changes in".into(),
            ));
        }
    }
    if let (Some(c), Some(b)) = (&context, &new_body) {
        task::set(store, c.task, None, Some(b))?;
    }
    println!("saved");
    Ok(true)
}

/// How a line of the hint an editor is opened with begins; such lines
/// are dropped from what is saved.
const HINT: &str = "#: ";

/// The hint line that says which version of the document was edited.
const VERSION: &str = "version ";

/// The format, said at the top of what an editor is opened with.
fn hint(shape: Shape, context: bool) -> String {
    let caps = match shape {
        Shape::Task => format!(
            "at most {} findings, {} decisions and {} open items, and {} words in all",
            cairn::MAX_FINDINGS,
            cairn::MAX_DECISIONS,
            cairn::MAX_OPEN,
            cairn::MAX_WORDS
        ),
        Shape::Project => format!(
            "at most {} conventions, {} gotchas and {} how-tos, and {} words in all",
            cairn::MAX_CONVENTIONS,
            cairn::MAX_GOTCHAS,
            cairn::MAX_HOWTO,
            cairn::MAX_PROJECT_WORDS
        ),
    };
    [
        "One line per bullet, each starting with \"- \", under its heading. Name the file or",
        "`identifier` a bullet is about. A line you add or change is yours ([mine]): updates",
        &format!(
            "never change or drop it. It holds {caps}. Lines starting with \"{}\" are left out.",
            HINT.trim()
        ),
        "The text under Context:, above them, is the task's own: its fresh sessions start",
        "with it, and it is not capped.",
    ]
    .iter()
    .take(if context { 5 } else { 3 })
    .map(|l| format!("{HINT}{l}\n"))
    .collect()
}

/// Edit `text` in `$VISUAL` or `$EDITOR`, in a file named after `what`.
pub(crate) fn edit_in_editor(what: &str, text: &str) -> Result<String> {
    let mut file = tempfile::Builder::new().prefix(&format!("varde-{what}-")).suffix(".md").tempfile()?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    // An empty one is unset, as git takes it.
    let set = |v: &str| std::env::var(v).ok().filter(|e| !e.trim().is_empty());
    let editor = set("VISUAL").or_else(|| set("EDITOR")).unwrap_or_else(|| "vi".into());
    // The shell says 127 when it finds no such command: nothing was
    // typed, so nothing is kept.
    let name = editor.split_whitespace().next().unwrap_or("vi");
    let missing = || anyhow::anyhow!("{name} not found: set $VISUAL or $EDITOR");
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(file.path())
        .status()
        .map_err(|_| missing())?;
    if status.code() == Some(127) {
        return Err(missing());
    }
    if !status.success() {
        // What was typed before the editor failed is kept all the same.
        let kept =
            file.keep().map(|(_, p)| format!("; what it had is kept in {}", p.display())).unwrap_or_default();
        bail!("{editor} exited with {status}; nothing saved{kept}");
    }
    Ok(std::fs::read_to_string(file.path())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_named_short_unless_its_ending_names_two() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE run (id TEXT PRIMARY KEY);
             INSERT INTO run VALUES ('01AAAAAAAAAAAAAAAAAAABCDEF'), ('01BBBBBBBBBBBBBBBBBBGHJKMN'),
                                    ('01CCCCCCCCCCCCCCCCCCGHJKMN');",
        )
        .unwrap();
        assert_eq!(short_run(&conn, "01AAAAAAAAAAAAAAAAAAABCDEF"), "abcdef");
        assert_eq!(resolve_run(&conn, "abcdef").unwrap(), "01AAAAAAAAAAAAAAAAAAABCDEF");
        assert_eq!(short_run(&conn, "01BBBBBBBBBBBBBBBBBBGHJKMN"), "01BBBBBBBBBBBBBBBBBBGHJKMN");
        assert!(resolve_run(&conn, "ghjkmn").is_err());
    }
}
