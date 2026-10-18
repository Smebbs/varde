//! The project picker: every prompt for a project is this one, as
//! org-varde's is its `completing-read`. The projects varde knows are
//! listed by name and directory, the likeliest first and picked; typing
//! narrows them, and a directory typed (`~`, `/` or `.` first) lists
//! the directories in it instead, Tab completing it as a shell does.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;
use varde_core::title::clean;

use super::draw::{left_clip, tilde};
use super::field::{Field, Words};
use super::home_in;
use crate::list::{pad, project_name};

/// The last row, which takes a directory to be typed.
pub(super) const OTHER: &str = "another directory…";

/// The directories in the one `line` names up to its last `/`, whose
/// names start as its last part does; hidden ones once a `.` is typed.
fn dirs_in(line: &str) -> Vec<String> {
    let line = if line == "~" { "~/" } else { line };
    let (parent, stem) = line.rsplit_once('/').map_or(("", line), |(p, s)| (p, s));
    let Ok(entries) = std::fs::read_dir(format!("{}/", home_in(parent))) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(stem) && (stem.starts_with('.') || !n.starts_with('.')))
        .collect();
    names.sort();
    names.into_iter().map(|n| format!("{parent}/{n}/")).collect()
}

/// `line` completed as a shell completes a directory: as far as the
/// directories it could name agree, closed with a `/` when only one does.
fn complete(line: &str) -> String {
    let dirs = dirs_in(line);
    let Some(first) = dirs.first() else { return line.to_string() };
    let common = dirs.iter().fold(first.as_str(), |a, b| {
        let end = a
            .char_indices()
            .zip(b.chars())
            .find(|((_, x), y)| x != y)
            .map_or(a.len().min(b.len()), |((i, _), _)| i);
        &a[..end]
    });
    if common.len() > line.len() {
        common.to_string()
    } else {
        line.to_string()
    }
}

/// The most rows of choices shown at once.
const ROWS: usize = 8;

pub(super) struct Pick {
    /// What it is asked for, its box's title.
    pub(super) title: &'static str,
    /// The projects offered, likeliest first, as absolute directories.
    known: Vec<String>,
    /// The project the task is in, said of its row and picked last.
    now: Option<String>,
    pub(super) line: Field,
    /// The row picked, of [`rows`](Self::rows).
    at: usize,
    /// The directories in the one typed, while one is.
    dirs: Vec<String>,
}

/// What a key did.
pub(super) enum Picked {
    Typing,
    /// A directory, as typed or `~`-shortened.
    Dir(String),
    Back,
}

impl Pick {
    /// Offering `known`, the first picked unless it is `now`, the
    /// project the task is in already.
    pub(super) fn new(title: &'static str, known: Vec<String>, now: Option<&str>) -> Pick {
        let now = now.map(String::from);
        let mut known: Vec<String> = known.into_iter().filter(|p| Some(p) != now.as_ref()).collect();
        known.extend(now.clone());
        Pick { title, known, now, line: Field::default(), at: 0, dirs: Vec::new() }
    }

    /// Starting with `text` typed.
    pub(super) fn typed(mut self, text: &str) -> Pick {
        self.line.set(text);
        self.retype();
        self
    }

    /// Whether a directory is being typed, rather than a project's name.
    fn is_path(&self) -> bool {
        self.line.text().starts_with(['~', '/', '.'])
    }

    /// The projects the line names, best first: its name starting with
    /// what is typed, then having it, then the directory having it, then
    /// those its letters are in, in order.
    fn matching(&self) -> Vec<&String> {
        let q = self.line.text().trim().to_lowercase();
        let rank = |p: &String| {
            let (name, dir) = (project_name(Some(p)).to_lowercase(), tilde(p).to_lowercase());
            let mut letters = dir.chars();
            match () {
                _ if name.starts_with(&q) => Some(0),
                _ if name.contains(&q) => Some(1),
                _ if dir.contains(&q) => Some(2),
                _ if q.chars().filter(|c| !c.is_whitespace()).all(|c| letters.any(|d| d == c)) => Some(3),
                _ => None,
            }
        };
        let mut ranked: Vec<(usize, &String)> =
            self.known.iter().filter_map(|p| Some((rank(p)?, p))).collect();
        ranked.sort_by_key(|(r, _)| *r);
        ranked.into_iter().map(|(_, p)| p).collect()
    }

