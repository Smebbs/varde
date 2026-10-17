//! The new-task form.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use serde_json::{json, Value};
use varde_core::title::clean;

use super::draw::left_clip;
use super::field::{Field, Words};

/// A field of the form.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub(super) enum Part {
    #[default]
    Title,
    /// The directory of a new task's project; a task made has one.
    Project,
    When,
    Body,
}

/// A new task being written: a short title, and as much context as it
/// needs. Or a task already made, its title and context being changed.
#[derive(Default, Clone)]
pub(super) struct Draft {
    pub(super) title: Field,
    pub(super) project: Field,
    /// When varde starts it, if at all: a schedule's rule.
    pub(super) when: Field,
    pub(super) body: Field,
    /// The field being typed in.
    pub(super) at: Part,
    /// The task this changes; a new one when `None`.
    pub(super) task: Option<String>,
    /// C-x was pressed: C-e next opens the editor, as in a shell.
    pub(super) prefix: bool,
    /// Taken up again from where it was put away: emptied, Esc says it
    /// is dropped.
    pub(super) restored: bool,
}

pub(super) enum Edit {
    Typing,
    Create,
    Cancel,
    /// Write it in `$EDITOR` instead.
    Editor,
    /// Alt-Tab on the Project: pick its directory.
    Pick,
}

/// The hint at the top of the file a draft is written in.
const HINT: &str = "#: ";

/// The line between the fields and the context, as org-varde's form draws it.
const RULE: &str = "── context ──────────────────────────────";

impl Draft {
    /// A new task, in the project `project`, opened in its context: most
    /// tasks are only that, and varde names them from it.
    pub(super) fn new(project: &str) -> Draft {
        Draft { project: Field::new(project), at: Part::Body, ..Draft::default() }
    }

    /// A task's title and context, to change.
    pub(super) fn of(task: &str, title: &str, body: &str, when: &str) -> Draft {
        Draft {
            title: Field::new(title),
            when: Field::new(when),
            body: Field::new(body),
            task: Some(task.to_string()),
            ..Draft::default()
        }
    }

    /// Whether anything has been typed.
    pub(super) fn is_empty(&self) -> bool {
        self.title.text().trim().is_empty() && self.body.text().trim().is_empty()
    }

    /// The draft as the drafts file keeps it.
    pub(super) fn json(&self) -> Value {
        let target = if self.task.is_some() { "edit task" } else { "new task" };
        json!({ "task": self.task, "target": target, "title": self.title.text(), "text": self.body.text(),
                "when": self.when.text(), "project": self.project.text() })
    }

    /// A draft [`json`](Self::json) wrote; `None` for another kind.
    pub(super) fn of_json(d: &Value) -> Option<Draft> {
        let task = match d["target"].as_str()? {
            "new task" => None,
            "edit task" => Some(d["task"].as_str()?.to_string()),
            _ => return None,
        };
        let (title, body) = (d["title"].as_str()?, d["text"].as_str()?);
        let mut draft = Draft { task, ..Draft::of("", title, body, d["when"].as_str().unwrap_or_default()) };
        if draft.task.is_none() {
            draft.project.set(d["project"].as_str().unwrap_or_default());
            draft.at = Part::Body;
        }
        Some(draft)
    }

