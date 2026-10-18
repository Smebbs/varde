//! The view: a pane under the board with two tabs, the task (its cairn,
//! notes, context and sessions) and its project (the project cairn); `[`
//! and `]` switch them. It takes the keys that work on a line, and leaves
//! the rest to the board: j/k pick a line, `e` changes it (on the Status,
//! the Status), `o` adds one to the section at point and `a` to a section
//! a key picks, typed on the prompt line under the board; `x` removes it
//! and `u` brings it back (on a Status you set, `x` hands it back to the
//! updates); `X` drops the task, asking first, as `x` does on the board;
//! J/K go to the next or previous task, the view
//! kept open; q or Esc go back to the board. RET, `s`, `d`, `n`, `/` and
//! every other key do what they do on the board, for the task in view.
//! Every change is one of `varde edit`'s, so the line is yours, and the
//! caps hold. A line being typed is never lost: Esc puts it away, one for
//! each line, shown dim in its section, and the same key on the same line
//! takes it up again; quitting keeps them for the next board.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use unicode_width::UnicodeWidthStr;
use varde_core::cairn::{Cairn, Section, Shape, MAX_BULLET_WORDS, MAX_STATUS_WORDS};
use varde_core::memory::{self, Change, Owner, Usage};
use varde_core::title::clean;

use super::draw::{key_spans, left_clip, tone_style, wrap};
use super::field::{Field, Words};
use super::keys::fit;
use super::{Act, App, Away, BUSY};
use crate::cairn_view;
use crate::list::{age, clip, project_name};
use crate::look::{self, Tone};
use crate::tasks::Part;
use crate::{now_ms, process};

/// Whose cairn: a task's, by its id, or a project's, by its path.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Whom {
    key: String,
    project: bool,
}

impl Whom {
    fn owner(&self) -> Owner<'_> {
        if self.project {
            Owner::Project(&self.key)
        } else {
            Owner::Task(&self.key)
        }
    }
}

/// A line that can be picked: the Status, a section's heading, a
/// bullet by its id, or a session by its place in the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pick {
    Status,
    Heading(Section),
    Bullet(i64),
    Session(usize),
}

/// What a line being typed will be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Status,
    Add(Section),
    Set(i64),
}

impl Target {
    /// The line the view highlights while it is typed.
    fn pick(self) -> Pick {
        match self {
            Target::Status => Pick::Status,
            Target::Add(s) => Pick::Heading(s),
            Target::Set(id) => Pick::Bullet(id),
        }
    }
}

/// A line being typed, in the cairn it was begun in.
struct Typing {
    whom: Whom,
    target: Target,
    field: Field,
    /// What the line had when it was begun: Esc with nothing changed
    /// keeps nothing.
    was: String,
    /// Taken up again from where it was put away: emptied, Esc says it is
    /// dropped.
    restored: bool,
}

impl Typing {
    fn changed(&self) -> bool {
        self.field.text().trim() != self.was.trim()
    }
}

/// The cairn as read from the store, with what is shown around it.
struct Doc {
    whom: Whom,
    cairn: Option<Cairn>,
    usage: Usage,
    notes: Vec<String>,
    /// The branch of a task's worktree.
    worktree: Option<String>,
    /// A task's context, under it.
    rest: Vec<(Part, String)>,
    /// Its sessions under that: their heading, and a conversation a line,
    /// newest first.
    sessions: Option<(String, Vec<varde_core::task::Conversation>)>,
}

#[derive(Clone)]
enum Undo {
    /// A bullet removed, by its id.
    Line(i64),
    /// The Status handed back, as it read.
    HandBack(String),
}

#[derive(Default)]
pub(super) struct View {
    at: Option<Pick>,
    typing: Option<Typing>,
    /// `a` was pressed: the next key picks the section.
    picking: bool,
    /// Lines put away with Esc, one for each line, to be taken up again.
    kept: Vec<Typing>,
    /// What `u` undoes: the bullet last removed, or the Status last
    /// handed back.
    undo: Option<(Whom, Undo)>,
    pub(super) scroll: u16,
    /// Bring the picked line into view at the next draw.
    follow: bool,
    doc: Option<Doc>,
    /// The row each line that can be picked starts on, the rows the
    /// pane shows, and how far it scrolls, as last drawn.
    spots: Vec<(Pick, u16)>,
    height: u16,
    most: u16,
}

impl View {
    /// Open on a cairn, nothing picked yet; what was put away stays, and
    /// so does what `u` brings back.
    pub(super) fn open(&mut self) {
        let (kept, undo) = (std::mem::take(&mut self.kept), self.undo.take());
        *self = View { kept, undo, follow: true, ..View::default() };
    }

    /// Nothing for `u` to undo in the cairn: something newer was done.
    pub(super) fn forget_undo(&mut self) {
        self.undo = None;
    }

    /// Whether the cairn is read, for a test.
    #[cfg(test)]
    pub(super) fn loaded(&self) -> bool {
        self.doc.is_some()
    }

    /// Read the cairn again before the next draw.
    pub(super) fn reload(&mut self) {
        self.doc = None;
    }

    /// Whether a line is being typed on the prompt line.
    pub(super) fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// Pasted text goes into the line being typed, as one line.
    pub(super) fn paste(&mut self, text: &str) -> bool {
        let Some(t) = &mut self.typing else { return false };
        t.field.insert(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        true
    }

    /// The lines that can be picked, in the order they are shown: the
    /// Status, then each heading and its bullets.
    fn picks(&self) -> Vec<Pick> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let shape = shape(&doc.whom);
        let c = doc.cairn.as_ref().filter(|c| !c.is_empty());
        let mut out = Vec::new();
        // An empty one shows no Status: `a s` sets it.
        if shape == Shape::Task && c.is_some() {
            out.push(Pick::Status);
        }
        for &s in shape.sections() {
            out.push(Pick::Heading(s));
            out.extend(c.into_iter().flat_map(|c| c.section(s)).map(|i| Pick::Bullet(i.id)));
        }
        let sessions = doc.sessions.as_ref().map_or(0, |(_, c)| c.len());
        out.extend((0..sessions).map(Pick::Session));
        out
    }

    /// The run RET opens: the picked session's.
    fn session(&self) -> Option<String> {
        let Some(Pick::Session(i)) = self.at else { return None };
        Some(self.doc.as_ref()?.sessions.as_ref()?.1.get(i)?.run.clone())
    }

    fn step(&mut self, by: isize) {
        let picks = self.picks();
        let Some(last) = picks.len().checked_sub(1) else { return };
        let i = self.at.and_then(|a| picks.iter().position(|p| *p == a)).unwrap_or(0) as isize;
        self.at = Some(picks[(i + by).clamp(0, last as isize) as usize]);
        self.follow = true;
    }

    /// C-d and C-u: half the pane down or up; the picked line stays if
    /// it is still in view, else the nearest one in view is picked.
    fn half(&mut self, down: bool) {
        let n = (self.height / 2).max(1);
        self.scroll =
            if down { self.scroll.saturating_add(n).min(self.most) } else { self.scroll.saturating_sub(n) };
        self.follow = false;
        let (from, to) = (self.scroll, self.scroll + self.height);
        let shown = |row: u16| row >= from && row < to;
        if self.spots.iter().any(|s| Some(s.0) == self.at && shown(s.1)) {
            return;
        }
        let mut seen = self.spots.iter().filter(|s| shown(s.1));
        if let Some(s) = if down { seen.next() } else { seen.next_back() } {
            self.at = Some(s.0);
        }
    }

    /// The text of the bullet `id`, and its section.
    fn bullet(&self, id: i64) -> Option<(Section, String)> {
        let c = self.doc.as_ref()?.cairn.as_ref()?;
        c.items().find(|(_, i)| i.id == id).map(|(s, i)| (s, i.text.clone()))
    }

    /// What `target` says now: the Status's words, or the line's.
    fn words(&self, target: Target) -> String {
        match target {
            Target::Status => self.doc.as_ref().and_then(|d| d.cairn.as_ref()).map(|c| c.status.clone()),
            Target::Add(_) => None,
            Target::Set(id) => self.bullet(id).map(|(_, t)| t),
        }
        .unwrap_or_default()
    }

    /// Whether the Status is the user's, which `x` hands back.
    fn status_mine(&self) -> bool {
        self.doc.as_ref().and_then(|d| d.cairn.as_ref()).is_some_and(|c| c.status_mine)
    }

