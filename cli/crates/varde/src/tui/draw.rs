//! What the board draws: its rows, the detail and recall panes, the
//! line under it, and the help.

use std::path::{Path, PathBuf};

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;
use varde_core::task;
use varde_core::title::clean;

use super::draft::draw_draft;
use super::{busy, App, Close, Found, Item, Mode, BUSY};
use crate::list::{age, clip, elsewhere, pad, project_name};
use crate::{now_ms, process};

impl App<'_> {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let line = self.bottom_line(f.area().width as usize);
        let rows = if line.width() > f.area().width as usize { 2 } else { 1 };
        let [main, bottom] = Layout::vertical([Constraint::Min(1), Constraint::Length(rows)]).areas(f.area());
        let (board_area, detail_area) = if self.detail || self.found.is_some() {
            let [a, b] =
                Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(main);
            (a, Some(b))
        } else {
            (main, None)
        };

        let now = now_ms();
        let width = board_area.width as usize;
        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|i| {
                let marked = matches!(i, Item::Row(r) if self.marked.contains(&r.task));
                ListItem::new(item_line(i, marked, now, width))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().title(self.scope_title()).borders(Borders::TOP))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        // ratatui scrolls only as far as the selection needs and never
        // back, so the list is never left scrolled past what fills it, and
        // with its first row selected it shows its top: the first group's
        // header with it.
        let room = self.items.len().saturating_sub(board_area.height.saturating_sub(1) as usize);
        let first = self.list.selected().is_some_and(|s| (0..s).all(|i| self.row(i).is_none()));
        let offset = self.list.offset_mut();
        *offset = if first { 0 } else { (*offset).min(room) };
        f.render_stateful_widget(list, board_area, &mut self.list);

        if let Some(area) = detail_area {
            let text = if let Some(found) = &self.found {
                // The picked hit stays in view.
                let bottom = 3 + u16::from(found.everywhere) + 2 * found.at as u16;
                self.scroll = bottom.saturating_sub(area.height.saturating_sub(1));
                found_lines(found)
            } else if self.project_detail {
                self.current()
                    .map(|r| {
                        let p = r.project.clone();
                        let c = p.as_deref().and_then(|p| {
                            let s = self.paths.open(BUSY).ok()?;
                            varde_core::memory::project_cairn(&s.conn, p).ok().flatten().map(|(c, _)| c)
                        });
                        project_lines(p, c)
                    })
                    .unwrap_or_default()
            } else {
                self.current_id()
                    .and_then(|id| {
                        let s = self.paths.open(BUSY).ok()?;
                        let d = task::detail(&s, &id, &process::alive).ok()?;
                        let c = varde_core::memory::cairn(&s.conn, &id).ok().flatten().map(|(c, _)| c);
                        let notes = varde_core::memory::pending_notes(&s.conn, &id).unwrap_or_default();
                        Some(detail_lines(d, c, notes.into_iter().map(|(_, t)| t).collect()))
                    })
                    .unwrap_or_default()
            };
            self.scroll = self.scroll.min(text.len().saturating_sub(1) as u16);
            let title = if text.len() > area.height as usize { " C-d/C-u scroll " } else { "" };
            let para = Paragraph::new(text)
                .block(Block::default().borders(Borders::TOP).title(title.dark_gray()))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0));
            f.render_widget(para, area);
        }

        match &self.mode {
            Mode::New(draft) => draw_draft(f, draft, main),
            Mode::Help => draw_help(f, main),
            _ => {}
        }

        f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), bottom);
    }

    /// The board's title: which project it keeps to, what that leaves
    /// out, and what `.` does.
    fn scope_title(&self) -> String {
        let Some(here) = &self.here else { return " varde ".into() };
        let name = clean(project_name(Some(here)));
        match self.elsewhere {
            Some(gone) if gone.tasks > 0 => format!(" varde · {name} · {}  . all ", elsewhere(gone)),
            Some(_) => format!(" varde · {name} "),
            None if self.here_has => format!(" varde · every project  . only {name} "),
            None => format!(" varde · every project (none in {name} yet) "),
        }
    }

    /// What follows a directory prompt: the directories Tab could
    /// complete it to, the one in the line in view and marked, in at most
    /// half the `width`; or else its `keys`.
    fn prompt_tail(&self, keys: &str, width: usize) -> Vec<Span<'static>> {
        let Some(menu) = &self.menu else { return vec![keys.to_string().dark_gray()] };
        let names: Vec<String> = menu.names.iter().map(|n| clean(n)).collect();
        let fits = |shown: &[String]| shown.iter().map(|n| n.width() + 2).sum::<usize>() + 6 <= width / 2;
        let at = menu.at.unwrap_or(0);
        let mut from = 0;
        while from < at && !fits(&names[from..=at]) {
            from += 1;
        }
        let mut to = at + 1;
        while to < names.len() && fits(&names[from..=to]) {
            to += 1;
        }
        let mut out = Vec::new();
        if from > 0 {
            out.push("  …".dark_gray());
        }
        for (i, n) in names.iter().enumerate().take(to).skip(from) {
            out.push("  ".into());
            out.push(if menu.at == Some(i) { n.clone().reversed() } else { n.clone().dark_gray() });
        }
        if to < names.len() {
            out.push("  …".dark_gray());
        }
        out
    }

    /// The line under the board: a prompt, the message, or the keys.
    /// Two rows where one is too narrow for it.
    fn bottom_line(&self, width: usize) -> Line<'static> {
        let ask = |s: String| Line::from(s.bold());
        match &self.mode {
            Mode::New(d) => Line::from(
                format!(
                    "Tab title/body  Enter newline in body  C-s or Enter on title {}  C-g $EDITOR  Esc cancel",
                    if d.task.is_some() { "save" } else { "create" }
                )
                .dark_gray(),
            ),
            Mode::Help => Line::from("any key closes this".dark_gray()),
            Mode::Recall(q) => Line::from(vec!["Recall: ".bold(), Span::raw(clean(q)), "▏".into()]),
            Mode::Move(_, dir) => {
                let tail = self.prompt_tail("  RET move  Tab complete  C-u clear  Esc cancel", width);
                let room = width.saturating_sub(17 + 1 + tail.iter().map(Span::width).sum::<usize>());
                let mut line = vec!["Move to project: ".bold(), Span::raw(left_clip(&clean(dir), room)), "▏".into()];
                line.extend(tail);
                Line::from(line)
            }
            Mode::Where(_, dir) => {
                let tail = self.prompt_tail("  RET create  Tab complete  C-u clear  Esc back", width);
                let room = width.saturating_sub(23 + 1 + tail.iter().map(Span::width).sum::<usize>());
                let mut line = vec!["Project for this task: ".bold(), Span::raw(left_clip(&clean(dir), room)), "▏".into()];
                line.extend(tail);
                Line::from(line)
            }
            Mode::Worktree(..) => ask("Work in a separate git worktree?  [Y/n]".into()),
            Mode::Project(_, _, choices, i) => {
                let keys = format!("  {}/{}  j/k other  RET start  Esc cancel", i + 1, choices.len());
                let room = width.saturating_sub(23 + keys.width());
                Line::from(vec![
                    "Project for this task: ".bold(),
                    Span::raw(left_clip(&tilde(&choices[*i]), room)).bold(),
                    keys.dark_gray(),
                ])
            }
            Mode::Drop(id) => ask(format!(
                "Throw {} away?{} Its work is kept under refs/varde/discarded/, and starting it again brings it back.  [y/N]",
                task::short(id),
                if self.row_of(id).is_some_and(|r| busy(r.status)) {
                    " Its working session is stopped."
                } else {
                    ""
                }
            )),
            Mode::Stop(how, ids) => {
                let verb = if *how == Close::Done { "Close" } else { "Park" };
                ask(match ids.as_slice() {
                    [id] => format!(
                        "{} is still working. {verb} it: [s]top it, [k]eep it running, Esc cancels",
                        task::short(id)
                    ),
                    _ => format!(
                        "Some are still working. {verb} them: [s]top them, [k]eep them running, Esc cancels"
                    ),
                })
            }
            Mode::Reopen(_, id) => ask(format!(
                "{} is {}: reopen and start it?  [y/N]",
                task::short(id),
                self.row_of(id).map_or("closed", |r| r.state.as_str())
            )),
            Mode::Normal if !self.message.is_empty() => {
                let text = fit_message(&clean(&tilde_in(&self.message)), 2 * width - width / 4);
                if self.message.starts_with("error:") {
                    Line::from(text.red())
                } else {
                    Line::from(text)
                }
            }
            Mode::Normal => Line::from(FOOTER.dark_gray()),
        }
    }
}