    /// The rows: the projects matching and [`OTHER`] last, or the
    /// directories in the one typed.
    fn rows(&self) -> Vec<String> {
        match self.is_path() {
            true => self.dirs.clone(),
            false => self.matching().into_iter().cloned().chain([OTHER.to_string()]).collect(),
        }
    }

    /// The line changed: the first row picked, and a directory typed listed.
    fn retype(&mut self) {
        self.at = 0;
        self.dirs = if self.is_path() { dirs_in(self.line.text()) } else { Vec::new() };
    }

    /// Up and Down, C-p and C-n, go between the rows; RET picks the row,
    /// or takes the directory typed; Tab puts the row in the line, a
    /// project's directory to go into, or completes the directory typed;
    /// Esc and C-g go back. The rest edits the line.
    pub(super) fn key(&mut self, key: KeyEvent) -> Picked {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.rows();
        let n = rows.len();
        // A directory typed has no row picked until one is put in the line.
        let unpicked = self.is_path() && rows.get(self.at).map(String::as_str) != Some(self.line.text());
        match key.code {
            KeyCode::Esc => return Picked::Back,
            KeyCode::Char('g') if ctrl => return Picked::Back,
            KeyCode::Down | KeyCode::Char('n') if n > 0 && (ctrl || key.code == KeyCode::Down) => {
                self.at = if unpicked { self.at } else { (self.at + 1) % n };
                self.follow(&rows);
            }
            KeyCode::Up | KeyCode::Char('p') if n > 0 && (ctrl || key.code == KeyCode::Up) => {
                self.at = if unpicked { n - 1 } else { (self.at + n - 1) % n };
                self.follow(&rows);
            }
            KeyCode::Enter if self.is_path() => return Picked::Dir(self.line.text().trim().to_string()),
            KeyCode::Enter => match rows.get(self.at) {
                Some(r) if r != OTHER => return Picked::Dir(r.clone()),
                _ => self.go_to("~/"),
            },
            KeyCode::Tab if self.is_path() => {
                let text = complete(self.line.text());
                if text != self.line.text() {
                    self.go_to(&text);
                } else if n > 0 {
                    // As far as they agree already: Tab steps through them.
                    self.at = if unpicked { self.at } else { (self.at + 1) % n };
                    self.follow(&rows);
                }
            }
            KeyCode::Tab => match rows.get(self.at) {
                Some(r) if r != OTHER => self.go_to(&format!("{}/", tilde(r))),
                _ => self.go_to("~/"),
            },
            _ => {
                if self.line.key(key, Words::Path) {
                    self.retype();
                }
            }
        }
        Picked::Typing
    }

    /// The line is now `dir`, and what is in it listed.
    fn go_to(&mut self, dir: &str) {
        self.line.set(dir);
        self.retype();
    }

    /// Stepping through the directories in one typed puts each in the line.
    fn follow(&mut self, rows: &[String]) {
        if self.is_path() {
            let at = self.at;
            self.line.set(&rows[at]);
        }
    }

    pub(super) fn paste(&mut self, text: &str) {
        self.line.insert(text.lines().next().unwrap_or_default().trim());
        self.retype();
    }