    /// The section the picked line is in.
    fn section(&self) -> Option<Section> {
        match self.at? {
            Pick::Status | Pick::Session(_) => None,
            Pick::Heading(s) => Some(s),
            Pick::Bullet(id) => self.bullet(id).map(|(s, _)| s),
        }
    }

    /// Why section `s` takes no line more, when it is full.
    fn full(&self, s: Section) -> Option<String> {
        let n = self.doc.as_ref()?.cairn.as_ref().map_or(0, |c| c.section(s).len());
        (n >= s.max()).then(|| {
            format!("{} is full ({n}/{}): x removes a line to make room, e changes one", s.heading(), s.max())
        })
    }

    /// The lines typed and put away, and the one being typed if it says
    /// anything new, as the drafts file keeps them.
    pub(super) fn drafts(&self) -> Vec<Value> {
        let all = self.kept.iter().chain(self.typing.as_ref().filter(|t| t.changed()));
        all.filter(|t| !t.field.text().trim().is_empty())
            .map(|t| {
                let target = match t.target {
                    Target::Status => "status".to_string(),
                    Target::Add(s) => format!("add {}", s.name()),
                    Target::Set(id) => format!("set {id}"),
                };
                let key = if t.whom.project { "project" } else { "task" };
                json!({ key: t.whom.key, "target": target, "text": t.field.text() })
            })
            .collect()
    }

    /// Take back the lines [`drafts`](Self::drafts) wrote among `all`;
    /// for each, whose cairn it is in (a task's id, or a project's path)
    /// and where in it: the Status, a section, or a line.
    pub(super) fn take_drafts(&mut self, all: &[Value]) -> Vec<(String, bool, String)> {
        let mut took = Vec::new();
        for d in all {
            let Some((whom, target, text)) = draft(d) else { continue };
            let what = match target {
                Target::Status => "Status".to_string(),
                Target::Add(s) => s.heading().to_string(),
                Target::Set(id) => format!("line #{id}"),
            };
            took.push((whom.key.clone(), whom.project, what));
            let field = Field::new(&text);
            self.kept.push(Typing { whom, target, field, was: String::new(), restored: false });
        }
        took
    }
}

