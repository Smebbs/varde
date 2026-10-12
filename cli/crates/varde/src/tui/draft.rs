//! The new-task form.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use varde_core::title::clean;

use super::draw::left_clip;

/// A new task being written: a short title, and as much context as it
/// needs. Or a task already made, its title and context being changed.
#[derive(Default)]
pub(super) struct Draft {
    pub(super) title: String,
    pub(super) body: String,
    pub(super) in_body: bool,
    /// The task this changes; a new one when `None`.
    pub(super) task: Option<String>,
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
    /// Tab moves between title and body. Enter creates from the title and
    /// starts a new line in the body; Ctrl-S creates from either, and
    /// Ctrl-G opens the draft in `$EDITOR`. Ctrl-W and Alt-Backspace rub
    /// out a word, Ctrl-U the line.
    pub(super) fn key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let field = if self.in_body { &mut self.body } else { &mut self.title };
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('s') if ctrl => return Edit::Create,
            KeyCode::Char('g') if ctrl => return Edit::Editor,
            KeyCode::Enter if !self.in_body => return Edit::Create,
            KeyCode::Enter => field.push('\n'),
            KeyCode::Tab | KeyCode::BackTab => self.in_body = !self.in_body,
            KeyCode::Char('w') if ctrl => rub_word(field),
            KeyCode::Backspace if alt || ctrl => rub_word(field),
            KeyCode::Char('u') if ctrl => field.truncate(field.rfind('\n').map_or(0, |i| i + 1)),
            KeyCode::Backspace => {
                field.pop();
            }
            KeyCode::Char(c) if !ctrl => field.push(c),
            _ => {}
        }
        Edit::Typing
    }

    /// Pasted text is typed, never run as keys. Lines after the first of a
    /// paste into the title go to the body.
    pub(super) fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.in_body {
            self.body.push_str(&text);
            return;
        }
        let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
        self.title.push_str(first);
        if !rest.trim().is_empty() {
            if !self.body.is_empty() && !self.body.ends_with('\n') {
                self.body.push('\n');
            }
            self.body.push_str(rest);
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
        format!("{hint}{}\n\n{}\n", self.title, self.body.trim_end())
    }

    /// Take back the draft as [`text`](Self::text) wrote it.
    pub(super) fn set_text(&mut self, text: &str) {
        let text: String = text.split_inclusive('\n').filter(|l| !l.starts_with(HINT)).collect();
        let text = text.trim_start();
        let (title, body) = text.split_once('\n').unwrap_or((text, ""));
        self.title = title.trim().to_string();
        self.body = body.trim().to_string();
        self.in_body = !self.body.is_empty();
    }
}

/// Rub out the word before the end of `s`, and the blanks after it; at
/// the start of a line, the line break.
fn rub_word(s: &mut String) {
    let kept = s.trim_end_matches([' ', '\t']).trim_end_matches(|c: char| !c.is_whitespace()).len();
    if kept < s.len() {
        s.truncate(kept);
    } else {
        s.pop();
    }
}

