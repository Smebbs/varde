//! `varde note`, `varde recall` and `varde edit`.

use std::io::Write;
use std::path::Path;
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

/// A run by its full id, or by a unique ending of at least four
/// characters, such as the six `varde show` prints.
fn resolve_run(conn: &rusqlite::Connection, reference: &str) -> Result<String> {
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

pub fn recall(paths: &Paths, words: &[String], all: bool, as_json: bool) -> Result<()> {
    let store = paths.open(BUSY)?;
    let scope = crate::scope(&store, all)?;
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
            "nothing found{}",
            if all { String::new() } else { format!(" in {here} or any other project") }
        );
    } else if found.everywhere {
        println!("nothing in {here}; the best from every project:");
    }
    for h in &found.hits {
        // A project's cairn is named by its project.
        let place = if found.everywhere || h.task.is_empty() {
            h.project.as_deref().map(|p| format!("  ({p})"))
        } else {
            None
        };
        println!("{}  {}{}{}", h.short, h.title, closed(&h.state), place.unwrap_or_default());
        println!("    {}: {}{}", what_matched(&h.kind), clip(&h.snippet, 300), stale(h.stale));
    }
    Ok(())
}

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
    let versions =
        if history { memory::history_of(&store.conn, memory::Owner::Project(&project))? } else { Vec::new() };
    if as_json {
        let (cairn, version) = doc.map_or((None, 0), |(c, v)| (Some(c), v));
        let mut out = json!({"v": 1, "project": project, "version": version, "cairn": cairn});
        if history {
            out["history"] = json!(versions);
        }
        println!("{out}");
        return Ok(());
    }
    println!("project cairn  {project}");
    match &doc {
        Some((c, _)) if !c.is_empty() => {
            println!();
            // An empty section says nothing here.
            let text = c.display();
            let lines: Vec<&str> = text.lines().collect();
            for (n, line) in lines.iter().enumerate() {
                let heading = !line.starts_with("- ");
                if heading && lines.get(n + 1).is_none_or(|next| !next.starts_with("- ")) {
                    continue;
                }
                println!("  {line}");
            }
        }
        _ => println!("\n  none yet: tasks closed as done add to it, and varde edit --project writes to it"),
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
    if print {
        print!("{shown}{editable}{before_text}");
        return Ok(false);
    }
    let text = if stdin {
        std::io::read_to_string(std::io::stdin())?
    } else {
        let hint = hint(shape, context.as_ref().is_some_and(|c| c.linked.is_none()));
        edit_in_editor("cairn", &format!("{hint}{shown}{editable}{before_text}"))?
    };
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
            bail!("it was updated while you were editing; run varde edit again");
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
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(file.path())
        .status()?;
    if !status.success() {
        bail!("{editor} exited with {status}; nothing saved");
    }
    Ok(std::fs::read_to_string(file.path())?)
}