impl App<'_> {
    /// Whose cairn the view is on: the selected task's, or on the Project
    /// tab its project's.
    fn whom(&self) -> Option<Whom> {
        let row = self.current()?;
        if self.project_detail {
            Some(Whom { key: row.project.clone()?, project: true })
        } else {
            Some(Whom { key: row.task.clone(), project: false })
        }
    }

    /// A key while the view is open: `Some` with what it asks for when
    /// the view took it, `None` for the board's.
    pub(super) fn view_key(&mut self, key: KeyEvent, pending_g: bool) -> Option<Option<Away>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(mut t) = self.view.typing.take() {
            match key.code {
                KeyCode::Enter => self.save_line(t),
                KeyCode::Esc => self.put_line_away(t),
                KeyCode::Char('g') if ctrl => self.put_line_away(t),
                _ => {
                    t.field.key(key, Words::Prose);
                    self.view.typing = Some(t);
                }
            }
            return Some(None);
        }
        // Keys before the first draw act on the cairn as it is.
        self.load_view();
        let whom = self.whom();
        if std::mem::take(&mut self.view.picking) {
            let section = Section::ALL.into_iter().find(|s| {
                whom.as_ref().is_some_and(|w| shape(w).sections().contains(s))
                    && key.code == KeyCode::Char(section_key(*s))
            });
            match (section, key.code) {
                (Some(s), _) => self.add_line(s),
                (None, KeyCode::Char('s')) if whom.as_ref().is_some_and(|w| !w.project) => {
                    self.write_line(Target::Status)
                }
                _ => self.message = "cancelled".into(),
            }
            return Some(None);
        }
        let v = &mut self.view;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if !ctrl => v.step(1),
            KeyCode::Char('k') | KeyCode::Up if !ctrl => v.step(-1),
            KeyCode::Char('g') if !ctrl && !pending_g => self.pending_g = true,
            KeyCode::Char('g') | KeyCode::Home if !ctrl => v.step(isize::MIN / 2),
            // The last line that can be picked, and all that is under it.
            KeyCode::Char('G') | KeyCode::End => {
                v.step(isize::MAX / 2);
                (v.scroll, v.follow) = (u16::MAX, false);
            }
            // RET on a session opens it, not the task's newest.
            KeyCode::Enter if v.session().is_some() => {
                let start = super::Start { go: true, run: v.session(), ..Default::default() };
                // A closed task is asked about first, as RET on its row asks.
                if let Some(r) = self.current().filter(|r| super::is_closed(&r.state)) {
                    self.mode = super::Mode::Reopen(start, r.task.clone());
                    return Some(None);
                }
                return Some(self.begin(start));
            }
            KeyCode::Char('d') if ctrl => v.half(true),
            KeyCode::Char('u') if ctrl => v.half(false),
            KeyCode::PageDown => v.half(true),
            KeyCode::PageUp => v.half(false),
            _ if ctrl => return None,
            KeyCode::Char('J') => self.step_task(true),
            KeyCode::Char('K') => self.step_task(false),
            KeyCode::Char('[') | KeyCode::Char(']') => {
                self.project_detail = !self.project_detail;
                self.view.open();
            }
            // Marks first, as on the board.
            KeyCode::Esc if !self.marked.is_empty() => self.marked.clear(),
            KeyCode::Esc => self.detail = false,
            KeyCode::Char('a') | KeyCode::Char('o') if whom.is_none() => {
                self.message = "this task has no project yet: its first start picks one".into()
            }
            KeyCode::Char('a') => v.picking = true,
            KeyCode::Char('e') if matches!(v.at, Some(Pick::Bullet(_) | Pick::Status)) => match v.at {
                Some(Pick::Bullet(id)) => self.write_line(Target::Set(id)),
                _ => self.write_line(Target::Status),
            },
            // On a heading, `e` adds under it as `o` does: a heading has no
            // words of its own to change.
            KeyCode::Char('o') | KeyCode::Char('e') if matches!(v.at, Some(Pick::Session(_))) => {
                self.message = "a session is not a line of the cairn: RET opens it".into()
            }
            KeyCode::Char('o') | KeyCode::Char('e') => match v.section() {
                Some(s) => self.add_line(s),
                None => v.picking = true,
            },
            KeyCode::Char('x') => match (v.at, whom) {
                (Some(Pick::Bullet(id)), Some(w)) => self.remove_line(w, id),
                // A Status the user set is handed back to the updates.
                (Some(Pick::Status), Some(w)) if v.status_mine() => {
                    let was = v.words(Target::Status);
                    if self.change_line(
                        &w,
                        Change::HandBack,
                        "Status handed back: the next update may change it; u undoes",
                    ) {
                        self.view.undo = Some((w, Undo::HandBack(was)));
                    }
                }
                (Some(Pick::Status), _) => {
                    self.message = "the Status is already the updates': e makes it yours".into()
                }
                _ => self.message = "pick a line with j/k to remove it".into(),
            },
            // `x` is the line's here: `X` drops the task in view, asking
            // as `x` on the board does.
            KeyCode::Char('X') => self.normal(Act::Drop),
            KeyCode::Char('u') => match v.undo.take() {
                Some((w, undo)) if Some(&w) == whom.as_ref() => {
                    let done = match &undo {
                        Undo::Line(id) => self.change_line(&w, Change::Restore(*id), "brought back"),
                        Undo::HandBack(was) => self.change_line(
                            &w,
                            Change::Status(was),
                            "Status yours again: updates won't change it",
                        ),
                    };
                    // Refused, as when its section has filled since, it can be tried again.
                    if !done {
                        self.view.undo = Some((w, undo));
                    }
                }
                // Nothing of the cairn's to undo: a task closed from the
                // view is reopened, as `u` on the board does.
                undo if self.closed_in_view && !self.closed.is_empty() => {
                    v.undo = undo;
                    self.reopen_last();
                }
                undo => {
                    v.undo = undo;
                    self.message = "nothing here to undo".into();
                }
            },
            _ => return None,
        }
        Some(None)
    }

    /// Begin a line for section `s`, unless it is full.
    fn add_line(&mut self, s: Section) {
        match self.view.full(s) {
            Some(why) => self.message = why,
            None => self.write_line(Target::Add(s)),
        }
    }

    /// Begin typing `target`: what was put away for it, else its words.
    fn write_line(&mut self, target: Target) {
        let Some(whom) = self.whom() else { return };
        let was = self.view.words(target);
        let kept = &mut self.view.kept;
        let typing = match kept.iter().position(|k| k.whom == whom && k.target == target) {
            Some(i) => {
                self.message = "your draft, as you left it".into();
                Typing { was, restored: true, ..kept.remove(i) }
            }
            None => Typing { whom, target, field: Field::new(&was), was, restored: false },
        };
        self.view.typing = Some(typing);
        self.view.follow = true;
    }

    /// Esc on a line being typed: keep it to take up again, unless it says
    /// nothing, or nothing new. To throw a draft away: C-u, then Esc.
    fn put_line_away(&mut self, t: Typing) {
        if t.field.text().trim().is_empty() || !t.changed() {
            if t.restored {
                self.message = "draft dropped".into();
            }
            return;
        }
        let whose = match &t.whom {
            Whom { key, project: true } => format!("{}'s project cairn", project_name(Some(key))),
            Whom { key, .. } => self.row_of(key).map_or_else(
                || varde_core::task::short(key),
                |r| format!("\"{}\"", clip(&clean(&r.title), 24)),
            ),
        };
        let again = match t.target {
            Target::Status => "e on the Status".to_string(),
            Target::Add(s) => format!("a {}", section_key(s)),
            Target::Set(_) => "e on the line".into(),
        };
        self.message = format!("draft kept for {whose}: {again} continues it");
        self.view.kept.push(t);
    }

    /// Save the line typed. A refusal leaves it open, saying why.
    fn save_line(&mut self, t: Typing) {
        let text = t.field.text().to_string();
        let change = match t.target {
            Target::Status => Change::Status(&text),
            Target::Add(s) => Change::Add(s, &text),
            Target::Set(id) => Change::Set(id, &text),
        };
        let done = (|| memory::change(&mut self.paths.open(BUSY)?, t.whom.owner(), change, now_ms()))();
        match done {
            Ok(done) => {
                self.message = match (t.target, done.changed) {
                    (_, false) => "unchanged".into(),
                    (Target::Status, _) => {
                        "Status set, yours now: updates won't change it; x hands it back".into()
                    }
                    (Target::Add(s), _) => format!("added to {}, as yours", s.heading()),
                    (Target::Set(_), _) => "saved, as yours".into(),
                };
                self.view.at = done.id.map(Pick::Bullet).or(Some(Pick::Status));
                self.after_change();
            }
            Err(e) => {
                self.message = format!("error: {e:#}");
                self.view.typing = Some(t);
            }
        }
    }

    fn remove_line(&mut self, whom: Whom, id: i64) {
        // What is picked next: the line after it, else the one before.
        let picks = self.view.picks();
        let at = picks.iter().position(|p| *p == Pick::Bullet(id));
        let next = at.and_then(|i| picks.get(i + 1).or_else(|| i.checked_sub(1).and_then(|j| picks.get(j))));
        let next = next.copied();
        let section = self.view.bullet(id).map(|(s, _)| s);
        if self.change_line(
            &whom,
            Change::Remove(id),
            "removed: u brings it back, and recall no longer finds it",
        ) {
            self.view.undo = Some((whom.clone(), Undo::Line(id)));
            self.view.at = next;
            // A change being typed to it is a line to add now.
            if let Some(s) = section {
                orphan(&mut self.view.kept, &whom, id, s);
            }
        }
    }

    /// Make a change that needs no typing; whether it was made.
    fn change_line(&mut self, whom: &Whom, change: Change, said: &str) -> bool {
        let done = (|| memory::change(&mut self.paths.open(BUSY)?, whom.owner(), change, now_ms()))();
        match done {
            Ok(done) => {
                self.message = said.into();
                if let (Change::Restore(_), Some(id)) = (change, done.id) {
                    self.view.at = Some(Pick::Bullet(id));
                }
                self.after_change();
                true
            }
            Err(e) => {
                self.message = format!("error: {e:#}");
                false
            }
        }
    }

    fn after_change(&mut self) {
        self.view.follow = true;
        self.view.reload();
        self.stale = true;
    }

    /// Read the cairn the view is on, if it is not read yet. Another
    /// one's is read from its top, nothing picked.
    pub(super) fn load_view(&mut self) {
        let whom = self.whom();
        if self.view.doc.as_ref().is_some_and(|d| Some(&d.whom) == whom.as_ref()) {
            return;
        }
        let Some(whom) = whom else {
            self.view.doc = None;
            return;
        };
        if self.view.doc.as_ref().is_some_and(|d| d.whom != whom) {
            self.view.open();
        }
        // A repeating task lists every session, under its schedule.
        let schedule = self.current().and_then(|r| r.schedule.as_ref()).filter(|s| s.repeats);
        let schedule = schedule.map(|s| {
            use varde_core::schedule::{from_ms, when};
            let next = if s.due <= now_ms() { "due" } else { "next" };
            format!("{}, {next} {}", s.rule, when(from_ms(s.due), from_ms(now_ms())))
        });
        let read = || -> anyhow::Result<(Doc, Vec<(i64, Section)>)> {
            let s = self.paths.open(BUSY)?;
            let owner = whom.owner();
            let cairn = memory::document(&s.conn, owner)?.map(|(c, _)| c);
            let usage = memory::usage(&s.conn, owner)?;
            // Changes put away to lines no longer there: their sections.
            let mut gone = Vec::new();
            for k in self.view.kept.iter().filter(|k| k.whom == whom) {
                if let Target::Set(id) = k.target {
                    if !cairn.as_ref().is_some_and(|c| c.items().any(|(_, i)| i.id == id)) {
                        let s: Option<String> = s
                            .conn
                            .query_row("SELECT section FROM cairn_item WHERE id = ?1", [id], |r| r.get(0))
                            .ok();
                        let first = shape(&whom).sections()[0];
                        let s = s.and_then(|s| Section::ALL.into_iter().find(|x| x.name() == s));
                        gone.push((id, s.unwrap_or(first)));
                    }
                }
            }
            let (notes, worktree, rest, sessions) = match owner {
                Owner::Project(_) => (Vec::new(), None, Vec::new(), None),
                Owner::Task(id) => {
                    let notes = memory::pending_notes(&s.conn, id)?.into_iter().map(|(_, t)| t).collect();
                    let d = varde_core::task::detail(&s, id, &process::alive)?;
                    let worktree =
                        d.worktree.as_ref().filter(|w| w.state == "active").map(|w| w.branch.clone());
                    (notes, worktree, rest(&d), sessions(d, schedule.as_deref()))
                }
            };
            Ok((Doc { whom: whom.clone(), cairn, usage, notes, worktree, rest, sessions }, gone))
        };
        match read() {
            Ok((doc, gone)) => {
                for (id, s) in gone {
                    orphan(&mut self.view.kept, &doc.whom, id, s);
                }
                // A line picked that is gone is picked no longer.
                self.view.doc = Some(doc);
                let picks = self.view.picks();
                if !self.view.at.is_some_and(|a| picks.contains(&a)) {
                    self.view.at =
                        picks.iter().find(|p| matches!(p, Pick::Bullet(_))).or(picks.first()).copied();
                }
            }
            Err(e) => {
                self.view.doc = None;
                self.message = format!("error: {e:#}");
            }
        }
    }

    /// What the view shows under its tabs, in `width` columns, as
    /// [`view_lines`](Self::view_lines) has it: read first, so the pane
    /// is as tall as it needs.
    pub(super) fn view_body(&mut self, width: usize) -> Body {
        self.load_view();
        match &self.view.doc {
            Some(doc) => self.view_lines(doc, width),
            None if self.current().is_none() => {
                (vec![Line::from("No task here.".dark_gray())], None, Vec::new())
            }
            None => (
                vec![Line::from("This task has no project yet: its first start picks one.".dark_gray())],
                None,
                Vec::new(),
            ),
        }
    }

    /// Draw the view's `body` in `area`: a pane with the task's title (on
    /// the Project tab, the project's name) in its border, its tabs on
    /// top, and in its bottom border a sign while there is more below.
    pub(super) fn draw_view(&mut self, f: &mut Frame, area: Rect, body: Body) {
        let pane = || Block::bordered().border_type(BorderType::Rounded).padding(Padding::horizontal(1));
        let inner = pane().inner(area);
        let [tabs, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        let width = inner.width as usize;
        let (lines, picked, spots) = body;
        let height = rest.height;
        let v = &mut self.view;
        if let Some((top, bottom)) = picked.filter(|_| v.follow) {
            if top < v.scroll {
                v.scroll = top.saturating_sub(1);
            } else if bottom >= v.scroll + height {
                v.scroll = bottom + 1 - height.min(bottom + 1);
            }
        }
        (v.spots, v.height, v.most) = (spots, height, lines.len().saturating_sub(height as usize) as u16);
        v.scroll = v.scroll.min(v.most);
        let more = v.scroll < v.most;
        let scroll = v.scroll;
        let mut block = pane().title(self.view_title(width));
        if more {
            block = block.title_bottom(more_below());
        }
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(self.tab_bar(width)), tabs);
        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), rest);
    }

    /// The pane's title: the task's title and short id; on the Project
    /// tab, the project's name.
    fn view_title(&self, width: usize) -> Line<'static> {
        let Some(r) = self.current() else { return Line::default() };
        if self.project_detail {
            let name = r.project.as_deref().map(|p| clean(project_name(Some(p))));
            return name.map_or_else(Line::default, |n| Line::from(format!(" {} ", clip(&n, width)).bold()));
        }
        let title = clip(&clean(&r.title), width.saturating_sub(r.short.width() + 4).max(8));
        Line::from(vec![" ".into(), title.bold(), format!("  {} ", r.short).dark_gray()])
    }

    /// The tabs, the one shown standing out, and what a cairn is, where
    /// there is room for it.
    fn tab_bar(&self, width: usize) -> Line<'static> {
        let tab = |name: &'static str, on: bool| {
            if on {
                Span::raw(format!(" {name} ")).bold().reversed()
            } else {
                Span::raw(format!(" {name} ")).dark_gray()
            }
        };
        let mut spans =
            vec![tab("Task", !self.project_detail), " ".into(), tab("Project", self.project_detail)];
        // The longest that fits.
        let about = match self.current().map(|r| r.project.as_deref()) {
            _ if !self.project_detail => {
                vec!["cairn: what its sessions learned, handed to the next one".to_string()]
            }
            Some(Some(p)) => {
                let name = clean(project_name(Some(p)));
                vec![
                    format!("project cairn: what holds across {name}, handed to each new session"),
                    format!("what holds across {name}, handed to each new session"),
                    format!("what holds across {name}"),
                ]
            }
            _ => Vec::new(),
        };
        let used: usize = spans.iter().map(Span::width).sum();
        if let Some(about) = about.into_iter().find(|a| used + 4 + a.width() <= width) {
            spans.push(" ".repeat(width - used - about.width()).into());
            spans.push(about.dark_gray());
        }
        Line::from(spans)
    }

    /// The view's lines; the first and last of the picked one, or of the
    /// one being typed, to keep in view; and where each line that can be
    /// picked is.
    fn view_lines(&self, doc: &Doc, width: usize) -> (Vec<Line<'static>>, Option<(u16, u16)>, Spots) {
        let v = &self.view;
        let whose = match doc.whom.project {
            true => cairn_view::Whose::Project(&doc.whom.key),
            false => cairn_view::Whose::Task,
        };
        let parts = cairn_view::lines(whose, doc.cairn.as_ref(), &doc.usage, &doc.notes, "", true, now_ms());
        // What is highlighted: the line being typed, else the one picked.
        let typing = v.typing.as_ref().filter(|t| t.whom == doc.whom);
        let hl = typing.map(|t| t.target.pick()).or(v.at);
        let quiet = Style::default().fg(Color::DarkGray);
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut picked = None;
        let mut spots = Vec::new();
        // Where the task stands and how its cairn is used, on one line.
        let task = self.current().filter(|_| !doc.whom.project);
        let facts = self.facts(task, doc, width);
        if !facts.spans.is_empty() {
            out.push(facts);
        }
        // Under it, a blank line only between parts.
        let top = out.len();
        let gap = |out: &mut Vec<Line<'static>>| {
            if out.len() > top {
                out.push(Line::default());
            }
        };
        // The lines put away for this cairn, each a dim line where it goes;
        // a change to a line by its end, where it is likely to differ.
        let kept: Vec<&Typing> = v.kept.iter().filter(|k| k.whom == doc.whom).collect();
        let draft = |out: &mut Vec<Line<'static>>, target: Target| {
            for k in kept.iter().filter(|k| k.target == target) {
                let add = matches!(target, Target::Add(_));
                let tail = if add { " · o continues" } else { " · e continues" };
                let room = width.saturating_sub(4 + 7 + tail.width());
                let words = clean(k.field.text());
                let words = if add { clip(&words, room) } else { left_clip(&words, room) };
                let text = format!("    draft: {words}{tail}");
                out.push(Line::from(Span::styled(text, quiet)));
            }
        };
        // An empty cairn is its hint and its sections' headings, dim.
        let empty = doc.cairn.as_ref().is_none_or(|c| c.is_empty());
        // The section whose lines are being shown: what is put away for it
        // goes after them.
        let mut open: Option<Section> = None;
        for part in &parts {
            if matches!(part, cairn_view::Line::Heading(..) | cairn_view::Line::Notes(_)) {
                if let Some(s) = open.take() {
                    draft(&mut out, Target::Add(s));
                }
            }
            let from = out.len() as u16;
            let at = |p: Pick| hl == Some(p);
            match part {
                cairn_view::Line::About(_) | cairn_view::Line::Usage(_) => continue,
                cairn_view::Line::Empty(_) => {
                    let s = if doc.whom.project {
                        "No project cairn yet: it holds only what you add; a adds a line."
                    } else {
                        "No cairn yet: its first session's update writes one; a adds a line."
                    };
                    gap(&mut out);
                    out.extend(wrap(s, width).into_iter().map(|l| Line::from(Span::styled(l, quiet))));
                }
                cairn_view::Line::Status(..) if empty => continue,
                // Its heading, then its words under it, marked as a line is.
                cairn_view::Line::Status(text, mine) => {
                    gap(&mut out);
                    out.push(Line::from("Status".bold()));
                    spots.push((Pick::Status, out.len() as u16));
                    let lead = vec![Span::styled(if *mine { "  › " } else { "    " }, quiet)];
                    let words = if text.is_empty() { "none yet" } else { text };
                    bullet(&mut out, lead, words, Vec::new(), at(Pick::Status), text.is_empty(), width);
                    draft(&mut out, Target::Status);
                }
                cairn_view::Line::Heading(s, n) => {
                    open = Some(*s);
                    // A project's cairn has no Status to set it apart, nor
                    // does an empty one.
                    if (doc.whom.project || empty) && *s == shape(&doc.whom).sections()[0] {
                        gap(&mut out);
                    }
                    spots.push((Pick::Heading(*s), out.len() as u16));
                    let name = Span::raw(s.heading()).bold();
                    let name = if empty { name.dark_gray() } else { name };
                    let name = if at(Pick::Heading(*s)) { name.reversed() } else { name };
                    // How full it is, only once that matters.
                    let count = match s.max().saturating_sub(*n) {
                        0 => Span::raw(format!(" {n}/{}", s.max())).yellow(),
                        1 => Span::styled(format!(" {n}/{}", s.max()), quiet),
                        _ => Span::raw(""),
                    };
                    out.push(Line::from(vec![name, count]));
                }
                cairn_view::Line::Bullet(item) => {
                    spots.push((Pick::Bullet(item.id), out.len() as u16));
                    let lead = vec![Span::styled(if item.mine { "  › " } else { "  - " }, quiet)];
                    let stale = item.stale.then(|| Span::raw(" ?").yellow());
                    bullet(
                        &mut out,
                        lead,
                        &item.text,
                        stale.into_iter().collect(),
                        at(Pick::Bullet(item.id)),
                        false,
                        width,
                    );
                    draft(&mut out, Target::Set(item.id));
                }
                cairn_view::Line::Notes(h) => {
                    gap(&mut out);
                    out.push(Line::from(Span::raw(h.to_string()).bold()));
                }
                cairn_view::Line::Note(n) => {
                    for (k, l) in wrap(n, width.saturating_sub(4)).into_iter().enumerate() {
                        out.push(Line::from(Span::styled(
                            format!("{}{l}", if k == 0 { "  - " } else { "    " }),
                            quiet,
                        )));
                    }
                }
            }
            if matches!(part, cairn_view::Line::Heading(s, _) if at(Pick::Heading(*s)))
                || matches!(part, cairn_view::Line::Status(..) if at(Pick::Status))
                || matches!(part, cairn_view::Line::Bullet(i) if at(Pick::Bullet(i.id)))
            {
                // The Status, its heading too, past the blank over it.
                let blank = out.get(from as usize).is_some_and(|l| l.spans.is_empty());
                let from =
                    if matches!(part, cairn_view::Line::Status(..)) && blank { from + 1 } else { from };
                picked = Some((from, out.len().saturating_sub(1) as u16));
            }
        }
        if let Some(s) = open {
            draft(&mut out, Target::Add(s));
        }
        for (part, text) in &doc.rest {
            if *part == Part::Heading {
                out.push(Line::default());
            }
            let indent = text.len() - text.trim_start().len();
            let style = match part {
                Part::Heading | Part::Title => Style::default().add_modifier(Modifier::BOLD),
                Part::Quiet => quiet,
                Part::Fact | Part::Text => Style::default(),
            };
            let text = clean(text);
            if text.width() <= width {
                out.push(Line::from(Span::styled(text, style)));
                continue;
            }
            let rows = wrap(&text, width.saturating_sub(indent).max(10));
            out.extend(
                rows.into_iter()
                    .map(|r| Line::from(Span::styled(format!("{}{r}", " ".repeat(indent)), style))),
            );
        }
        if let Some((heading, all)) = &doc.sessions {
            out.push(Line::default());
            out.push(Line::from(clip(heading, width).bold()));
            for (i, c) in all.iter().enumerate() {
                spots.push((Pick::Session(i), out.len() as u16));
                if hl == Some(Pick::Session(i)) {
                    picked = Some((out.len() as u16, out.len() as u16));
                }
                out.push(session_line(c, hl == Some(Pick::Session(i))));
            }
        }
        (out, picked, spots)
    }

    /// Where the task `r` stands, then how its cairn is used, on one dim
    /// line: its status, what it left running, git, its project by name,
    /// its worktree's branch; when the cairn was updated, how many
    /// sessions it was handed to, and how many lines are stale. What does
    /// not fit is left out.
    fn facts(&self, r: Option<&varde_core::board::Row>, doc: &Doc, width: usize) -> Line<'static> {
        let mut parts = Vec::new();
        if let Some(r) = r {
            parts = look::meta(r, now_ms(), true);
            // How long ago is the board's to say; when it starts, the
            // view's too.
            if r.schedule.is_none() || look::word(r.status).is_some() {
                parts.pop();
            }
            if look::word(r.status).is_none() {
                parts.insert(0, (r.state.clone(), Tone::Quiet));
            }
            if r.inbox {
                parts.push(("in the Inbox".into(), Tone::Quiet));
            }
            if let Some(b) = &doc.worktree {
                parts.push((format!("worktree {b}"), Tone::Quiet));
            }
            if let Some(e) = &r.external {
                parts.push((format!("linked to {e}"), Tone::Quiet));
            }
        }
        parts.extend(
            usage(&doc.usage).into_iter().map(|(p, warn)| (p, if warn { Tone::Edited } else { Tone::Quiet })),
        );
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut room = width;
        for (text, tone) in parts {
            let sep = if spans.is_empty() { "" } else { look::SEP };
            // Only the first is cut to fit; another that does not fit is
            // left out, and a shorter one after it may still fit.
            let text = clean(&text);
            if !spans.is_empty() && sep.width() + text.width() > room {
                continue;
            }
            let text = clip(&text, room.saturating_sub(sep.width()));
            if text.is_empty() || room <= sep.width() {
                break;
            }
            room = room.saturating_sub(sep.width() + text.width());
            spans.push(Span::styled(sep, tone_style(Tone::Quiet)));
            spans.push(Span::styled(text, tone_style(tone)));
        }
        Line::from(spans)
    }

    /// The prompt of the line being typed, under the board: what it
    /// edits, then its words, wrapped under it (at most four rows, the
    /// cursor's in view), and at the right of the last how many words it
    /// has of what it may.
    pub(super) fn line_prompt(&self, width: usize) -> Vec<Line<'static>> {
        let Some(t) = self.view.typing.as_ref() else { return Vec::new() };
        let (label, max) = match t.target {
            Target::Status => ("Status: ".to_string(), MAX_STATUS_WORDS),
            Target::Add(s) => (format!("{} · new line: ", s.heading()), MAX_BULLET_WORDS),
            Target::Set(id) => (format!("Edit line #{id}: "), MAX_BULLET_WORDS),
        };
        let n = t.field.text().split_whitespace().count();
        let count = format!("  {n}/{max}");
        let count = if n > max { count.red() } else { count.dark_gray() };
        let indent = label.width();
        let mut rows = typed_rows(&t.field, width.saturating_sub(indent + 1).max(10));
        // The cursor's row is the one drawn in three parts.
        let at = rows.iter().position(|r| r.spans.len() == 3).unwrap_or(rows.len() - 1);
        let mut rows: Vec<Line<'static>> = rows.drain((at + 1).saturating_sub(4)..).take(4).collect();
        let mut tail = key_spans(&[("RET", "save"), ("Esc", "later")]);
        tail.push(count.clone());
        let last_w = rows.last().map_or(0, Line::width) + indent;
        if last_w + tail.iter().map(Span::width).sum::<usize>() > width {
            tail = vec![count];
        }
        if last_w + tail.iter().map(Span::width).sum::<usize>() > width {
            rows.push(Line::default());
        }
        let last = rows.len() - 1;
        let mut lead = Some(label.bold());
        for (k, row) in rows.iter_mut().enumerate() {
            row.spans.insert(0, lead.take().unwrap_or_else(|| " ".repeat(indent).into()));
            if k == last {
                let used = row.width() + tail.iter().map(Span::width).sum::<usize>();
                row.spans.push(" ".repeat(width.saturating_sub(used)).into());
                row.spans.extend(tail.clone());
            }
        }
        rows
    }

    /// The keys under the view, in `width` columns.
    pub(super) fn view_footer(&self, width: usize) -> Vec<(String, &'static str)> {
        if self.view.picking {
            let shape = if self.project_detail { Shape::Project } else { Shape::Task };
            let mut out = vec![("Add to:".to_string(), "")];
            out.extend(shape.sections().iter().map(|s| (section_key(*s).to_string(), s.heading())));
            if shape == Shape::Task {
                out.push(("s".into(), "Status"));
            }
            out.push(("Esc".into(), "cancel"));
            return out;
        }
        let at = self.view.at;
        let undo = match &self.view.undo {
            Some((w, u)) if Some(w) == self.whom().as_ref() => match u {
                Undo::Line(_) => Some("bring back"),
                Undo::HandBack(_) => Some("undo"),
            },
            _ if self.closed_in_view && !self.closed.is_empty() => Some("reopen"),
            _ => None,
        };
        let marked = self.marked.len().to_string();
        // Each with how much it is needed: 1 most.
        let all = [
            // A reminder: marks are the board's, and the view acts on its task.
            (marked.as_str(), "marked", 0, !self.marked.is_empty()),
            ("RET", if matches!(at, Some(Pick::Session(_))) { "open" } else { "go" }, 1, true),
            ("e", "edit", 1, matches!(at, Some(Pick::Bullet(_) | Pick::Status))),
            ("a", "add", 1, self.whom().is_some()),
            ("x", "remove", 2, matches!(at, Some(Pick::Bullet(_)))),
            ("x", "hand back", 2, at == Some(Pick::Status) && self.view.status_mine()),
            ("u", undo.unwrap_or_default(), 1, undo.is_some()),
            ("[ ]", "tabs", 2, true),
            ("?", "help", 2, true),
            ("q", "back", 3, true),
        ];
        fit(all.iter().filter(|a| a.3).map(|&(k, w, n, _)| (k.to_string(), w, n)).collect(), width)
    }
}