/// The essentials, in 80 columns; `?` has the rest.
pub(super) const FOOTER: &str = "RET start  n new  d done  p park  x throw away  / recall  ? help  q quit";

const HELP: [(&str, &[(&str, &str)]); 4] = [
    (
        "Start",
        &[
            ("RET", "start, or go to it"),
            ("s", "start in the background (tmux)"),
            ("R", "start a fresh conversation"),
            ("F", "fork the conversation"),
            ("n", "new task"),
            ("m", "move to another project"),
        ],
    ),
    ("Close", &[("d", "done"), ("p", "park"), ("x", "throw away"), ("a", "accept (Inbox)")]),
    (
        "Look",
        &[
            ("v", "detail; C-d/C-u scroll"),
            ("e", "edit title and context, or the cairn"),
            ("V", "its project cairn"),
            ("E", "edit the project cairn"),
            ("=", "diff of its work"),
            ("/", "recall"),
            ("Tab", "show Parked"),
        ],
    ),
    (
        "Board",
        &[
            ("j/k", "move; G/End last, Home first"),
            ("space", "mark; a, d and p act on all"),
            (".", "this project, or every one"),
            ("g", "refresh"),
            ("C-l", "redraw the screen"),
            ("Esc", "unmark, close the pane"),
            ("q", "close the pane, else quit"),
        ],
    ),
];