/// The new-task form, over the bottom of the board. The title stays put
/// and the body scrolls to keep its end, where you type, in view.
pub(super) fn draw_draft(f: &mut Frame, draft: &Draft, area: Rect) {
    let width = area.width.saturating_sub(2).max(1) as usize;
    let rows: usize = draft.body.split('\n').map(|l| l.chars().count() / width + 1).sum();
    // Six rows where the screen has them; a smaller one gets all it has.
    let height = (rows + 4).max(6).min(area.height as usize) as u16;
    let area = Rect { y: area.bottom() - height, height, ..area };
    let name = if draft.task.is_some() { " Edit task " } else { " New task " };
    let block = Block::default().title(name).borders(Borders::ALL);
    let [title, rule, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
            .areas(block.inner(area));
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let cursor = |on: bool| if on { "▏" } else { "" };
    // A title wider than the box shows its end, where the typing is.
    let shown = left_clip(&clean(&draft.title), width.saturating_sub(8));
    let line = Line::from(vec!["Title: ".bold(), Span::raw(shown), cursor(!draft.in_body).into()]);
    f.render_widget(Paragraph::new(line), title);
    f.render_widget(Paragraph::new("─".repeat(width).dark_gray()), rule);

    if draft.body.is_empty() && !draft.in_body {
        f.render_widget(Paragraph::new("Tab to add context for the agent".dark_gray()), body);
        return;
    }
    let mut lines: Vec<Line> = draft.body.split('\n').map(|l| Line::from(clean(l))).collect();
    if let Some(last) = lines.last_mut().filter(|_| draft.in_body) {
        last.push_span(cursor(true));
    }
    let scroll = rows.saturating_sub(body.height as usize) as u16;
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), body);
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
        press(&mut d, KeyCode::Tab);
        typed(&mut d, "line one");
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Typing));
        typed(&mut d, "line two");
        press(&mut d, KeyCode::Backspace);
        assert_eq!(d.title, "Short title");
        assert_eq!(d.body, "line one\nline tw");
        assert!(matches!(d.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)), Edit::Create));
        assert_eq!(d.body, "line one\nline tw", "Ctrl-S is not typed");
    }

    #[test]
    fn enter_on_the_title_creates_and_esc_cancels() {
        let mut d = Draft::default();
        typed(&mut d, "t");
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Create));
        press(&mut d, KeyCode::Tab);
        press(&mut d, KeyCode::Tab);
        assert!(matches!(press(&mut d, KeyCode::Enter), Edit::Create), "Tab goes back to the title");
        assert!(matches!(press(&mut d, KeyCode::Esc), Edit::Cancel));
    }

    #[test]
    fn ctrl_w_rubs_out_a_word_and_ctrl_u_the_line() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut d = Draft::default();
        typed(&mut d, "Fix the  login");
        d.key(ctrl('w'));
        assert_eq!(d.title, "Fix the  ");
        d.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(d.title, "Fix ");
        press(&mut d, KeyCode::Tab);
        typed(&mut d, "one");
        press(&mut d, KeyCode::Enter);
        typed(&mut d, "two words");
        d.key(ctrl('u'));
        assert_eq!(d.body, "one\n");
        d.key(ctrl('w'));
        assert_eq!(d.body, "one", "at a line's start, the line break goes");
        d.key(ctrl('u'));
        assert_eq!(d.body, "");
        assert!(matches!(d.key(ctrl('g')), Edit::Editor));
    }

    #[test]
    fn a_draft_goes_to_the_editor_and_comes_back() {
        let mut d =
            Draft { title: "Fix the login".into(), body: "It fails.\n".into(), in_body: false, task: None };
        let text = d.text();
        assert!(text.starts_with(HINT), "{text}");
        assert!(text.ends_with("Fix the login\n\nIt fails.\n"), "{text}");
        d.set_text(&format!("{text}\n    indented()\n\n"));
        assert_eq!(d.title, "Fix the login");
        assert_eq!(d.body, "It fails.\n\n    indented()");
        assert!(d.in_body);
        // Only a title, and no blank line after it.
        d.set_text("#: hint\n\nJust a title\n");
        assert_eq!((d.title.as_str(), d.body.as_str(), d.in_body), ("Just a title", "", false));
    }

    #[test]
    fn a_paste_into_the_title_puts_its_other_lines_in_the_body() {
        let mut d = Draft::default();
        d.paste("Fix the login\r\nIt fails on Safari.\r\nSee the log.");
        assert_eq!(d.title, "Fix the login");
        assert_eq!(d.body, "It fails on Safari.\nSee the log.");
        assert!(d.in_body);
        d.paste("\nqdpx");
        assert_eq!(d.body, "It fails on Safari.\nSee the log.\nqdpx", "typed, never run");
    }

    #[test]
    fn the_form_shows_title_and_body_and_follows_the_typing() {
        let mut d = Draft::default();
        typed(&mut d, "Board git status");
        let screen = |d: &Draft| render(40, 12, |f| draw_draft(f, d, f.area()));
        let s = screen(&d);
        assert!(s.contains("Title: Board git status▏"), "{s}");
        // Past the box's width, the title scrolls to keep the typing in view.
        let mut long = Draft::default();
        typed(&mut long, "A title far longer than this narrow box ends here");
        let s = screen(&long);
        assert!(s.contains("Title: …") && s.contains("box ends here▏"), "{s}");
        assert!(s.contains("Tab to add context"), "{s}");
        press(&mut d, KeyCode::Tab);
        for i in 0..15 {
            typed(&mut d, &format!("line {i}"));
            press(&mut d, KeyCode::Enter);
        }
        typed(&mut d, "the end");
        let s = screen(&d);
        assert!(s.contains("Title: Board git status"), "the title stays:\n{s}");
        assert!(s.contains("the end▏"), "the cursor stays in view:\n{s}");
    }
}
