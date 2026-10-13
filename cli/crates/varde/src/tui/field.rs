//! Text being typed into the board: one editor for every prompt, so a
//! key does the same in each. The keys are readline's (and so Emacs's
//! and macOS's text fields'): arrows, C-a/C-e and Home/End, C-b/C-f,
//! Alt-b/Alt-f and Ctrl-arrows by word, Backspace and Delete, C-w and
//! Alt-Backspace, C-u, C-k and Alt-d to rub out, and C-y to put back what
//! was last rubbed out. C-w rubs out to the blank before the cursor, as
//! readline's does, and Alt-Backspace to the start of the word.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use varde_core::title::clean;

/// What C-w and Alt-Backspace rub out: in prose, back to a blank and to
/// a word's start; in a directory prompt the last directory, as fish
/// does.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Words {
    Prose,
    Path,
}

#[derive(Default, Clone)]
pub(super) struct Field {
    text: String,
    /// The cursor, a byte offset on a char boundary.
    at: usize,
    /// What was last rubbed out, for C-y.
    killed: String,
}

impl Field {
    pub(super) fn new(text: &str) -> Field {
        Field { text: text.to_string(), at: text.len(), killed: String::new() }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// Replace the text, the cursor at its end.
    pub(super) fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.at = self.text.len();
    }

    /// Where the cursor is, as a byte offset.
    pub(super) fn cursor(&self) -> usize {
        self.at
    }

    /// Whether the cursor is on the first line.
    pub(super) fn on_first_line(&self) -> bool {
        !self.text[..self.at].contains('\n')
    }

    pub(super) fn insert(&mut self, s: &str) {
        self.text.insert_str(self.at, s);
        self.at += s.len();
    }