fn shape(w: &Whom) -> Shape {
    if w.project {
        Shape::Project
    } else {
        Shape::Task
    }
}

/// The key that picks a section after `a`.
fn section_key(s: Section) -> char {
    match s {
        Section::Howto => 'h',
        s => s.name().chars().next().unwrap_or(' '),
    }
}

/// A change put away for line `id` of `whom`'s, which is gone: a line to
/// add to its section `s` now.
fn orphan(kept: &mut [Typing], whom: &Whom, id: i64, s: Section) {
    for k in kept.iter_mut().filter(|k| &k.whom == whom && k.target == Target::Set(id)) {
        k.target = Target::Add(s);
    }
}

/// How the cairn is used, each part and whether it is a warning: when it
/// was updated, how many sessions it was handed to, how many lines are
/// stale. Who updated it is `varde show`'s to say.
fn usage(u: &Usage) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if let Some(ts) = u.updated {
        out.push((format!("updated {} ago", age(now_ms() - ts)), false));
        out.push(match (u.sessions, u.handed) {
            (0, _) | (_, None) => ("not handed to a session yet".to_string(), false),
            (1, _) => ("handed to 1 session".to_string(), false),
            (n, _) => (format!("handed to {n} sessions"), false),
        });
    }
    if u.stale > 0 {
        out.push((format!("{} stale", u.stale), true));
    }
    out
}