    /// The fields, in order: a task made keeps its project here.
    fn parts(&self) -> &'static [Part] {
        match self.task {
            None => &[Part::Title, Part::Project, Part::When, Part::Body],
            Some(_) => &[Part::Title, Part::When, Part::Body],
        }
    }

    /// Tab goes to the next field, round from the body to the title, and
    /// Shift-Tab back, as org-varde's form; Alt-Tab (Esc Tab, as Emacs's
    /// M-TAB) on the Project picks its directory. Down and Up go through
    /// the fields too. Enter creates from a field above the
    /// line and starts a new line in the body; C-s creates from any.
    /// Esc and C-g put the form away, and C-x C-e or Alt-e opens the
    /// draft in `$EDITOR`. The rest edits the text as every prompt of the
    /// board does.
    pub(super) fn key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if std::mem::take(&mut self.prefix) && ctrl && key.code == KeyCode::Char('e') {
            return Edit::Editor;
        }
        let project = self.at == Part::Project;
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('g') if ctrl => return Edit::Cancel,
            KeyCode::Char('s') if ctrl => return Edit::Create,
            KeyCode::Char('x') if ctrl => self.prefix = true,
            KeyCode::Char('e') if alt => return Edit::Editor,
            KeyCode::Enter if self.at != Part::Body => return Edit::Create,
            KeyCode::Enter => self.body.insert("\n"),
            KeyCode::Tab if alt && project => return Edit::Pick,
            KeyCode::Tab => self.step(1),
            KeyCode::BackTab => self.step(-1),
            KeyCode::Down if self.at != Part::Body => self.step(1),
            KeyCode::Up if self.at != Part::Title && (self.at != Part::Body || self.body.on_first_line()) => {
                self.step(-1)
            }
            _ => {
                let words = if project { Words::Path } else { Words::Prose };
                self.field().key(key, words);
            }
        }
        Edit::Typing
    }

    /// Go `n` fields on, round the ends.
    fn step(&mut self, n: isize) {
        let parts = self.parts();
        let i = parts.iter().position(|p| *p == self.at).unwrap_or(0) as isize;
        self.at = parts[(i + n).rem_euclid(parts.len() as isize) as usize];
    }

    /// The field being typed in.
    fn field(&mut self) -> &mut Field {
        match self.at {
            Part::Title => &mut self.title,
            Part::Project => &mut self.project,
            Part::When => &mut self.when,
            Part::Body => &mut self.body,
        }
    }

    /// Pasted text is typed, never run as keys. Lines after the first of a
    /// paste into the title go to the body; a field above the line takes
    /// one.
    pub(super) fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.at {
            Part::Body => self.body.insert(&text),
            Part::Project => self.project.insert(text.lines().next().unwrap_or_default().trim()),
            Part::When => self.when.insert(text.lines().next().unwrap_or_default()),
            Part::Title => {
                let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
                self.title.insert(first);
                if !rest.trim().is_empty() {
                    let body = self.body.text();
                    let gap = if !body.is_empty() && !body.ends_with('\n') { "\n" } else { "" };
                    self.body.set(&format!("{body}{gap}{rest}"));
                    self.at = Part::Body;
                }
            }
        }
    }

    /// The draft as a file to edit, as the form shows it: its fields
    /// above the line, the context below.
    pub(super) fn text(&self) -> String {
        let hint = [
            "The fields are above the line, and the context for the agent below it.",
            "Left empty, the Title is made from the context.",
            "Lines starting with \"#:\" are left out. Save and quit to go back to the form.",
        ];
        let mut text: String = hint.iter().map(|l| format!("{HINT}{l}\n")).collect();
        text += &format!("Title: {}\n", self.title.text());
        if self.task.is_none() {
            text += &format!("Project: {}\n", self.project.text());
        }
        text += &format!("When: {}\n{RULE}\n{}\n", self.when.text(), self.body.text().trim_end());
        text
    }

    /// Take back the draft as [`text`](Self::text) wrote it: the fields
    /// its first lines name, up to the line; the rest is the context.
    pub(super) fn set_text(&mut self, text: &str) {
        let mut lines = text.lines().filter(|l| !l.starts_with(HINT)).peekable();
        while let Some(line) = lines.peek() {
            let field = match line.split_once(':') {
                Some(("Title", v)) => Some((&mut self.title, v)),
                Some(("Project", v)) if self.task.is_none() => Some((&mut self.project, v)),
                Some(("When", v)) => Some((&mut self.when, v)),
                _ => None,
            };
            match field {
                Some((field, value)) => field.set(value.trim()),
                None if line.trim().is_empty() => {}
                None if line.starts_with('─') => {
                    lines.next();
                    break;
                }
                None => break,
            }
            lines.next();
        }
        self.body.set(lines.collect::<Vec<_>>().join("\n").trim());
    }
}

