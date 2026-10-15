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

/// A new task being written: a short title, and as much context as it
/// needs. Or a task already made, its title and context being changed.
#[derive(Default, Clone)]
pub(super) struct Draft {
    pub(super) title: Field,
    /// When varde starts it, if at all: a schedule's rule.
    pub(super) when: Field,
    pub(super) body: Field,
    pub(super) in_body: bool,
    pub(super) in_when: bool,
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
}

/// The hint at the top of the file a draft is written in.
const HINT: &str = "#: ";

impl Draft {
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
                "when": self.when.text() })
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
        draft.in_body = !body.is_empty();
        Some(draft)
    }

    /// Tab goes to the next field, round from the body to the title, and
    /// Shift-Tab back, as org-varde's form; Down and Up go through them too.
    /// Enter creates from the title or the when and starts a new line in the body;
    /// C-s creates from either. Esc and C-g put the form away, and C-x C-e
    /// or Alt-e opens the draft in `$EDITOR`. The rest edits the text as
    /// every prompt of the board does.
    pub(super) fn key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if std::mem::take(&mut self.prefix) && ctrl && key.code == KeyCode::Char('e') {
            return Edit::Editor;
        }
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('g') if ctrl => return Edit::Cancel,
            KeyCode::Char('s') if ctrl => return Edit::Create,
            KeyCode::Char('x') if ctrl => self.prefix = true,
            KeyCode::Char('e') if alt => return Edit::Editor,
            KeyCode::Enter if !self.in_body => return Edit::Create,
            KeyCode::Enter => self.body.insert("\n"),
            KeyCode::Tab => self.go((self.at() + 1) % 3),
            KeyCode::BackTab => self.go((self.at() + 2) % 3),
            KeyCode::Down if !self.in_body => self.go(self.at() + 1),
            KeyCode::Up if self.in_when || (self.in_body && self.body.on_first_line()) => {
                self.go(self.at() - 1)
            }
            _ => {
                let field = if self.in_body {
                    &mut self.body
                } else if self.in_when {
                    &mut self.when
                } else {
                    &mut self.title
                };
                field.key(key, Words::Prose);
            }
        }
        Edit::Typing
    }

    /// Which field is being typed in: 0 the title, 1 when, 2 the body.
    fn at(&self) -> usize {
        if self.in_body {
            2
        } else {
            usize::from(self.in_when)
        }
    }

    /// Go to field `i`.
    fn go(&mut self, i: usize) {
        self.in_when = i == 1;
        self.in_body = i == 2;
    }

    /// Pasted text is typed, never run as keys. Lines after the first of a
    /// paste into the title go to the body.
    pub(super) fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.in_body {
            self.body.insert(&text);
            return;
        }
        if self.in_when {
            self.when.insert(text.lines().next().unwrap_or_default());
            return;
        }
        let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
        self.title.insert(first);
        if !rest.trim().is_empty() {
            let body = self.body.text();
            let gap = if !body.is_empty() && !body.ends_with('\n') { "\n" } else { "" };
            self.body.set(&format!("{body}{gap}{rest}"));
            self.in_body = true;
        }
    }

    /// The draft as a file to edit: the title on its first line, then a
    /// blank line and the body.
    pub(super) fn text(&self) -> String {
        let hint = [
            "The first line is the title, and the rest is context for the agent.",
            "Lines starting with \"#:\" are left out. Save and quit to go back to the form.",
        ];
        let hint: String = hint.iter().map(|l| format!("{HINT}{l}\n")).collect();
        format!("{hint}{}\n\n{}\n", self.title.text(), self.body.text().trim_end())
    }

    /// Take back the draft as [`text`](Self::text) wrote it.
    pub(super) fn set_text(&mut self, text: &str) {
        let text: String = text.split_inclusive('\n').filter(|l| !l.starts_with(HINT)).collect();
        let text = text.trim_start();
        let (title, body) = text.split_once('\n').unwrap_or((text, ""));
        self.title.set(title.trim());
        self.body.set(body.trim());
        self.in_body = !self.body.text().is_empty();
        self.in_when = false;
    }
}