/// What follows the cairn: the task's context; the rest of what is
/// known of the task is `varde show`'s.
fn rest(d: &varde_core::task::Detail) -> Vec<(Part, String)> {
    let mut out = Vec::new();
    if let Some(b) = d.body.as_deref().filter(|b| !b.trim().is_empty()) {
        out.push((Part::Heading, "Context".into()));
        out.extend(b.lines().map(|l| (Part::Text, format!("  {l}"))));
    }
    out
}

/// A task's sessions, a conversation a line: every one when it repeats,
/// under its `schedule`, or ever ran on one, else the last few. Of a
/// schedule's, those that found nothing are only the newest few: a task
/// every 30 minutes would bury the rest under them.
fn sessions(
    d: varde_core::task::Detail,
    schedule: Option<&str>,
) -> Option<(String, Vec<varde_core::task::Conversation>)> {
    const SESSIONS: usize = 3;
    const QUIET: usize = 10;
    // One its schedule ran keeps them all once the series ends.
    let scheduled = d.runs.iter().any(|r| r.background);
    let mut all = d.conversations;
    if all.is_empty() {
        return None;
    }
    let mut heading = match schedule {
        Some(s) => format!("Sessions · {s}"),
        None if all.len() > SESSIONS && !scheduled => {
            let more = all.len() - SESSIONS;
            all.truncate(SESSIONS);
            format!("Sessions · {more} earlier: varde show {}", d.short)
        }
        None => "Sessions".into(),
    };
    // When each was last resumed says where RET goes among a schedule's
    // many; a task of yours has no such question.
    if schedule.is_none() && !scheduled {
        all.iter_mut().for_each(|c| c.continued = None);
    } else {
        let before = all.len();
        let mut quiet = 0;
        all.retain(|c| {
            if !c.quiet || c.unread || c.status.is_some() || c.continued.is_some() {
                return true;
            }
            quiet += 1;
            quiet <= QUIET
        });
        if all.len() < before {
            heading.push_str(&format!(" · {} earlier found nothing", before - all.len()));
        }
    }
    Some((heading, all))
}

