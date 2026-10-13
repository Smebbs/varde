//! A cairn as varde shows it, in `varde show` and in the board's view,
//! in the same order and words: what a cairn is, how it is used, its
//! Status and sections (each always there, with its cap), and the notes
//! waiting for its next update.

use varde_core::cairn::{Cairn, Item, Section, Shape, STALE_MARK};
use varde_core::memory::Usage;
use varde_core::task;

use crate::list::age;
use crate::tasks::{tilde, Part};

/// Whose cairn it is.
#[derive(Clone, Copy)]
pub enum Whose<'a> {
    Task,
    /// A project's, by its path.
    Project(&'a str),
}

/// A line of a cairn as shown.
pub enum Line<'a> {
    /// What it is, in a line.
    About(String),
    /// How it is used: each part, and whether it is a warning.
    Usage(Vec<(String, bool)>),
    /// There is none yet: how it gets one.
    Empty(String),
    /// The Status, empty when there is none, and whether it is yours.
    Status(&'a str, bool),
    /// A section's heading, and how many lines it has.
    Heading(Section, usize),
    Bullet(&'a Item),
    /// The heading over notes waiting for the next update.
    Notes(&'static str),
    Note(&'a str),
}

/// The words for each part of a cairn shown. `add` says how to add a
/// line, for an empty one; with `shape`, an empty one shows its Status
/// and sections too, for a line to be added to.
pub fn lines<'a>(
    whose: Whose,
    cairn: Option<&'a Cairn>,
    usage: &Usage,
    notes: &'a [String],
    add: &str,
    shape: bool,
    now: i64,
) -> Vec<Line<'a>> {
    let mut out = vec![Line::About(match whose {
        Whose::Task => "Cairn · what its sessions learned, handed to the next one".into(),
        Whose::Project(p) => format!(
            "Project cairn · what {}'s tasks learned, handed to each of their sessions",
            crate::list::project_name(Some(p))
        ),
    })];
    let mut parts = Vec::new();
    if let Whose::Project(p) = whose {
        parts.push((tilde(p), false));
    }
    if let (Some(ts), Some(author)) = (usage.updated, usage.author.as_deref()) {
        let by = match (author, &usage.task, &usage.run) {
            ("user", ..) => "by you".to_string(),
            (_, Some(t), _) => format!("from task {}", task::short(t)),
            (_, None, Some(r)) => format!("by session {}", task::short(r)),
            _ => "by an update".to_string(),
        };
        parts.push((format!("updated {} ago {by}", age(now - ts)), false));
        parts.push(match (usage.sessions, usage.handed) {
            (0, _) | (_, None) => ("not handed to a session yet".to_string(), false),
            (1, Some(t)) => (format!("handed to 1 session, {} ago", age(now - t)), false),
            (n, Some(t)) => (format!("handed to {n} sessions, last {} ago", age(now - t)), false),
        });
    }
    if usage.stale > 0 {
        parts.push((format!("{} stale", usage.stale), true));
    }
    if !parts.is_empty() {
        out.push(Line::Usage(parts));
    }
    let kind = match whose {
        Whose::Task => Shape::Task,
        Whose::Project(_) => Shape::Project,
    };
    let c = cairn.filter(|c| !c.is_empty());
    if c.is_none() {
        out.push(Line::Empty(match whose {
            Whose::Task => format!("No cairn yet: its first session's update writes one; {add}."),
            Whose::Project(_) => format!("No project cairn yet: tasks closed as done add to it; {add}."),
        }));
    }
    if c.is_some() || shape {
        if kind == Shape::Task {
            out.push(c.map_or(Line::Status("", false), |c| Line::Status(&c.status, c.status_mine)));
        }
        for &s in kind.sections() {
            let items = c.map_or(&[][..], |c| c.section(s));
            out.push(Line::Heading(s, items.len()));
            out.extend(items.iter().map(Line::Bullet));
        }
    }
    if !notes.is_empty() {
        out.push(Line::Notes(if crate::config::get().memory.enabled {
            "Notes for the next update"
        } else {
            "Notes (memory updates are off)"
        }));
        out.extend(notes.iter().map(|n| Line::Note(n)));
    }
    out
}

/// The text of a heading: `Findings 2/7`.
pub fn heading(s: Section, n: usize) -> String {
    format!("{} {n}/{}", s.heading(), s.max())
}

/// What follows a bullet's or a Status's words: whose it is, and whether
/// its code is gone.
pub fn marks(mine: bool, stale: bool) -> String {
    format!("{}{}", if stale { STALE_MARK } else { "" }, if mine { " (yours)" } else { "" })
}

/// The lines as `varde show` prints them, each bullet followed by its
/// id (`#12`), which `varde edit --set`, `--rm` and `--restore` take.
pub fn text(lines: &[Line]) -> Vec<(Part, String)> {
    lines
        .iter()
        .map(|l| match l {
            Line::About(s) => (Part::Heading, s.clone()),
            Line::Usage(parts) => (
                Part::Quiet,
                format!("  {}", parts.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(" · ")),
            ),
            Line::Empty(s) => (Part::Quiet, format!("  {s}")),
            Line::Status("", _) => (Part::Text, "  Status  none yet".into()),
            Line::Status(s, mine) => (Part::Text, format!("  Status  {s}{}", marks(*mine, false))),
            Line::Heading(s, n) => (Part::Text, format!("  {}", heading(*s, *n))),
            Line::Bullet(i) => (Part::Text, format!("    - {}{}  #{}", i.text, marks(i.mine, i.stale), i.id)),
            Line::Notes(h) => (Part::Text, format!("  {h}")),
            Line::Note(n) => (Part::Text, format!("    - {n}")),
        })
        .collect()
}

/// An earlier version of a cairn, as `varde show --history` prints it:
/// its Status and the sections that have lines, marked as `text` marks
/// them, without ids (which belong to the cairn as it is now).
pub fn version(c: &Cairn) -> Vec<String> {
    let mut out = Vec::new();
    if !c.status.is_empty() {
        out.push(format!("Status  {}{}", c.status, marks(c.status_mine, false)));
    }
    for &s in c.shape.sections() {
        let items = c.section(s);
        if !items.is_empty() {
            out.push(s.heading().to_string());
            out.extend(items.iter().map(|i| format!("  - {}{}", i.text, marks(i.mine, i.stale))));
        }
    }
    out
}