/// The new-task form, over the bottom of the board, a pane as the view
/// is. The title stays put and the body scrolls to keep the cursor in
/// view.
pub(super) fn draw_draft(f: &mut Frame, draft: &Draft, area: Rect) {
    let width = area.width.saturating_sub(4).max(1) as usize;
    let body = draft.body.text();
    let rows: usize = body.split('\n').map(|l| l.chars().count() / width + 1).sum();
    // Six rows where the screen has them; a smaller one gets all it has.
    let height = (rows + 5).max(7).min(area.height as usize) as u16;
    let area = Rect { y: area.bottom() - height, height, ..area };
    let name = if draft.task.is_some() { " Edit task " } else { " New task " };
    let block =
        Block::bordered().border_type(BorderType::Rounded).padding(Padding::horizontal(1)).title(name);
    let [title, when, rule, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(block.inner(area));
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let mut line = vec!["Title: ".bold()];
    if draft.in_body || draft.in_when {
        line.push(Span::raw(left_clip(&clean(draft.title.text()), width.saturating_sub(8))));
    } else {
        line.extend(draft.title.line_spans(width.saturating_sub(8)));
    }
    f.render_widget(Paragraph::new(Line::from(line)), title);
    let mut line = vec!["When:  ".bold()];
    if draft.in_when {
        line.extend(draft.when.line_spans(width.saturating_sub(8)));
    } else if draft.when.text().is_empty() {
        line.push("not scheduled: at mon 09:00 · every weekday 09:00 · every 4h".dark_gray());
    } else {
        line.push(Span::raw(left_clip(&clean(draft.when.text()), width.saturating_sub(8))));
    }
    f.render_widget(Paragraph::new(Line::from(line)), when);
    f.render_widget(Paragraph::new("─".repeat(width).dark_gray()), rule);

    if body.is_empty() && !draft.in_body {
        f.render_widget(Paragraph::new("context for the agent".dark_gray()), body_area);
        return;
    }
    // The cursor's row, as wrapped, kept in view.
    let before = &body[..draft.body.cursor()];
    let at: usize = before.split('\n').map(|l| l.chars().count() / width + 1).sum::<usize>() - 1;
    let scroll = (at + 1).saturating_sub(body_area.height as usize) as u16;
    let lines = draft.body.lines(draft.in_body);
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
    fn a_draft_takes_a_title_and_a_multiline_body() {
        let mut d = Draft::default();
        typed(&mut d, "Short title");
        press(&mut d, KeyCode::BackTab);
        typed(&mut d, "line one");
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Typing));
        typed(&mut d, "line two");
        press(&mut d, KeyCode::Backspace);
        assert_eq!(d.title.text(), "Short title");
        assert_eq!(d.body.text(), "line one\nline tw");
        assert!(matches!(d.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)), Edit::Create));
        assert_eq!(d.body.text(), "line one\nline tw", "Ctrl-S is not typed");
    }

    #[test]
    fn enter_on_the_title_creates_and_esc_cancels() {
        let mut d = Draft::default();
        typed(&mut d, "t");
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Create));
        press(&mut d, KeyCode::Tab);
        assert!(d.in_when);
        press(&mut d, KeyCode::Tab);
        assert!(d.in_body);
        press(&mut d, KeyCode::Tab);
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Create), "Tab goes round to the title");
        press(&mut d, KeyCode::BackTab);
        assert!(d.in_body, "Shift-Tab goes back, round to the body");
        press(&mut d, KeyCode::BackTab);
        assert!(d.in_when);
        assert!(matches!(press(&mut d, KeyCode::Esc), Edit::Cancel));
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
        let mut d = Draft::default();
        d.title.set("Fix the login");
        d.body.set("It fails.\n");
        let text = d.text();
        assert!(text.starts_with(HINT), "{text}");
        assert!(text.ends_with("Fix the login\n\nIt fails.\n"), "{text}");
        d.set_text(&format!("{text}\n    indented()\n\n"));
        assert_eq!(d.title.text(), "Fix the login");
        assert_eq!(d.body.text(), "It fails.\n\n    indented()");
        assert!(d.in_body);
        // Only a title, and no blank line after it.
        d.set_text("#: hint\n\nJust a title\n");
        assert_eq!((d.title.text(), d.body.text(), d.in_body), ("Just a title", "", false));
    }

    #[test]
    fn a_paste_into_the_title_puts_its_other_lines_in_the_body() {
        let mut d = Draft::default();
        d.paste("Fix the login\r\nIt fails on Safari.\r\nSee the log.");
        assert_eq!(d.title.text(), "Fix the login");
        assert_eq!(d.body.text(), "It fails on Safari.\nSee the log.");
        assert!(d.in_body);
        d.paste("\nqdpx");
        assert_eq!(d.body.text(), "It fails on Safari.\nSee the log.\nqdpx", "typed, never run");
    }

    #[test]
    fn the_form_shows_title_and_body_and_follows_the_typing() {
        let mut d = Draft::default();
        typed(&mut d, "Board git status");
        let screen = |d: &Draft| render(40, 12, |f| draw_draft(f, d, f.area()));
        let s = screen(&d);
        assert!(s.contains("Title: Board git status "), "{s}");
        // Past the box's width, the title scrolls to keep the typing in view.
        let mut long = Draft::default();
        typed(&mut long, "A title far longer than this narrow box ends here");
        let s = screen(&long);
        assert!(s.contains("Title: …") && s.contains("box ends here"), "{s}");
        assert!(s.contains("context for the agent"), "{s}");
        press(&mut d, KeyCode::BackTab);
        for i in 0..15 {
            typed(&mut d, &format!("line {i}"));
            press(&mut d, KeyCode::Enter);
        }
        typed(&mut d, "the end");
        let s = screen(&d);
        assert!(s.contains("Title: Board git status"), "the title stays:\n{s}");
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
        assert!(!d.in_body, "Up from the first line goes to the title");
    }
}
