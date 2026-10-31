//! How a board row looks, the same in the terminal board and in `varde
//! list`: a glyph for where the task stands, its title, and on the right
//! what else there is to know. A status gets a word only while a session
//! is live: the group already says the rest.

use varde_core::board::Row;

/// What a part of a row says, for whoever colours it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// A session at work.
    Working,
    /// Its turn ended: yours now.
    YourTurn,
    /// It asks you something mid-turn, such as a permission.
    Asking,
    /// Nothing is running, or the task is closed.
    Quiet,
    /// Git work not committed yet.
    Edited,
    /// Git work that landed.
    Landed,
    /// A PR whose checks fail.
    Failing,
}

/// The glyph and tone for a row: filled while something needs looking
/// at, hollow once seen or idle.
pub fn glyph(r: &Row) -> (&'static str, Tone) {
    match r.status {
        "working" | "delegated" => ("●", Tone::Working),
        "blocked" => ("●", Tone::Asking),
        "waiting" => (if r.unread { "●" } else { "○" }, Tone::YourTurn),
        "done" => ("✓", Tone::Quiet),
        "parked" => ("‖", Tone::Quiet),
        "dropped" => ("✕", Tone::Quiet),
        // Waiting for its schedule: again and again, or once.
        _ => match &r.schedule {
            Some(s) if s.late => (if s.repeats { "↻" } else { "◷" }, Tone::Asking),
            Some(s) => (if s.repeats { "↻" } else { "◷" }, Tone::Quiet),
            None => ("○", Tone::Quiet),
        },
    }
}

/// A waiting session's word, which Needs you says already.
pub const YOUR_TURN: &str = "your turn";

/// The word for a live session's status; none otherwise.
pub fn word(status: &str) -> Option<(&'static str, Tone)> {
    match status {
        "working" | "delegated" => Some(("working", Tone::Working)),
        "waiting" => Some((YOUR_TURN, Tone::YourTurn)),
        "blocked" => Some(("needs input", Tone::Asking)),
        _ => None,
    }
}

/// What follows the title, each part with its tone: the status word,
/// what the session left running, where deferred work came from, git,
/// the project when the board shows several, and how long ago.
pub fn meta(r: &Row, now: i64, project: bool) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    if let Some((w, t)) = word(r.status) {
        out.push((w.to_string(), t));
    }
    let n = |k: u32, what: &str| (k > 0).then(|| format!("{k} {what}{}", if k == 1 { "" } else { "s" }));
    for part in [n(r.agents, "agent"), n(r.shells, "shell")].into_iter().flatten() {
        out.push((part, Tone::Quiet));
    }
    if r.steward_until.is_some() {
        out.push(("steward's".to_string(), Tone::Working));
    }
    if let Some(o) = &r.origin {
        out.push((format!("← {}", o.short), Tone::Quiet));
    }
    if let Some(g) = r.git.as_ref().filter(|g| !g.label.is_empty()) {
        let tone = match (g.stage.as_str(), g.checks.as_deref()) {
            (_, Some("failing")) => Tone::Failing,
            ("edited", _) => Tone::Edited,
            ("landed", _) => Tone::Landed,
            _ => Tone::Quiet,
        };
        out.push((g.label.clone(), tone));
    }
    if let Some(p) = r.project.as_deref().filter(|_| project) {
        out.push((crate::list::project_name(Some(p)).to_string(), Tone::Quiet));
    }
    // A task waiting for its schedule shows when it starts, not its age;
    // one at work that repeats, that `d` ends this run, not the task, how
    // many of its runs wait to be read, and that its next start is due,
    // waiting for this session: late once that has gone on for a day.
    match r.schedule.as_ref() {
        Some(s) if word(r.status).is_none() => {
            out.push((s.label.clone(), if s.late { Tone::Asking } else { Tone::Quiet }))
        }
        s => {
            if let Some(s) = s.filter(|s| s.repeats) {
                let mut mark = String::from("↻");
                // A row that is not itself the one unread result, such as
                // a live session, says it waits too.
                if r.results > 1 || (r.results == 1 && !r.unread) {
                    mark.push_str(&format!(" {} unread", r.results));
                }
                let long = now - s.due > 24 * 3_600_000;
                match (s.late, long) {
                    (true, false) => mark.push_str(" due"),
                    (true, true) => mark.push_str(" late"),
                    _ => {}
                }
                out.push((mark, if s.late && long { Tone::Asking } else { Tone::Quiet }));
            }
            out.push((crate::list::age(now - r.last), Tone::Quiet))
        }
    }
    out
}

/// Between the parts of a row's meta.
pub const SEP: &str = " · ";