    /// The picker, at the bottom of `area`: the line, and under it the
    /// rows around the one picked, each project by name and directory.
    pub(super) fn draw(&self, f: &mut Frame, area: Rect) {
        let rows = self.rows();
        let shown = rows.len().clamp(1, ROWS);
        let height = (shown as u16 + 3).min(area.height);
        let area = Rect { y: area.bottom() - height, height, ..area };
        let count = if self.is_path() {
            String::new()
        } else {
            format!(" {}/{} ", self.matching().len(), self.known.len())
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .padding(Padding::horizontal(1))
            .title(format!(" {} ", self.title))
            .title_bottom(Line::from(count.dark_gray()).right_aligned());
        let inner = block.inner(area);
        f.render_widget(Clear, area);
        f.render_widget(block, area);
        let width = inner.width as usize;

        let mut line = vec!["› ".bold()];
        line.extend(self.line.line_spans(width.saturating_sub(2)));
        if self.line.text().is_empty() {
            line.push("type to narrow, or a directory: ~/…".dark_gray().italic());
        }
        f.render_widget(Paragraph::new(Line::from(line)), Rect { height: inner.height.min(1), ..inner });

        let from = (self.at + 1).saturating_sub(shown);
        let names = rows.iter().filter(|r| *r != OTHER).map(|r| project_name(Some(r)).width()).max();
        let column = names.unwrap_or(0).min(width / 3) + 2;
        let mut lines: Vec<Line> = Vec::new();
        for (i, r) in rows.iter().enumerate().skip(from).take(shown) {
            // A directory typed is what RET takes: one is picked once in the line.
            let picked = i == self.at && (!self.is_path() || self.line.text() == r);
            let mark = if picked { "▌".into() } else { Span::raw(" ") };
            let mut spans = vec![mark];
            if r == OTHER {
                spans.push(OTHER.italic());
            } else if self.is_path() {
                let name = r.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
                spans.push(Span::raw(left_clip(&clean(&format!("{name}/")), width.saturating_sub(1))));
            } else {
                let name = clean(project_name(Some(r)));
                spans.push(pad(&name, column).bold());
                let mut dir = tilde(r);
                if self.now.as_deref() == Some(r.as_str()) {
                    dir.push_str("  · its project now");
                }
                spans.push(left_clip(&clean(&dir), width.saturating_sub(column + 1)).dark_gray());
            }
            let mut l = Line::from(spans);
            if picked {
                l = l.reversed();
            }
            lines.push(l);
        }
        if rows.is_empty() {
            lines.push(Line::from(" no directory there: RET takes it as typed".dark_gray().italic()));
        }
        let list = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        f.render_widget(Paragraph::new(lines), list);
    }

    /// The keys, for the line under it.
    pub(super) fn keys(&self) -> [(&'static str, &'static str); 4] {
        let tab = if self.is_path() { "complete" } else { "into its directory" };
        [("↑/↓", "choose"), ("RET", "pick"), ("Tab", tab), ("Esc", "back")]
    }

    /// The directory picked, with `~` expanded.
    pub(super) fn expand(dir: &str) -> String {
        home_in(dir).trim_end_matches('/').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::super::draw::render;
    use super::*;

    fn press(p: &mut Pick, code: KeyCode) -> Picked {
        p.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(p: &mut Pick, text: &str) {
        text.chars().for_each(|c| {
            press(p, KeyCode::Char(c));
        });
    }

    fn known() -> Vec<String> {
        ["/src/web", "/src/api", "/work/varde", "/src/webhooks"].map(String::from).to_vec()
    }

    #[test]
    fn the_likeliest_is_picked_and_typing_narrows_by_name_first() {
        let mut p = Pick::new("Project", known(), None);
        assert!(matches!(press(&mut p, KeyCode::Enter), Picked::Dir(d) if d == "/src/web"), "RET: the first");
        typed(&mut p, "w");
        let names =
            |p: &Pick| p.matching().iter().map(|d| project_name(Some(d)).to_string()).collect::<Vec<_>>();
        assert_eq!(names(&p), ["web", "webhooks", "varde"], "the name first, then the directory");
        typed(&mut p, "ebh");
        assert_eq!(names(&p), ["webhooks"]);
        p.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        typed(&mut p, "vd");
        assert_eq!(names(&p), ["varde"], "letters in order");
        p.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        typed(&mut p, "src");
        assert_eq!(p.matching().len(), 3, "by directory too");
        press(&mut p, KeyCode::Down);
        p.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert!(matches!(press(&mut p, KeyCode::Enter), Picked::Dir(d) if d == "/src/webhooks"));
        p.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(matches!(press(&mut p, KeyCode::Enter), Picked::Dir(d) if d == "/src/api"));
        assert!(matches!(press(&mut p, KeyCode::Esc), Picked::Back));
    }

    #[test]
    fn its_project_now_is_offered_last() {
        let p = Pick::new("Move to project", known(), Some("/src/web"));
        assert_eq!(p.rows()[0], "/src/api", "another picked first");
        assert_eq!(p.rows()[3], "/src/web");
        let s = render(70, 12, |f| p.draw(f, f.area()));
        assert!(s.contains("/src/web  · its project now"), "{s}");
    }

    #[test]
    fn a_directory_is_typed_listed_and_completed() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["alpha/one", "alpha/two", "beta"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let root = dir.path().display().to_string();
        let mut p = Pick::new("Project", vec![], None);
        assert_eq!(p.rows(), [OTHER], "nothing known: a directory to type");
        assert!(matches!(press(&mut p, KeyCode::Enter), Picked::Typing));
        assert_eq!(p.line.text(), "~/", "RET on it starts one");
        p.line.set("");
        p.paste(&format!("{root}/"));
        assert_eq!(p.rows(), [format!("{root}/alpha/"), format!("{root}/beta/")]);
        typed(&mut p, "a");
        press(&mut p, KeyCode::Tab);
        assert_eq!(p.line.text(), format!("{root}/alpha/"), "one match, closed with a slash");
        assert_eq!(p.rows(), [format!("{root}/alpha/one/"), format!("{root}/alpha/two/")]);
        press(&mut p, KeyCode::Tab);
        assert_eq!(p.line.text(), format!("{root}/alpha/one/"), "Tab steps through them");
        press(&mut p, KeyCode::Down);
        assert_eq!(p.line.text(), format!("{root}/alpha/two/"), "and Down");
        assert!(
            matches!(press(&mut p, KeyCode::Enter), Picked::Dir(d) if Pick::expand(&d) == format!("{root}/alpha/two"))
        );
        // Tab on a project goes into its directory.
        let mut q = Pick::new("Project", vec![root.clone()], None);
        press(&mut q, KeyCode::Tab);
        assert_eq!(q.line.text(), format!("{}/", tilde(&root)));
        assert_eq!(q.rows().len(), 2, "{:?}", q.rows());
    }

    #[test]
    fn tab_completes_a_directory_as_a_shell_does() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["projects/varde", "projects/vardo", "projects/web", "projects/.hidden"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("projects/varfile"), "").unwrap();
        let root = dir.path().display().to_string();
        assert_eq!(
            complete(&format!("{root}/pro")),
            format!("{root}/projects/"),
            "one match, closed with a slash"
        );
        let names = |line: &str| {
            dirs_in(line)
                .iter()
                .map(|d| d.rsplit_terminator('/').next().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&format!("{root}/projects/")), ["varde", "vardo", "web"], "hidden left out");
        assert_eq!(
            complete(&format!("{root}/projects/v")),
            format!("{root}/projects/vard"),
            "as far as they agree, files left out"
        );
        assert_eq!(complete(&format!("{root}/projects/vard")), format!("{root}/projects/vard"));
        assert_eq!(complete(&format!("{root}/projects/.")), format!("{root}/projects/.hidden/"));
        assert_eq!(complete(&format!("{root}/nowhere/x")), format!("{root}/nowhere/x"));
    }

    #[test]
    fn it_shows_each_project_by_name_and_directory() {
        let p = Pick::new("Project for this task", known(), None);
        let s = render(60, 12, |f| p.draw(f, f.area()));
        assert!(s.contains("╭ Project for this task "), "{s}");
        assert!(s.contains("web       /src/web"), "{s}");
        assert!(s.contains(OTHER) && s.contains(" 4/4 "), "{s}");
    }
}