/// The new-task form, over the bottom of the board, a pane as the view
/// is. The fields stay put and the body scrolls to keep the cursor in
/// view. An empty field says what it is for, as org-varde's does.
pub(super) fn draw_draft(f: &mut Frame, draft: &Draft, area: Rect) {
    let width = area.width.saturating_sub(4).max(1) as usize;
    let body = draft.body.text();
    let rows: usize = body.split('\n').map(|l| l.chars().count() / width + 1).sum();
    let fields = draft.parts().len() - 1;
    // Six rows where the screen has them; a smaller one gets all it has.
    let height = (rows + fields + 3).max(fields + 5).min(area.height as usize) as u16;
    let area = Rect { y: area.bottom() - height, height, ..area };
    let name = if draft.task.is_some() { " Edit task " } else { " New task " };
    let block =
        Block::bordered().border_type(BorderType::Rounded).padding(Padding::horizontal(1)).title(name);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let room = width.saturating_sub(10);
    for (i, part) in draft.parts()[..fields].iter().enumerate().take(inner.height as usize) {
        let (label, field, hint) = match part {
            Part::Title => (
                "Title:   ",
                &draft.title,
                if draft.task.is_none() { "left empty, varde names it from the context" } else { "" },
            ),
            Part::Project => ("Project: ", &draft.project, "asked for on create, or M-Tab picks it"),
            Part::When => {
                ("When:    ", &draft.when, "not scheduled: at mon 09:00 · every weekday 09:00 · every 4h")
            }
            Part::Body => unreachable!("the body is below the line"),
        };
        let mut line = vec![label.bold()];
        if draft.at == *part {
            line.extend(field.line_spans(room));
        } else {
            line.push(Span::raw(left_clip(&clean(field.text()), room)));
        }
        if field.text().is_empty() {
            line.push(hint.dark_gray().italic());
        }
        let row = Rect { y: inner.y + i as u16, height: 1, ..inner };
        f.render_widget(Paragraph::new(Line::from(line)), row);
    }
    let [rule, body_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(Rect {
        y: inner.y + fields as u16,
        height: inner.height.saturating_sub(fields as u16),
        ..inner
    });
    let line: String = format!("── context {}", "─".repeat(width)).chars().take(width).collect();
    f.render_widget(Paragraph::new(line.dark_gray()), rule);

    let in_body = draft.at == Part::Body;
    if body.is_empty() && !in_body {
        f.render_widget(Paragraph::new("context for the agent".dark_gray()), body_area);
        return;
    }
    // The cursor's row, as wrapped, kept in view.
    let before = &body[..draft.body.cursor()];
    let at: usize = before.split('\n').map(|l| l.chars().count() / width + 1).sum::<usize>() - 1;
    let scroll = (at + 1).saturating_sub(body_area.height as usize) as u16;
    let lines = draft.body.lines(in_body);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), body_area);
}

#[cfg(test)]
mod tests {
    use super::super::draw::render;
    use super::*;