    /// Edit with `key`; whether it was an editing key. Enter, Tab and Esc
    /// are the prompt's own; Up and Down move between lines only in text
    /// that has them.
    pub(super) fn key(&mut self, key: KeyEvent, words: Words) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left if ctrl || alt => self.at = self.word_back(Words::Prose),
            KeyCode::Right if ctrl || alt => self.at = self.word_forward(),
            KeyCode::Left => self.at = self.prev(self.at),
            KeyCode::Right => self.at = self.next(self.at),
            KeyCode::Char('b') if ctrl => self.at = self.prev(self.at),
            KeyCode::Char('f') if ctrl => self.at = self.next(self.at),
            KeyCode::Char('b') if alt => self.at = self.word_back(Words::Prose),
            KeyCode::Char('f') if alt => self.at = self.word_forward(),
            KeyCode::Home => self.at = self.line_start(),
            KeyCode::End => self.at = self.line_end(),
            KeyCode::Char('a') if ctrl => self.at = self.line_start(),
            KeyCode::Char('e') if ctrl => self.at = self.line_end(),
            KeyCode::Up if self.text.contains('\n') => self.line(false),
            KeyCode::Down if self.text.contains('\n') => self.line(true),
            KeyCode::Backspace if ctrl || alt => self.kill(self.word_back(words), self.at),
            KeyCode::Char('w') if ctrl => self.kill(self.blank_back(words), self.at),
            KeyCode::Char('d') if alt => self.kill(self.at, self.word_forward()),
            KeyCode::Char('u') if ctrl => {
                // At a line's start, the line break before it.
                let from = if self.at == self.line_start() { self.prev(self.at) } else { self.line_start() };
                self.kill(from, self.at)
            }
            KeyCode::Char('k') if ctrl => {
                let to = if self.at == self.line_end() { self.next(self.at) } else { self.line_end() };
                self.kill(self.at, to)
            }
            KeyCode::Char('y') if ctrl => {
                let killed = self.killed.clone();
                self.insert(&killed);
            }
            KeyCode::Backspace => {
                let from = self.prev(self.at);
                self.text.replace_range(from..self.at, "");
                self.at = from;
            }
            KeyCode::Delete => {
                let to = self.next(self.at);
                self.text.replace_range(self.at..to, "");
            }
            KeyCode::Char('d') if ctrl => {
                let to = self.next(self.at);
                self.text.replace_range(self.at..to, "");
            }
            // AltGr comes as Ctrl+Alt on some terminals: a character all the same.
            KeyCode::Char(c) if ctrl == alt => {
                let mut b = [0; 4];
                self.insert(c.encode_utf8(&mut b));
            }
            _ => return false,
        }
        true
    }

    fn kill(&mut self, from: usize, to: usize) {
        if from < to {
            self.killed = self.text[from..to].to_string();
            self.text.replace_range(from..to, "");
            self.at = from;
        }
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.text[..self.at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.at..].find('\n').map_or(self.text.len(), |i| self.at + i)
    }

    /// The start of the word before the cursor, past the blanks after it;
    /// for a path, of the last directory, past its slash.
    fn word_back(&self, words: Words) -> usize {
        let before = &self.text[..self.at];
        if words == Words::Path {
            return before.trim_end_matches('/').rfind(['/', ' ']).map_or(0, |i| i + 1);
        }
        // At a line's start, Backspace's step: the line break.
        if before.ends_with('\n') {
            return self.prev(self.at);
        }
        let blank = before.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
        before[..blank].trim_end_matches(char::is_alphanumeric).len()
    }

    /// What C-w rubs out back to: the blank before the cursor, past the
    /// blanks after it; for a path, as [`word_back`](Self::word_back).
    fn blank_back(&self, words: Words) -> usize {
        let before = &self.text[..self.at];
        if words == Words::Path || before.ends_with('\n') {
            return self.word_back(words);
        }
        let word = before.trim_end_matches([' ', '\t']).len();
        before[..word].rfind(char::is_whitespace).map_or(0, |i| i + 1)
    }

    fn word_forward(&self) -> usize {
        let after = &self.text[self.at..];
        let word = after.find(char::is_alphanumeric).unwrap_or(after.len());
        let end = after[word..].find(|c: char| !c.is_alphanumeric()).map_or(after.len(), |i| word + i);
        self.at + end
    }

    /// To the line above or below, as near the same column as it has.
    fn line(&mut self, down: bool) {
        let start = self.line_start();
        let column = self.text[start..self.at].chars().count();
        let target = if down {
            match self.text[self.at..].find('\n') {
                Some(i) => self.at + i + 1,
                None => return,
            }
        } else if start == 0 {
            return;
        } else {
            self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
        };
        let end = self.text[target..].find('\n').map_or(self.text.len(), |i| target + i);
        self.at = self.text[target..end].char_indices().nth(column).map_or(end, |(i, _)| target + i);
    }

    /// The text as lines to draw, the cursor shown on the character under
    /// it when `focused`.
    pub(super) fn lines(&self, focused: bool) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        let mut from = 0;
        for line in self.text.split('\n') {
            let to = from + line.len();
            let mut spans = Vec::new();
            if focused && (from..=to).contains(&self.at) {
                let at = self.at - from;
                let under = line[at..].chars().next();
                let after = at + under.map_or(0, char::len_utf8);
                spans.push(Span::raw(clean(&line[..at])));
                spans.push(Span::raw(under.map_or(" ".to_string(), |c| clean(&c.to_string()))).reversed());
                spans.push(Span::raw(clean(&line[after..])));
            } else {
                spans.push(Span::raw(clean(line)));
            }
            out.push(Line::from(spans));
            from = to + 1;
        }
        out
    }

    /// One line of it, clipped from the left to keep the cursor in `room`
    /// columns: what a prompt shows.
    pub(super) fn line_spans(&self, room: usize) -> Vec<Span<'static>> {
        use unicode_width::UnicodeWidthChar;
        let chars: Vec<char> = self.text.chars().collect();
        let at = self.text[..self.at].chars().count();
        let w = |c: &char| c.width().unwrap_or(0);
        // Columns, not characters: a wide one takes two. Keep the cursor,
        // and one column after it, in view.
        let mut skip = 0;
        while skip < at && chars[skip..at].iter().map(w).sum::<usize>() + 2 > room.max(2) {
            skip += 1;
        }
        let mut spans = Vec::new();
        if skip > 0 {
            spans.push(Span::raw("…"));
            skip += 1;
        }
        let before: String = chars[skip.min(at)..at].iter().collect();
        let under = chars.get(at).map_or(" ".to_string(), |c| c.to_string());
        let after: String = chars[(at + 1).min(chars.len())..].iter().collect();
        spans.push(Span::raw(clean(&before)));
        spans.push(Span::raw(clean(&under)).reversed());
        spans.push(Span::raw(clean(&after)));
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(f: &mut Field, code: KeyCode, mods: KeyModifiers) {
        f.key(KeyEvent::new(code, mods), Words::Prose);
    }

    fn ctrl(f: &mut Field, c: char) {
        press(f, KeyCode::Char(c), KeyModifiers::CONTROL);
    }

    fn typed(f: &mut Field, s: &str) {
        s.chars().for_each(|c| press(f, KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn the_cursor_moves_and_typing_goes_where_it_is() {
        let mut f = Field::default();
        typed(&mut f, "Fix login");
        ctrl(&mut f, 'a');
        typed(&mut f, "Please ");
        press(&mut f, KeyCode::End, KeyModifiers::NONE);
        press(&mut f, KeyCode::Left, KeyModifiers::NONE);
        press(&mut f, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(f.text(), "Please Fix logn");
        press(&mut f, KeyCode::Left, KeyModifiers::ALT);
        press(&mut f, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(f.text(), "Please Fix ogn");
        press(&mut f, KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(f.text(), "Please Fix ogn", "Alt and a letter types nothing");
        press(&mut f, KeyCode::Home, KeyModifiers::NONE);
        press(&mut f, KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(f.text(), " Fix ogn");
    }

    #[test]
    fn what_is_rubbed_out_comes_back_with_ctrl_y() {
        let mut f = Field::default();
        typed(&mut f, "keep these words");
        ctrl(&mut f, 'u');
        assert_eq!(f.text(), "");
        ctrl(&mut f, 'y');
        assert_eq!(f.text(), "keep these words");
        ctrl(&mut f, 'w');
        assert_eq!(f.text(), "keep these ");
        press(&mut f, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(f.text(), "keep ");
        // C-w goes back to a blank, Alt-Backspace to a word's start.
        f.set("see auth/session.go now");
        ctrl(&mut f, 'w');
        ctrl(&mut f, 'w');
        assert_eq!(f.text(), "see ");
        f.set("see auth/session.go");
        press(&mut f, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(f.text(), "see auth/session.");
        f.set("keep ");
        ctrl(&mut f, 'a');
        ctrl(&mut f, 'k');
        assert_eq!(f.text(), "");
        ctrl(&mut f, 'y');
        assert_eq!(f.text(), "keep ");
    }

    #[test]
    fn lines_are_moved_between_and_joined_at_their_ends() {
        let mut f = Field::new("one\ntwo three\nx");
        press(&mut f, KeyCode::Up, KeyModifiers::NONE);
        typed(&mut f, "!");
        assert_eq!(f.text(), "one\nt!wo three\nx");
        press(&mut f, KeyCode::Up, KeyModifiers::NONE);
        ctrl(&mut f, 'e');
        ctrl(&mut f, 'k');
        assert_eq!(f.text(), "onet!wo three\nx", "C-k at a line's end joins the next");
        press(&mut f, KeyCode::Down, KeyModifiers::NONE);
        ctrl(&mut f, 'a');
        ctrl(&mut f, 'u');
        assert_eq!(f.text(), "onet!wo threex", "C-u at a line's start joins the one above");
        ctrl(&mut f, 'w');
        assert_eq!(f.text(), "onet!wo x");
    }

    #[test]
    fn a_path_is_rubbed_out_a_directory_at_a_time() {
        let mut f = Field::new("~/projects/varde/");
        f.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL), Words::Path);
        assert_eq!(f.text(), "~/projects/");
        f.set("~/projects/va");
        f.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT), Words::Path);
        assert_eq!(f.text(), "~/projects/");
    }

    #[test]
    fn the_cursor_is_drawn_on_the_character_it_is_on() {
        let mut f = Field::new("abc");
        press(&mut f, KeyCode::Left, KeyModifiers::NONE);
        let spans = f.line_spans(80);
        let shown: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(shown, ["ab", "c", ""]);
        let f = Field::new("a long line of text");
        let shown: String = f.line_spans(8).iter().map(|s| s.content.to_string()).collect();
        assert!(shown.starts_with('…') && shown.ends_with("text "), "{shown}");
        // Wide characters count two columns: the cursor stays in view.
        let f = Field::new("日本語のタイトルです");
        let shown: String = f.line_spans(8).iter().map(|s| s.content.to_string()).collect();
        assert!(
            unicode_width::UnicodeWidthStr::width(shown.as_str()) <= 9 && shown.ends_with("す "),
            "{shown}"
        );
        let f = Field::new("é\nü");
        let lines = f.lines(true);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans[1].content, " ");
    }
}