/// `● Thu Oct 15 09:00`: a conversation by when it started, marked while
/// what it found is unread, dim when it found nothing, with a live one's
/// status and when it was last resumed: with nothing unread, RET on the
/// task goes on from the newest.
fn session_line(c: &varde_core::task::Conversation, picked: bool) -> Line<'static> {
    use chrono::{Local, TimeZone};
    let at = |ms: i64| {
        Local
            .timestamp_millis_opt(ms)
            .single()
            .map_or_else(|| ms.to_string(), |t| t.format("%a %b %-d %H:%M").to_string())
    };
    let when = at(c.started);
    let word = match c.status.and_then(look::word) {
        _ if !c.launched => Some(("not launched yet", Tone::Quiet)),
        Some(w) => Some(w),
        None if c.quiet => Some(("quiet", Tone::Quiet)),
        None => None,
    };
    let mut spans = vec![
        if c.unread { Span::styled("  ● ", tone_style(Tone::YourTurn)) } else { Span::raw("    ") },
        if c.quiet { Span::styled(when, tone_style(Tone::Quiet)) } else { Span::raw(when) },
    ];
    if let Some((w, tone)) = word {
        spans.push(Span::styled(format!("  {w}"), tone_style(tone)));
    }
    if let Some((t, how)) = &c.continued {
        let how = if how == "fork" { "forked" } else { "resumed" };
        spans.push(Span::styled(format!("  {how} {}", at(*t)), tone_style(Tone::Quiet)));
    }
    if picked {
        spans[1] = spans[1].clone().reversed();
    }
    Line::from(spans)
}

/// A line [`View::drafts`] wrote: whose, what it is, and its words.
fn draft(d: &Value) -> Option<(Whom, Target, String)> {
    let whom = match (d["task"].as_str(), d["project"].as_str()) {
        (Some(t), _) => Whom { key: t.into(), project: false },
        (_, Some(p)) => Whom { key: p.into(), project: true },
        _ => return None,
    };
    let target = match d["target"].as_str()?.split_once(' ') {
        None if d["target"] == "status" => Target::Status,
        Some(("add", s)) => Target::Add(Section::ALL.into_iter().find(|x| x.name() == s)?),
        Some(("set", id)) => Target::Set(id.parse().ok()?),
        _ => return None,
    };
    Some((whom, target, d["text"].as_str()?.to_string()))
}

/// The row each line that can be picked starts on.
type Spots = Vec<(Pick, u16)>;

/// The line being typed in rows of at most `width` columns, broken after
/// a space where one is, the cursor drawn where it is.
fn typed_rows(field: &Field, width: usize) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthChar;
    let text = field.text();
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut w, mut space) = (0, 0, None);
    for (i, c) in text.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width && i > start {
            let end = space.filter(|&s| s > start).unwrap_or(i);
            rows.push((start, end));
            w = text[end..i].width();
            start = end;
            space = None;
        }
        w += cw;
        if c == ' ' {
            space = Some(i + 1);
        }
    }
    rows.push((start, text.len()));
    let at = field.cursor();
    let last = rows.len() - 1;
    rows.into_iter()
        .enumerate()
        .map(|(k, (from, to))| {
            // The cursor is in the row it starts, or at the end of the last.
            if at < from || at > to || (at == to && k != last) {
                return Line::from(Span::raw(clean(&text[from..to])));
            }
            let under = text[at..to].chars().next();
            let after = at + under.map_or(0, char::len_utf8);
            Line::from(vec![
                Span::raw(clean(&text[from..at])),
                Span::raw(under.map_or(" ".to_string(), |c| clean(&c.to_string()))).reversed(),
                Span::raw(clean(&text[after..to])),
            ])
        })
        .collect()
}

/// What the view shows under its tabs: its lines; the first and last of
/// the one picked, or being typed, to keep in view; and where each line
/// that can be picked is.
pub(super) type Body = (Vec<Line<'static>>, Option<(u16, u16)>, Spots);

/// The sign in a pane's bottom border while there is more below.
pub(super) fn more_below() -> Line<'static> {
    Line::from(" ↓ C-d more ".dark_gray()).right_aligned()
}