    fn press(draft: &mut Draft, code: KeyCode) -> Edit {
        draft.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(draft: &mut Draft, text: &str) {
        text.chars().for_each(|c| {
            press(draft, KeyCode::Char(c));
        });
    }

    #[test]
    fn a_new_task_opens_in_its_context() {
        let mut d = Draft::new("~/src/app");
        assert_eq!(d.at, Part::Body);
        typed(&mut d, "line one");
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Typing));
        typed(&mut d, "line two");
        press(&mut d, KeyCode::Backspace);
        assert_eq!(d.body.text(), "line one\nline tw");
        assert_eq!((d.title.text(), d.project.text()), ("", "~/src/app"));
        assert!(matches!(d.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)), Edit::Create));
        assert_eq!(d.body.text(), "line one\nline tw", "Ctrl-S is not typed");
    }

    #[test]
    fn tab_goes_between_fields_and_alt_tab_picks_the_project() {
        let mut d = Draft::new("");
        press(&mut d, KeyCode::Tab);
        assert_eq!(d.at, Part::Title, "round from the context to the title");
        typed(&mut d, "t");
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Create));
        press(&mut d, KeyCode::Tab);
        assert_eq!(d.at, Part::Project);
        assert!(matches!(d.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT)), Edit::Pick));
        typed(&mut d, "~/x/");
        d.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(d.project.text(), "~/", "C-w rubs out a directory");
        press(&mut d, KeyCode::Tab);
        assert_eq!(d.at, Part::When);
        press(&mut d, KeyCode::BackTab);
        press(&mut d, KeyCode::BackTab);
        press(&mut d, KeyCode::BackTab);
        assert_eq!(d.at, Part::Body, "Shift-Tab goes back, round to the context");
        assert!(matches!(press(&mut d, KeyCode::Esc), Edit::Cancel));

        // A task made keeps its project: no field for it.
        let mut e = Draft::of("t1", "Title", "", "");
        assert_eq!(e.at, Part::Title, "an edit opens on the title");
        press(&mut e, KeyCode::Tab);
        assert_eq!(e.at, Part::When);
        assert!(matches!(e.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT)), Edit::Typing));
    }

    #[test]
    fn ctrl_w_rubs_out_a_word_and_ctrl_u_the_line() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut d = Draft::default();
        typed(&mut d, "Fix the  login");
        d.key(ctrl('w'));
        assert_eq!(d.title.text(), "Fix the  ");
        d.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(d.title.text(), "Fix ");
        press(&mut d, KeyCode::BackTab);
        typed(&mut d, "one");
        press(&mut d, KeyCode::Enter);
        typed(&mut d, "two words");
        d.key(ctrl('u'));
        assert_eq!(d.body.text(), "one\n");
        d.key(ctrl('w'));
        assert_eq!(d.body.text(), "one", "at a line's start, the line break goes");
        d.key(ctrl('u'));
        assert_eq!(d.body.text(), "");
        assert!(matches!(d.key(ctrl('g')), Edit::Cancel), "C-g puts it away, as everywhere");
        assert!(matches!(d.key(ctrl('x')), Edit::Typing));
        assert!(matches!(d.key(ctrl('e')), Edit::Editor), "C-x C-e, as in a shell");
        assert!(matches!(d.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT)), Edit::Editor));
    }

    #[test]
    fn a_draft_goes_to_the_editor_and_comes_back() {
        let mut d = Draft::new("~/src/app");
        d.body.set("It fails.\n");
        let text = d.text();
        assert!(text.starts_with(HINT), "{text}");
        assert!(
            text.ends_with(
                "Title: \nProject: ~/src/app\nWhen: \n── context ──────────────────────────────\nIt fails.\n"
            ),
            "{text}"
        );
        // An empty title stays empty: the context is not taken for it.
        d.set_text(&text);
        assert_eq!((d.title.text(), d.project.text(), d.body.text()), ("", "~/src/app", "It fails."));
        assert_eq!(d.at, Part::Body);
        d.set_text(
            &text
                .replace("Title: ", "Title: Fix the login")
                .replace("It fails.\n", "It fails.\n\n    indented()\n\n"),
        );
        assert_eq!(d.title.text(), "Fix the login");
        assert_eq!(d.body.text(), "It fails.\n\n    indented()");
        // The line rubbed out: the fields named first are still the fields.
        d.set_text("#: hint\nTitle: Just a title\nWhen: every 4h\nThe context\n");
        assert_eq!(
            (d.title.text(), d.when.text(), d.body.text()),
            ("Just a title", "every 4h", "The context")
        );
        // An edit has no Project to change.
        let mut e = Draft::of("t1", "Old", "body", "");
        assert!(!e.text().contains("Project:"));
        e.set_text("Title: New\nProject: /elsewhere\n──\nbody\n");
        assert_eq!((e.title.text(), e.project.text()), ("New", ""));
    }

    #[test]
    fn a_paste_into_the_title_puts_its_other_lines_in_the_body() {
        let mut d = Draft::default();
        d.paste("Fix the login\r\nIt fails on Safari.\r\nSee the log.");
        assert_eq!(d.title.text(), "Fix the login");
        assert_eq!(d.body.text(), "It fails on Safari.\nSee the log.");
        assert_eq!(d.at, Part::Body);
        d.paste("\nqdpx");
        assert_eq!(d.body.text(), "It fails on Safari.\nSee the log.\nqdpx", "typed, never run");
    }

    #[test]
    fn the_form_shows_title_and_body_and_follows_the_typing() {
        let mut d = Draft::default();
        typed(&mut d, "Board git status");
        let screen = |d: &Draft| render(40, 12, |f| draw_draft(f, d, f.area()));
        let s = screen(&d);
        assert!(s.contains("Title:   Board git status "), "{s}");
        assert!(s.contains("Project: asked for on create"), "an empty field says what it is for:\n{s}");
        // Past the box's width, the title scrolls to keep the typing in view.
        let mut long = Draft::default();
        typed(&mut long, "A title far longer than this narrow box ends here");
        let s = screen(&long);
        assert!(s.contains("Title:   …") && s.contains("box ends here"), "{s}");
        assert!(s.contains("context for the agent"), "{s}");
        press(&mut d, KeyCode::BackTab);
        for i in 0..15 {
            typed(&mut d, &format!("line {i}"));
            press(&mut d, KeyCode::Enter);
        }
        typed(&mut d, "the end");
        let s = screen(&d);
        assert!(s.contains("Title:   Board git status"), "the title stays:\n{s}");
        assert!(s.contains("the end"), "the cursor stays in view:\n{s}");
        // Up to the middle, and the cursor's line is still in view.
        for _ in 0..8 {
            press(&mut d, KeyCode::Up);
        }
        press(&mut d, KeyCode::Home);
        typed(&mut d, ">");
        assert!(screen(&d).contains(">line 7"), "{}", screen(&d));
        for _ in 0..8 {
            press(&mut d, KeyCode::Up);
        }
        assert_eq!(d.at, Part::When, "Up from the first line goes above the line");
        assert!(render(60, 12, |f| draw_draft(f, &Draft::new(""), f.area()))
            .contains("Title:   left empty, varde names it"));
    }
}