/// The keys, in two columns over the board.
fn draw_help(f: &mut Frame, area: Rect) {
    let column = |groups: &[(&str, &[(&str, &str)])]| -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (name, keys) in groups {
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.push(Line::from(name.to_string().bold()));
            for (k, what) in *keys {
                out.push(Line::from(vec![format!("{k:<6}").bold(), Span::raw(what.to_string())]));
            }
        }
        out
    };
    let (left, right) = (column(&HELP[..2]), column(&HELP[2..]));
    let height = (left.len().max(right.len()) as u16 + 2).min(area.height);
    let width = 78.min(area.width);
    let area =
        Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height };
    let block = Block::default().title(" varde keys ").borders(Borders::ALL);
    let [l, r] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(block.inner(area));
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), r);
}

fn item_line(item: &Item, marked: bool, now: i64, width: usize) -> Line<'static> {
    match item {
        Item::Header { label, count, collapsed } => {
            let style = if *count == 0 || *collapsed {
                Style::default().dark_gray()
            } else {
                Style::default().bold()
            };
            Line::styled(format!("{label} ({count}){}", if *collapsed { " ▸" } else { "" }), style)
        }
        Item::Row(r) => {
            let color = match r.status {
                "blocked" => Color::Red,
                "waiting" => Color::Yellow,
                "working" => Color::Green,
                "delegated" => Color::Cyan,
                _ => Color::Reset,
            };
            let title_w = width.saturating_sub(2 + 6 + 2 + 9 + 1 + 10 + 1 + 16 + 1 + 4 + 1).max(10);
            let (git, git_style) = r.git.as_ref().map_or(("", Style::default()), |g| {
                let color = match (g.stage.as_str(), g.checks.as_deref()) {
                    (_, Some("failing")) => Color::Red,
                    ("edited", _) => Color::Yellow,
                    ("landed", _) => Color::Green,
                    _ => Color::DarkGray,
                };
                (g.label.as_str(), Style::default().fg(color))
            });
            let origin = r.origin.as_ref().map(|o| format!("  ← {}", o.short)).unwrap_or_default();
            let bg = background(r.agents, r.shells) + &origin;
            let bg_w = bg.width().min(title_w / 2);
            Line::from(vec![
                Span::raw(if marked { "*" } else { " " }).bold(),
                // What nobody has looked at yet stands out from what you
                // have seen and left.
                Span::styled(if r.unread { "●" } else { " " }, Style::default().fg(color)),
                Span::raw(format!("{}  ", r.short)).dark_gray(),
                Span::styled(
                    format!("{:<9} ", r.status),
                    if r.unread { Style::default().fg(color).bold() } else { Style::default().fg(color) },
                ),
                Span::styled(format!("{} ", pad(git, 10)), git_style),
                Span::raw(pad(&clean(&r.title), title_w - bg_w)),
                Span::raw(format!("{} ", clip(&bg, bg_w))).dark_gray(),
                Span::raw(format!("{} ", pad(project_name(r.project.as_deref()), 16))).dark_gray(),
                Span::raw(format!("{:>4}", age(now - r.last))).dark_gray(),
            ])
        }
    }
}