/// A line, wrapped under its words (a hanging indent past its `lead`),
/// highlighted or not, and what follows it at its end.
pub(super) fn bullet(
    out: &mut Vec<Line<'static>>,
    lead: Vec<Span<'static>>,
    text: &str,
    after: Vec<Span<'static>>,
    at: bool,
    dim: bool,
    width: usize,
) {
    let indent: usize = lead.iter().map(Span::width).sum();
    let mut lead = Some(lead);
    let mut rows = wrap(&clean(text), width.saturating_sub(indent + 1).max(10));
    if rows.is_empty() {
        rows.push(String::new());
    }
    let last = rows.len() - 1;
    let after_w: usize = after.iter().map(Span::width).sum();
    let fits = rows[last].width() + after_w + indent <= width;
    let base = if dim { Style::default().fg(Color::DarkGray) } else { Style::default() };
    let style = if at { base.add_modifier(Modifier::REVERSED) } else { base };
    for (k, row) in rows.into_iter().enumerate() {
        let mut spans = lead.take().unwrap_or_else(|| vec![Span::raw(" ".repeat(indent))]);
        spans.push(Span::styled(row, style));
        if k == last && fits {
            spans.extend(after.iter().cloned());
        }
        out.push(Line::from(spans));
    }
    if !after.is_empty() && !fits {
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        spans.extend(after);
        out.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use varde_core::task;

    use super::super::draw::render;
    use super::super::tests::{put_cairn, state};
    use super::super::{App, BUSY};
    use crate::{now_ms, Paths};

    fn press(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
        app.refresh().unwrap();
    }

    fn ctrl(app: &mut App, c: char) {
        app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn typed(app: &mut App, text: &str) {
        text.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    /// A board over `paths`, read.
    fn board(paths: &Paths) -> App<'_> {
        let mut app = App::new(paths, None);
        app.refresh().unwrap();
        app
    }

    fn screen(app: &mut App) -> String {
        render(80, 30, |f| app.draw(f))
    }

    /// The line under the board.
    fn bottom(app: &mut App) -> String {
        screen(app).lines().last().unwrap().trim_end().to_string()
    }

    /// A store of its own with a task in `/p`, its cairn `text` if any.
    fn store(text: Option<&str>) -> (tempfile::TempDir, Paths, String) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { home: dir.path().to_path_buf() };
        let store = paths.open(BUSY).unwrap();
        let id = task::create(&store, "One", None, Some("/p"), None, 1).unwrap();
        if let Some(text) = text {
            put_cairn(&store, &id, text);
        }
        (dir, paths, id)
    }

    fn findings(paths: &Paths, id: &str) -> Vec<(i64, String, bool)> {
        let store = paths.open(BUSY).unwrap();
        let c = varde_core::memory::cairn(&store.conn, id).unwrap().map(|(c, _)| c).unwrap_or_default();
        c.findings.into_iter().map(|i| (i.id, i.text, i.mine)).collect()
    }

    #[test]
    fn a_line_is_added_changed_removed_and_brought_back_in_place() {
        let (_dir, paths, id) = store(None);
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        let s = screen(&mut app);
        assert!(s.contains("╭ One  ") && s.contains(" Task   Project ") && s.contains("│ open · p "), "{s}");
        assert!(s.contains("No cairn yet: its first session's update writes one; a adds a line."), "{s}");
        assert!(s.contains("│ Findings ") && !s.contains("0/7") && s.contains("a add"), "{s}");

        press(&mut app, KeyCode::Char('a'));
        assert!(screen(&mut app).contains("f Findings  d Decisions  o Open  s Status  Esc cancel"));
        press(&mut app, KeyCode::Char('f'));
        typed(&mut app, "The cache key omits the locale");
        let line = bottom(&mut app);
        assert!(
            line.starts_with("Findings · new line: The cache key omits the locale")
                && line.ends_with(" 6/40")
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.message, "added to Findings, as yours");
        let (bullet, _, mine) = findings(&paths, &id)[0].clone();
        assert!(mine);
        let s = screen(&mut app);
        assert!(s.contains("updated ") && s.contains("› The cache key omits the locale"), "{s}");

        // Esc keeps what was typed, and the same key on the line takes it up again.
        press(&mut app, KeyCode::Char('e'));
        assert!(bottom(&mut app).starts_with(&format!("Edit line #{bullet}: The cache key")));
        typed(&mut app, " always");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.message, "draft kept for \"One\": e on the line continues it");
        assert_eq!(findings(&paths, &id)[0].1, "The cache key omits the locale");
        press(&mut app, KeyCode::Char('e'));
        assert!(app.message.contains("as you left it"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(findings(&paths, &id)[0].1, "The cache key omits the locale always");

        // `o` adds to the section at point. A refusal leaves the prompt
        // open, saying why over it; cleared, Esc keeps nothing.
        press(&mut app, KeyCode::Char('o'));
        typed(&mut app, "the cache key omits the locale, always");
        press(&mut app, KeyCode::Enter);
        let s = screen(&mut app);
        assert!(s.contains(&format!("a line already says this (line #{bullet})")), "{s}");
        assert!(bottom(&mut app).starts_with("Findings · new line: the cache"), "{s}");
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Esc);
        assert!(app.view.kept.is_empty() && app.message.is_empty(), "{}", app.message);
        assert_eq!(findings(&paths, &id).len(), 1);

        // Removed without asking, and brought back where it was by u.
        press(&mut app, KeyCode::Char('x'));
        assert!(findings(&paths, &id).is_empty());
        assert!(app.message.starts_with("removed: u brings it back"));
        assert!(app.view_footer(80).contains(&("u".into(), "bring back")));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(findings(&paths, &id)[0].0, bullet);

        // X drops the task in view, asking first as x on the board does.
        press(&mut app, KeyCode::Char('X'));
        assert!(matches!(&app.mode, super::super::Mode::Drop(ids) if ids.as_slice() == [id.as_str()]));
        press(&mut app, KeyCode::Esc);

        press(&mut app, KeyCode::Char('q'));
        assert!(!app.detail && !app.quit, "q leaves the view, and only that");
    }

    #[test]
    fn every_line_put_away_is_kept_and_outlives_the_board() {
        let (_dir, paths, _) = store(Some("Status: Looking.\nFindings:\n- The locale matters.\n"));
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('e'));
        typed(&mut app, " a lot");
        press(&mut app, KeyCode::Esc);
        // Another line begun and put away keeps the first.
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('d'));
        typed(&mut app, "Key by locale");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.message, "draft kept for \"One\": a d continues it");
        // Each shows where it goes, dim.
        let s = screen(&mut app);
        assert!(
            s.contains("› The locale matters.")
                && s.contains("draft: The locale matters. a lot · e continues"),
            "{s}"
        );
        let decisions = s.lines().position(|l| l.contains("│ Decisions")).unwrap();
        assert!(s.lines().nth(decisions + 1).unwrap().contains("draft: Key by locale · o continues"), "{s}");
        press(&mut app, KeyCode::Char('e'));
        assert!(bottom(&mut app).contains("The locale matters. a lot"));
        // Quit while typing: both wait for the next board.
        app.put_draft();
        let mut again = board(&paths);
        again.take_draft();
        assert!(
            again.message.starts_with("2 cairn lines you typed are kept, for \"One\" (Decisions, line #"),
            "{}",
            again.message
        );
        assert!(again.message.ends_with("): v shows them"), "{}", again.message);
        press(&mut again, KeyCode::Char('v'));
        press(&mut again, KeyCode::Char('e'));
        assert!(bottom(&mut again).contains("The locale matters. a lot"));
        press(&mut again, KeyCode::Esc);
        press(&mut again, KeyCode::Char('a'));
        press(&mut again, KeyCode::Char('d'));
        assert!(bottom(&mut again).contains("Key by locale"));
    }

    #[test]
    fn x_hands_a_status_you_set_back_and_e_makes_it_yours_again() {
        let (_dir, paths, id) = store(Some("Status: Looking.\nFindings:\n- The locale matters.\n"));
        let mine = || {
            let store = paths.open(BUSY).unwrap();
            let c = varde_core::memory::cairn(&store.conn, &id).unwrap().unwrap().0;
            (c.status, c.status_mine)
        };
        assert_eq!(mine(), ("Looking.".into(), true));
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert!(app.view.at == Some(super::Pick::Status));
        let s = screen(&mut app);
        assert!(s.contains("│   › Looking.") && s.contains("x hand back"), "{s}");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.message, "Status handed back: the next update may change it; u undoes");
        assert_eq!(mine(), ("Looking.".into(), false));
        assert!(screen(&mut app).contains("│     Looking."));
        assert!(app.view_footer(80).contains(&("u".into(), "undo")));
        // u makes it yours again, as it read.
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.message, "Status yours again: updates won't change it");
        assert_eq!(mine(), ("Looking.".into(), true));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.message, "nothing here to undo");
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.message, "the Status is already the updates': e makes it yours");
        press(&mut app, KeyCode::Char('e'));
        typed(&mut app, " Found it.");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.message, "Status set, yours now: updates won't change it; x hands it back");
        assert_eq!(mine(), ("Looking. Found it.".into(), true));
    }

    #[test]
    fn on_a_short_screen_the_view_leaves_the_board_three_rows() {
        let (_dir, paths, _) = store(Some("Status: Looking.\nFindings:\n- The locale matters.\n"));
        let store = paths.open(BUSY).unwrap();
        for t in ["Two", "Three", "Four", "Five"] {
            task::create(&store, t, None, Some("/p"), None, 2).unwrap();
        }
        store
            .conn
            .execute(
                "UPDATE task SET body = ?1",
                [(0..30).map(|i| format!("line {i}\n")).collect::<String>()],
            )
            .unwrap();
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('v'));
        let s = render(80, 24, |f| app.draw(f));
        let top = s.lines().position(|l| l.starts_with("╭")).unwrap();
        assert_eq!(top, 4, "a title and three rows over it:\n{s}");
        // The row over the selection and the one under it.
        let rows: Vec<&str> = s.lines().take(4).collect();
        let at = app.current().unwrap().title.clone();
        assert!(rows[2].contains(&format!(" {at} ")) && rows[3].contains(" One "), "{s}");
        press(&mut app, KeyCode::Char('J'));
        let s = render(80, 24, |f| app.draw(f));
        // Facts and usage on one line, the Status under it.
        let facts = s.lines().nth(top + 2).unwrap();
        assert!(facts.contains("open · p") && facts.contains(" · updated "), "{s}");
        assert!(s.lines().nth(top + 3).unwrap().contains("│ Status"), "{s}");
        // Tall enough, the view is at most 70% of it.
        let s = render(80, 40, |f| app.draw(f));
        assert!(s.lines().position(|l| l.starts_with("╭")).unwrap() > 10, "{s}");
    }

    #[test]
    fn two_boards_drafts_are_merged_not_written_over() {
        let (_dir, paths, _) = store(Some("Status: Looking.\n"));
        let (mut one, mut two) = (board(&paths), board(&paths));
        press(&mut one, KeyCode::Char('v'));
        press(&mut one, KeyCode::Char('e'));
        typed(&mut one, " Still.");
        press(&mut two, KeyCode::Char('n'));
        typed(&mut two, "A new one");
        two.put_draft();
        one.put_draft();
        let mut three = board(&paths);
        three.take_draft();
        assert!(three.message.contains("new task is kept") && three.message.contains("cairn line"));
    }

    #[test]
    fn a_change_to_a_line_gone_is_a_line_to_add() {
        let (_dir, paths, id) = store(Some("Status: s\nFindings:\n- First.\n- Second.\n"));
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('e'));
        typed(&mut app, " More.");
        press(&mut app, KeyCode::Esc);
        // Removed here: what was typed for it waits under `a f`.
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('f'));
        assert!(bottom(&mut app).starts_with("Findings · new line: First. More."));
        press(&mut app, KeyCode::Esc);

        // Removed elsewhere, while the draft waited for the next board.
        press(&mut app, KeyCode::Char('e'));
        typed(&mut app, " Too.");
        app.put_draft();
        let second = findings(&paths, &id)[0].0;
        let change = varde_core::memory::Change::Remove(second);
        let owner = varde_core::memory::Owner::Task(&id);
        varde_core::memory::change(&mut paths.open(BUSY).unwrap(), owner, change, now_ms()).unwrap();
        let mut again = board(&paths);
        again.take_draft();
        press(&mut again, KeyCode::Char('v'));
        // Two lines to add to Findings now: one after the other.
        for words in ["First. More.", "Second. Too."] {
            press(&mut again, KeyCode::Char('a'));
            press(&mut again, KeyCode::Char('f'));
            assert!(bottom(&mut again).contains(words), "{}", bottom(&mut again));
            press(&mut again, KeyCode::Esc);
        }
    }

    #[test]
    fn the_view_keeps_its_keys_and_the_board_has_the_rest() {
        let full = (1..=7).map(|i| format!("- Finding number {i}.\n")).collect::<String>();
        let (_dir, paths, first) = store(Some(&format!("Status: s\nFindings:\n{full}")));
        task::create(&paths.open(BUSY).unwrap(), "Two", None, Some("/p"), None, 2).unwrap();
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.current_id().as_deref(), Some(first.as_str()));
        press(&mut app, KeyCode::Char('v'));
        let s = screen(&mut app);
        assert!(s.contains("Finding number 1") && s.contains("Findings 7/7") && s.contains("? help"), "{s}");

        // A full section says so at once.
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('f'));
        assert!(app.message.starts_with("Findings is full (7/7)"), "{}", app.message);
        press(&mut app, KeyCode::Char('o'));
        assert!(app.message.starts_with("Findings is full"), "o on a finding: {}", app.message);

        // gg and G pick the first and last lines; J and K change the task.
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert!(app.view.at == Some(super::Pick::Status));
        press(&mut app, KeyCode::Char('G'));
        assert!(app.view.at == Some(super::Pick::Heading(varde_core::cairn::Section::Open)));
        press(&mut app, KeyCode::Char('K'));
        assert!(app.detail && app.current().unwrap().title == "Two");
        assert!(screen(&mut app).contains("No cairn yet"), "the other task's cairn");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.current_id().as_deref(), Some(first.as_str()));

        // The board's keys do what they do on the board, for the task in
        // view and not the marked ones; Esc unmarks first.
        press(&mut app, KeyCode::Char('s'));
        assert!(app.message.contains("needs tmux"), "{}", app.message);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.message, "nothing needs you");
        let two = app.rows().find(|r| r.title == "Two").unwrap().task.clone();
        app.marked = vec![two];
        assert!(app.view_footer(80).starts_with(&[("1".into(), "marked")]));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(state(&paths, &first), "done");
        assert_eq!(state(&paths, &app.marked[0]), "open", "marks are the board's");
        assert!(app.detail, "the view stays");
        assert_eq!(app.current().unwrap().title, "Two", "on the task still open");
        press(&mut app, KeyCode::Esc);
        assert!(app.marked.is_empty() && app.detail);
        press(&mut app, KeyCode::Esc);
        assert!(!app.detail && !app.quit);
    }

    #[test]
    fn an_empty_cairn_is_its_hint_and_headings_and_e_on_one_adds() {
        let (_dir, paths, _) = store(None);
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        let s = screen(&mut app);
        assert!(s.contains("No cairn yet") && !s.contains("Status") && !s.contains("none yet"), "{s}");
        // As tall as it needs: the board above it in view.
        let top = s.lines().position(|l| l.starts_with("╭")).unwrap();
        assert!(top > 15, "{s}");
        press(&mut app, KeyCode::Char('e'));
        assert!(bottom(&mut app).starts_with("Findings · new line: "), "{}", bottom(&mut app));
        press(&mut app, KeyCode::Esc);
    }

    #[test]
    fn the_view_says_when_there_is_more_below_and_g_goes_to_its_end() {
        let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let (_dir, paths, id) = store(Some("Status: Looking.\nFindings:\n- The locale matters.\n"));
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET body = ?1", [&body]).unwrap();
        // A session that ended: one line, with no more than that.
        store
            .conn
            .execute(
                "INSERT INTO run (id, task, agent, native_id, relation, started, launch) VALUES ('R1', ?1, 'claude', 'n', 'fresh', 1, 'managed')",
                [&id],
            )
            .unwrap();
        for (kind, ts) in [("started", 2), ("ended", 3)] {
            store
                .conn
                .execute(
                    "INSERT INTO event (run, kind, ts, raw) VALUES ('R1', ?1, ?2, '{}')",
                    rusqlite::params![kind, ts],
                )
                .unwrap();
        }
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        let s = screen(&mut app);
        assert!(s.contains("↓ C-d more ╯") && !s.contains("line 39"), "{s}");
        press(&mut app, KeyCode::Char('G'));
        let s = screen(&mut app);
        assert!(s.contains("line 39") && !s.contains("C-d more"), "{s}");
        // An ended session is when it started, and nothing more.
        let session = s.lines().skip_while(|l| !l.contains("Sessions")).nth(1).unwrap_or_default();
        use chrono::TimeZone;
        let when = chrono::Local.timestamp_millis_opt(1).unwrap().format("%a %b %-d %H:%M").to_string();
        assert_eq!(session.trim_matches(['│', ' ']), when, "{s}");
    }

    #[test]
    fn j_and_k_in_the_view_go_over_tasks_opening_a_folded_group() {
        let (_dir, paths, first) = store(None);
        let store = paths.open(BUSY).unwrap();
        let later = task::create(&store, "Later", None, Some("/p"), None, 2).unwrap();
        store.conn.execute("UPDATE task SET state = 'parked' WHERE id = ?1", [&later]).unwrap();
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.current_id().as_deref(), Some(later.as_str()), "into Parked, opened");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.current_id().as_deref(), Some(later.as_str()), "the last stays");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(app.current_id().as_deref(), Some(first.as_str()));
    }

    #[test]
    fn u_in_the_view_reopens_a_task_closed_there_once_its_lines_are_back() {
        let (_dir, paths, id) = store(Some("Status: Looking.\nFindings:\n- The locale matters.\n"));
        let mut app = board(&paths);
        press(&mut app, KeyCode::Char('v'));
        for _ in 0..5 {
            screen(&mut app);
            if matches!(app.view.at, Some(super::Pick::Bullet(_))) {
                break;
            }
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('x'));
        assert!(findings(&paths, &id).is_empty());
        // The close is newer than the line: u reopens it, as it says.
        press(&mut app, KeyCode::Char('d'));
        assert!(app.message.ends_with("is done; u reopens"), "{}", app.message);
        assert!(app.view_footer(80).contains(&("u".into(), "reopen")));
        press(&mut app, KeyCode::Char('u'));
        assert!(app.message.starts_with("reopened"), "{}", app.message);
        assert_eq!(state(&paths, &id), "open");
        assert!(app.detail && app.current_id().as_deref() == Some(id.as_str()));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.message, "nothing here to undo");

        // Closed on the board, the view leaves it to the board's u.
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('P'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.message, "nothing here to undo");
    }
}