/// The detail pane: what the task is and where, then its cairn, which
/// is what matters most, then its body and runs.
fn detail_lines(
    d: task::Detail,
    cairn: Option<varde_core::cairn::Cairn>,
    notes: Vec<String>,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::raw(d.short).dark_gray(),
        "  ".into(),
        Span::raw(clean(&d.title)).bold(),
    ])];
    let mut kv = |k: &str, v: String| {
        out.push(Line::from(vec![Span::raw(format!("{k:<9}")).dark_gray(), Span::raw(v)]))
    };
    kv("state", format!("{}{}", d.state, if d.inbox { " (inbox)" } else { "" }));
    if let Some(e) = d.external {
        kv("linked", e);
    }
    if let Some(p) = d.project {
        kv("project", tilde(&p));
    }
    if let Some(o) = d.origin {
        kv("from", task::short(&o));
    }
    if let Some(w) = d.worktree {
        kv("worktree", format!("{} ({})", tilde(&w.path), w.state));
        kv("branch", format!("{} from {}", w.branch, w.base));
    }
    if let Some(c) = cairn {
        out.push(Line::default());
        out.push(Line::from("cairn".dark_gray()));
        out.extend(c.display().lines().map(|l| Line::from(clean(l))));
    }
    if !notes.is_empty() {
        out.push(Line::default());
        out.push(Line::from(
            if crate::config::get().memory.enabled {
                "notes, for the next cairn update"
            } else {
                "notes (cairn updates are off)"
            }
            .dark_gray(),
        ));
        out.extend(notes.into_iter().map(|n| Line::from(format!("- {}", clean(&n)))));
    }
    if let Some(b) = d.body {
        out.push(Line::default());
        out.push(Line::from("body".dark_gray()));
        out.extend(b.lines().map(|l| Line::from(clean(l))));
    }
    if !d.runs.is_empty() {
        out.push(Line::default());
        out.push(Line::from("runs".dark_gray()));
        for r in d.runs.iter().rev() {
            out.push(Line::from(format!(
                "  {}  {:<6} {:<8} {}",
                task::short(&r.id),
                r.relation,
                if r.launched { r.status.unwrap_or("-") } else { "never launched" },
                r.cwd.as_deref().map(tilde).unwrap_or_default()
            )));
            if let Some(why) = crate::tasks::describe_why(&r.why, now_ms()) {
                out.push(Line::from(format!("          {why}")).dark_gray());
            }
        }
    }
    out
}

/// The detail pane on a task's project: its project cairn.
fn project_lines(project: Option<String>, cairn: Option<varde_core::cairn::Cairn>) -> Vec<Line<'static>> {
    let Some(p) = project else {
        return vec![Line::from("this task has no project yet: its first start picks one".dark_gray())];
    };
    let mut out = vec![Line::from(vec!["project cairn  ".dark_gray(), Span::raw(tilde(&p)).bold()])];
    out.push(Line::default());
    match cairn.filter(|c| !c.is_empty()) {
        Some(c) => out.extend(c.display().lines().map(|l| Line::from(clean(l)))),
        None => {
            out.push(Line::from("none yet: tasks closed as done add to it, and E writes to it".dark_gray()))
        }
    }
    out
}

/// The recall pane: the query and where it looked, then each hit, the
/// picked one highlighted.
fn found_lines(found: &Found) -> Vec<Line<'static>> {
    let scope = if found.all { "every project" } else { "this project; Tab: all" };
    let mut lines = vec![Line::from(vec![
        format!("recall: {}", clean(&found.query)).bold(),
        format!("  ({scope})  j/k pick, RET go, s start").dark_gray(),
    ])];
    if found.hits.is_empty() {
        lines.push(Line::from("nothing found".dark_gray()));
    } else if found.everywhere {
        lines.push(Line::from("nothing in this project; the best from every project".dark_gray()));
    }
    for (i, h) in found.hits.iter().enumerate() {
        let mut line = Line::from(vec![
            Span::raw(h.short.clone()).dark_gray(),
            "  ".into(),
            Span::raw(clean(&h.title)).bold(),
            format!("  {}", project_name(h.project.as_deref())).dark_gray(),
        ]);
        if i == found.at {
            line = line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        lines.push(line);
        let what = crate::memory_cmd::what_matched(&h.kind);
        lines.push(Line::from(format!("    {what}: {}", clean(&crate::memory_cmd::clip(&h.snippet, 300)))));
    }
    lines
}

/// `path` with the home directory as `~`.
pub(super) fn tilde(path: &str) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute() && h != Path::new("/"));
    match home.and_then(|h| Path::new(path).strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.to_string(),
    }
}

/// `text` with each path under the home directory written from `~`.
fn tilde_in(text: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if home.len() > 1 && home.starts_with('/') => {
            let home = home.trim_end_matches('/');
            text.replace(&format!("{home}/"), "~/")
        }
        _ => text.to_string(),
    }
}

/// `s` in at most `n` columns, keeping its end: a path's last
/// components say the most.
pub(super) fn left_clip(s: &str, n: usize) -> String {
    if s.width() <= n {
        return s.to_string();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut w = 1;
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > n {
            break;
        }
        tail.push(c);
        w += cw;
    }
    std::iter::once('…').chain(tail.into_iter().rev()).collect()
}

/// A message in at most `n` columns: its long paths shortened first,
/// keeping their ends, so the words around them stay whole; only then
/// cut in the middle.
fn fit_message(s: &str, n: usize) -> String {
    let mut out = s.to_string();
    let mut paths: Vec<&str> = s.split_whitespace().filter(|w| w.contains('/')).collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.width()));
    for p in paths {
        let over = out.width().saturating_sub(n);
        if over == 0 {
            break;
        }
        out = out.replacen(p, &left_clip(p, p.width().saturating_sub(over).max(16)), 1);
    }
    middle_clip(&out, n)
}

/// `s` in at most `n` columns, cut in the middle, so both what a message
/// says and where it ends stay in view.
fn middle_clip(s: &str, n: usize) -> String {
    if s.width() <= n || n < 3 {
        return s.to_string();
    }
    let head = clip(s, n / 2 + 1);
    let head = head.trim_end_matches('…');
    let tail = left_clip(s, n - head.width());
    format!("{head}{tail}")
}

/// What a session left running, e.g. `  2 agents · 1 shell`.
fn background(agents: u32, shells: u32) -> String {
    let n = |k: u32, what: &str| (k > 0).then(|| format!("{k} {what}{}", if k == 1 { "" } else { "s" }));
    let parts: Vec<String> = [n(agents, "agent"), n(shells, "shell")].into_iter().flatten().collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" · "))
    }
}

/// What `draw` puts on a `w` by `h` screen, as text.
#[cfg(test)]
pub(super) fn render(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(draw).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paths_are_cut_where_they_say_least() {
        assert_eq!(left_clip("/home/me/src/app", 8), "…src/app");
        assert_eq!(left_clip("/a/b", 8), "/a/b");
        let m = middle_clip("restored worktree /very/long/path/to/the/worktree on varde/x", 30);
        assert_eq!(m.width(), 30);
        assert!(m.starts_with("restored work") && m.ends_with("on varde/x"), "{m}");
        // A message's path gives way before its words do.
        let path = format!("/home/me/{}/.worktrees/fix-it-k3m9qz", "deep/".repeat(20));
        let said = format!("worktree kept at {path} while a session runs in it; it goes once that ends");
        let m = fit_message(&said, 140);
        assert!(m.width() <= 140, "{m}");
        assert!(m.starts_with("worktree kept at …") && m.contains("fix-it-k3m9qz"), "{m}");
        assert!(m.ends_with(" while a session runs in it; it goes once that ends"), "{m}");
        assert_eq!(fit_message("short, varde/x", 140), "short, varde/x");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(tilde_in(&format!("at {home}/src/app; ok")), "at ~/src/app; ok");
    }
}
