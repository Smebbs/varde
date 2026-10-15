//! What the board draws: its rows, the detail and recall panes, the
//! line under it, and the help.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;
use varde_core::board::Row;
use varde_core::task;
use varde_core::title::clean;

use super::cairn::{bullet, more_below};
use super::draft::draw_draft;
use super::keys::{Act, HelpGroup, Keys, HELP};
use super::{App, Close, Found, Item, Mode};
use crate::list::{clip, elsewhere, pad, project_name};
use crate::look::{self, Tone};
use crate::now_ms;

impl App<'_> {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        // A pane's lines inside its border and padding.
        let inner = f.area().width.saturating_sub(4) as usize;
        // The view read first, so its keys under it are for what it shows.
        let view = (self.detail && self.found.is_none()).then(|| self.view_body(inner));
        let found = self.found.as_ref().map(|found| found_lines(found, inner));
        let mut line = self.bottom_line(f.area().width as usize);
        let rows = line.len().max(1) as u16;
        let [main, bottom] = Layout::vertical([Constraint::Min(1), Constraint::Length(rows)]).areas(f.area());
        // A pane is as tall as what it shows, its tabs and border, up to
        // most of the screen: a short cairn leaves the board in view. On a
        // short screen, or in a popup, the view leaves the board three rows
        // around the selection.
        let need = match (&view, &found) {
            (Some(body), _) => Some(body.0.len() + 3),
            (_, Some((lines, _))) => Some(lines.len() + 2),
            _ => None,
        };
        let compact = view.is_some() && (self.popup || f.area().height < 30);
        let (board_area, detail_area) = match need {
            Some(need) => {
                let most = match view {
                    Some(_) if compact => main.height.saturating_sub(4),
                    Some(_) => main.height * 70 / 100,
                    None => main.height * 60 / 100,
                };
                let height = (need as u16).min(most);
                let [a, b] = Layout::vertical([Constraint::Min(0), Constraint::Length(height)]).areas(main);
                (a, Some(b))
            }
            None => (main, None),
        };

        let now = now_ms();
        let width = board_area.width as usize;
        let selected = self.list.selected();
        // Kept to one project, the board names it once, in its title.
        let projects = self.elsewhere.is_none();
        // A pane open has the keys: the board dims under it, its selection
        // still marked; so it does under the form and the help.
        let dim = detail_area.is_some() || matches!(self.mode, Mode::New(_) | Mode::Help);
        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(n, i)| {
                ListItem::new(match *i {
                    Item::Header(g) => {
                        let g = &self.groups[g];
                        header_line(g.label, g.rows.len(), self.folded(g), dim)
                    }
                    Item::Row(g, r) => {
                        let needs_you = self.groups[g].key == "needs_you";
                        let r = &self.groups[g].rows[r];
                        let (marked, at) = (self.is_marked(&r.task), selected == Some(n));
                        row_line(r, marked, at, now, width, projects, dim, needs_you)
                    }
                })
            })
            .collect();
        (self.page, self.pane) = (board_area.height.saturating_sub(1), detail_area.map_or(0, |a| a.height));
        let highlight = if dim { Style::default().fg(Color::DarkGray) } else { Style::default() };
        let list = List::new(items)
            .block(Block::default().title(self.scope_title()).borders(Borders::TOP))
            .highlight_style(highlight.add_modifier(Modifier::REVERSED));
        // ratatui scrolls only as far as the selection needs and never
        // back, so the list is never left scrolled past what fills it, and
        // with its first row selected it shows its top: the first group's
        // header with it.
        let room = self.items.len().saturating_sub(board_area.height.saturating_sub(1) as usize);
        let first = self.list.selected().is_some_and(|s| (0..s).all(|i| !self.selectable(i)));
        let around = self.list.selected().filter(|_| compact && board_area.height <= 6);
        let offset = self.list.offset_mut();
        *offset = match around {
            // The row over the selection in view too.
            Some(s) => s.saturating_sub(1).min(room),
            None if first => 0,
            None => (*offset).min(room),
        };
        f.render_stateful_widget(list, board_area, &mut self.list);
        // A first look at an empty board says how it fills.
        if self.rows().next().is_none() && board_area.height > self.items.len() as u16 + 4 {
            let y = board_area.y + self.items.len() as u16 + 2;
            let hint = Rect { y, height: board_area.bottom() - y, ..board_area };
            let text = format!(
                "No tasks here yet. {} writes one, or start {} in a repository: its session shows up here once you ask it something.",
                self.keys.label(Act::New),
                crate::start::default_agent()
            );
            f.render_widget(Paragraph::new(text.dark_gray()).wrap(Wrap { trim: true }), hint);
        }

        match (detail_area, view, found) {
            (Some(area), Some(body), _) => self.draw_view(f, area, body),
            (Some(area), _, Some((text, (top, bottom)))) => {
                let height = area.height.saturating_sub(2);
                // The picked hit stays in view.
                if top < self.scroll {
                    self.scroll = top;
                } else if bottom >= self.scroll + height {
                    self.scroll = bottom + 1 - height.min(bottom + 1);
                }
                self.scroll = self.scroll.min(text.len().saturating_sub(height as usize) as u16);
                let mut block =
                    Block::bordered().border_type(BorderType::Rounded).padding(Padding::horizontal(1));
                if (self.scroll + height) < text.len() as u16 {
                    block = block.title_bottom(more_below());
                }
                f.render_widget(Paragraph::new(text).block(block).scroll((self.scroll, 0)), area);
            }
            _ => {}
        }

        match &self.mode {
            Mode::New(draft) => draw_draft(f, draft, main),
            Mode::Help => {
                self.help_most = draw_help(f, &self.keys, main, &mut self.help_scroll);
                // Its keys under it, now it is known whether it scrolls.
                line = self.bottom_line(f.area().width as usize);
            }
            _ => {}
        }

        f.render_widget(Paragraph::new(line), bottom);
    }

    /// Marked, or one of the marked tasks a question is about.
    fn is_marked(&self, task: &str) -> bool {
        let asked = match &self.mode {
            Mode::Drop(ids) | Mode::Stop(_, ids) if ids.len() > 1 => ids.as_slice(),
            _ => &[],
        };
        self.marked.iter().chain(asked).any(|t| t == task)
    }

    /// A task as a question names it: by its title, as the board shows it.
    pub(super) fn named(&self, id: &str) -> String {
        match self.row_of(id) {
            Some(r) => format!("\"{}\"", clip(&clean(&r.title), 40)),
            None => task::short(id),
        }
    }

    /// The board's title: which project it keeps to, what that leaves
    /// out, and what `.` does.
    fn scope_title(&self) -> Line<'static> {
        let (what, key) = match &self.here {
            None => (String::new(), String::new()),
            Some(here) => {
                let name = clean(project_name(Some(here)));
                match self.elsewhere {
                    Some(gone) if gone.tasks > 0 => {
                        (format!(" · {name} · {}", elsewhere(gone)), ". all".into())
                    }
                    Some(_) => (format!(" · {name}"), String::new()),
                    None if self.here_has => (" · every project".into(), format!(". only {name}")),
                    None if self.rows().next().is_none() => (format!(" · {name}"), String::new()),
                    None => (format!(" · every project (none in {name} yet)"), String::new()),
                }
            }
        };
        let key = if key.is_empty() { key } else { format!("  {key}") };
        Line::from(vec![" varde".bold(), Span::raw(what), key.dark_gray(), " ".into()])
    }

    /// What follows a directory prompt: the directories Tab could
    /// complete it to, the one in the line in view and marked, in at most
    /// half the `width`; or else its `keys`.
    fn prompt_tail(&self, keys: &[(&str, &str)], width: usize) -> Vec<Span<'static>> {
        let Some(menu) = &self.menu else { return key_spans(keys) };
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

    /// The rows under the board: a prompt, the message, or the keys.
    /// Two where one is too narrow for it.
    fn bottom_line(&self, width: usize) -> Vec<Line<'static>> {
        // A question too wide for one row has its keys on a row of their own.
        let ask = |s: String| match s.width() <= width {
            true => vec![Line::from(s.bold())],
            false => {
                let (question, keys) = s.split_once("  ").unwrap_or((&s, ""));
                let mut rows: Vec<Line> =
                    wrap(question, width).into_iter().map(|r| Line::from(r.bold())).collect();
                rows.extend(wrap(keys, width).into_iter().map(|r| Line::from(r.bold())));
                rows
            }
        };
        let said = || {
            let rows = fit_message(&clean(&tilde_in(&self.message)), width);
            let error = self.message.starts_with("error:");
            rows.into_iter().map(move |r| if error { Line::from(r.red()) } else { Line::from(r) })
        };
        // While a form or prompt is open, what was said about it, such as
        // why it was refused, shows over it until the next key.
        let prompt = matches!(
            self.mode,
            Mode::New(_) | Mode::Where(..) | Mode::Move(..) | Mode::Project(..) | Mode::Recall(_)
        );
        let one = |l: Line<'static>| -> Vec<Line<'static>> {
            let mut rows: Vec<Line<'static>> =
                if prompt && !self.message.is_empty() { said().collect() } else { Vec::new() };
            rows.push(l);
            rows
        };
        match &self.mode {
            Mode::New(d) => one(Line::from(
                key_spans(&[
                    match (d.in_body, &d.task) {
                        (false, Some(_)) => ("RET", "save"),
                        (false, None) => ("RET", "create"),
                        (true, Some(_)) => ("C-s", "save"),
                        (true, None) => ("C-s", "create"),
                    },
                    ("Tab", "next field"),
                    ("Esc", "later"),
                    ("C-x C-e", "$EDITOR"),
                ])[1..]
                    .to_vec(),
            )),
            Mode::Help if self.help_most > 0 => {
                one(Line::from("j/k C-d/C-u scroll · any other key closes this".dark_gray()))
            }
            Mode::Help => one(Line::from("any key closes this".dark_gray())),
            Mode::Recall(q) => {
                let keys = key_spans(&[("RET", "search"), ("Esc", "cancel")]);
                let room = width.saturating_sub(8 + keys.iter().map(Span::width).sum::<usize>());
                let mut line = vec!["Recall: ".bold()];
                line.extend(q.line_spans(room));
                line.extend(keys);
                one(Line::from(line))
            }
            Mode::Move(_, dir) => {
                let tail =
                    self.prompt_tail(&[("RET", "move"), ("Tab", "complete"), ("Esc", "cancel")], width);
                let room = width.saturating_sub(17 + tail.iter().map(Span::width).sum::<usize>());
                let mut line = vec!["Move to project: ".bold()];
                line.extend(dir.line_spans(room));
                line.extend(tail);
                one(Line::from(line))
            }
            Mode::Where(_, dir) => {
                let tail =
                    self.prompt_tail(&[("RET", "create"), ("Tab", "complete"), ("Esc", "back")], width);
                let room = width.saturating_sub(23 + tail.iter().map(Span::width).sum::<usize>());
                let mut line = vec!["Project for this task: ".bold()];
                line.extend(dir.line_spans(room));
                line.extend(tail);
                one(Line::from(line))
            }
            Mode::WhenTree(d, dir) => {
                let title =
                    if d.title.text().trim().is_empty() { "the new task" } else { d.title.text().trim() };
                let keys = "  Y/n · Esc back";
                let question = crate::start::worktree_question(
                    title,
                    Some(std::path::Path::new(&super::home_in(dir))),
                    width.saturating_sub(keys.width()),
                );
                ask(format!("{question}{keys}"))
            }
            Mode::TakeOver(_, asked) => {
                let row = self.row_of(&asked.task);
                let title = row.map_or_else(|| task::short(&asked.task), |r| r.title.clone());
                ask(format!("\"{title}\" is working in the background. Take it over? It stops, and carries on here  y/N"))
            }
            Mode::Worktree(_, asked, project) => {
                let row = self.row_of(&asked.task);
                let project = project.clone().or_else(|| row.and_then(|r| r.project.clone()).map(Into::into));
                let title = row.map_or_else(|| task::short(&asked.task), |r| r.title.clone());
                let keys = "  Y/n · Esc cancel";
                let question = crate::start::worktree_question(
                    &title,
                    project.as_deref(),
                    width.saturating_sub(keys.width()),
                );
                ask(format!("{question}{keys}"))
            }
            Mode::Project(_, _, choices, i, dir) => {
                let mut keys = vec![];
                if !choices.is_empty() {
                    keys.push(format!("  {}/{}", i + 1, choices.len()).dark_gray());
                    keys.extend(key_spans(&[("↑/↓", "other")]));
                }
                keys.extend(
                    self.prompt_tail(&[("Tab", "complete"), ("RET", "start"), ("Esc", "cancel")], width),
                );
                let room = width.saturating_sub(23 + keys.iter().map(Span::width).sum::<usize>());
                let mut line = vec!["Project for this task: ".bold()];
                line.extend(dir.line_spans(room));
                line.extend(keys);
                one(Line::from(line))
            }
            Mode::Drop(ids) => {
                let working = ids.iter().any(|id| self.row_of(id).is_some_and(|r| working(r.status)));
                // Only a worktree's work is kept: a task without one has
                // nothing there to keep.
                let kept = ids.iter().any(|id| self.row_of(id).is_some_and(|r| r.worktree));
                ask(match ids.as_slice() {
                    [id] => format!(
                        "Drop {}?{}{} Starting it again brings it back.  y/N",
                        self.named(id),
                        if working { " Its working session is stopped." } else { "" },
                        if kept { " Its work is kept under refs/varde/discarded/." } else { "" }
                    ),
                    _ => format!(
                        "Drop the {} marked tasks?{}{} Starting one again brings it back.  y/N",
                        ids.len(),
                        if working { " Their working sessions are stopped." } else { "" },
                        if kept {
                            " The work in their worktrees is kept under refs/varde/discarded/."
                        } else {
                            ""
                        }
                    ),
                })
            }
            Mode::Stop(how, ids) => {
                // Worded by what the sessions are doing: a question waiting
                // for you is not work in progress, nor is what a finished
                // turn left running.
                let rows: Vec<&Row> = ids.iter().filter_map(|id| self.row_of(id)).collect();
                let left = |r: &Row| r.agents + r.shells > 0 && !working(r.status) && r.status != "blocked";
                let verb = if *how == Close::Done { "Done" } else { "Park" };
                let keys = "y stop · n leave running · Esc cancel";
                ask(match ids.as_slice() {
                    [id] => {
                        let doing = match rows.first() {
                            Some(r) if r.status == "blocked" => "is waiting for your answer".to_string(),
                            Some(r) if left(r) => format!("has {} running", running(r)),
                            _ => "is working".into(),
                        };
                        format!("{verb} {}? It {doing}.  {keys}", self.named(id))
                    }
                    _ => {
                        let doing = if rows.iter().all(|r| r.status == "blocked") {
                            "are waiting for your answer"
                        } else if rows.iter().any(|r| working(r.status)) {
                            "are working"
                        } else if rows.iter().any(|r| left(r)) {
                            "have agents or shells running"
                        } else {
                            "are working"
                        };
                        format!("{verb} the {} marked tasks? Some {doing}.  {keys}", ids.len())
                    }
                })
            }
            Mode::Reopen(_, id) => ask(format!(
                "{} is {}: reopen and start it?  y/N",
                self.named(id),
                self.row_of(id).map_or("closed", |r| r.state.as_str())
            )),
            // A line typed for the view: on the prompt line, what was said
            // about it, such as why it was refused, over it.
            Mode::Normal if self.detail && self.found.is_none() && self.view.typing() => {
                let mut rows: Vec<Line<'static>> = said().collect();
                rows.extend(self.line_prompt(width));
                rows
            }
            Mode::Normal if !self.message.is_empty() => said().collect(),
            Mode::Normal => {
                let mut spans = Vec::new();
                let keys = if let Some(found) = &self.found {
                    // The task keys act on the hit picked, as `d` does.
                    let scope = if found.all { "this project" } else { "every project" };
                    let k = |act| self.keys.label(act);
                    // A project's cairn has no task to start or close.
                    let task = found.hits.get(found.at).is_some_and(|h| !h.task.is_empty());
                    let mut keys = vec![("j/k".into(), "pick"), ("RET".into(), "view")];
                    if task {
                        keys.extend([(k(Act::Background), "start"), (k(Act::Done), "done")]);
                    }
                    keys.extend([(k(Act::Scope), scope), ("Esc".into(), "close")]);
                    keys
                } else if self.detail {
                    self.view_footer(width)
                } else {
                    self.footer(width)
                };
                for (key, what) in keys {
                    if !spans.is_empty() {
                        spans.push("  ".into());
                    }
                    spans.push(Span::raw(key).bold());
                    if !what.is_empty() {
                        spans.push(format!(" {what}").dark_gray());
                    }
                }
                one(Line::from(spans))
            }
        }
    }
}

/// Keys as the footer shows them, each bold and what it does dim, after
/// two spaces.
pub(super) fn key_spans(keys: &[(&str, &str)]) -> Vec<Span<'static>> {
    keys.iter().flat_map(|(k, w)| ["  ".into(), k.to_string().bold(), format!(" {w}").dark_gray()]).collect()
}

/// A session at work, as against one waiting for you.
fn working(status: &str) -> bool {
    matches!(status, "working" | "delegated")
}

/// What a row's session left running: `3 agents and a shell`.
fn running(r: &Row) -> String {
    let n = |k: u32, one: &str, many: &str| match k {
        0 => None,
        1 => Some(one.to_string()),
        k => Some(format!("{k} {many}")),
    };
    let parts = [n(r.agents, "an agent", "agents"), n(r.shells, "a shell", "shells")];
    parts.into_iter().flatten().collect::<Vec<_>>().join(" and ")
}

/// What `?` shows over the board, in a pane as the view is: what each
/// group holds and what a row's glyph says, the keys as bound in two
/// columns, each as wide as what it says, and what a cairn is and how its
/// lines are marked. Blank lines split them where there is room; too
/// narrow for two columns, it is one. Taller than `area`, it scrolls from
/// `scroll`, kept within what it has; how far it can is returned.
fn draw_help(f: &mut Frame, keys: &Keys, area: Rect, scroll: &mut u16) -> u16 {
    // Rows of a column: a heading, or a key and what it does.
    let heading = |name: &str| Span::raw(name.to_string()).bold().underlined();
    let column = |groups: &[HelpGroup], gap: bool| -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, lines) in groups {
            if gap && !out.is_empty() {
                out.push((String::new(), String::new()));
            }
            out.push((String::new(), name.to_string()));
            for (acts, what) in *lines {
                // Esc closes a pane as `q` does, and unmarks first.
                out.push(match (acts, keys.label(Act::Quit)) {
                    ([Act::Quit], q) if q != "Esc" => {
                        (format!("{q}/Esc"), format!("close the pane; {q} quits"))
                    }
                    _ => (keys.labels(acts), what.to_string()),
                });
            }
        }
        out
    };
    // A column's rows as lines, its keys as wide as the widest.
    let lines = |rows: &[(String, String)]| -> Vec<Line<'static>> {
        let w = rows.iter().map(|(k, _)| k.width()).max().unwrap_or(0) + 1;
        rows.iter()
            .map(|(k, what)| match k.is_empty() {
                true if what.is_empty() => Line::default(),
                true => Line::from(heading(what)),
                false => Line::from(vec![format!("{k:<w$}").bold(), Span::raw(what.clone())]),
            })
            .collect()
    };
    let legend = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
        rows.iter().map(|(g, what)| (format!("{g} "), what.to_string())).collect()
    };
    let (left_legend, right_legend) = (
        lines(&legend(&[
            ("Needs you", "turn ended or it asks"),
            ("Working", "a session at work"),
            ("Open", "no session running"),
        ])),
        lines(&legend(&[
            ("Inbox", "not made by you"),
            ("Done", "closed today or not landed"),
            ("Parked", "put aside, folded"),
        ])),
    );
    let columns = |gap: bool| {
        let mut left = column(&HELP[..2], gap);
        let right = column(&HELP[2..], gap);
        // The view's own keys, which no `[keys]` binds.
        if gap {
            left.push((String::new(), String::new()));
        }
        left.push((String::new(), "In the view".into()));
        for (k, what) in [
            ("[ ]", "Task / Project tab"),
            ("e", "edit the line picked"),
            ("o/a", "add here / to section"),
            ("x/u", "remove / bring back"),
            ("X", "drop the task"),
            ("J/K", "next / previous task"),
        ] {
            left.push((k.into(), what.into()));
        }
        (lines(&left), lines(&right))
    };
    let width = 80.min(area.width);
    let inner = width.saturating_sub(4) as usize;
    let widest = |lines: &[Line]| lines.iter().map(Line::width).max().unwrap_or(0);
    // Two columns side by side, the right one `w` in, or with no `w` one
    // under the other.
    let side =
        |left: Vec<Line<'static>>, right: Vec<Line<'static>>, w: Option<usize>| -> Vec<Line<'static>> {
            let Some(w) = w else { return left.into_iter().chain(right).collect() };
            let n = left.len().max(right.len());
            let (mut left, mut right) = (left.into_iter(), right.into_iter());
            (0..n)
                .map(|_| {
                    let mut line = left.next().unwrap_or_default();
                    let pad = w.saturating_sub(line.width());
                    line.spans.push(" ".repeat(pad).into());
                    line.spans.extend(right.next().unwrap_or_default().spans);
                    line
                })
                .collect()
        };
    let body = |gap: bool| {
        let cairn = "    what sessions learned  › yours  ? stale: its code is gone";
        let mut out = vec![
            Line::from(vec![
                heading("Groups"),
                "   ● working or unread  ○ seen or idle  ✓ done  ‖ parked".dark_gray(),
            ]),
            Line::from(vec![heading("Cairn"), clip(cairn, inner.saturating_sub(5)).dark_gray()]),
        ];
        // The right columns lined up, after the widest left one.
        let (left, right) = columns(gap);
        let w = widest(&left).max(widest(&left_legend)) + 2;
        let w = (w + widest(&right).max(widest(&right_legend)) <= inner).then_some(w);
        out.extend(side(left_legend.clone(), right_legend.clone(), w));
        if gap {
            out.push(Line::default());
        }
        out.extend(side(left, right, w));
        if gap {
            out.push(Line::default());
        }
        out.push(Line::from(
            "u undoes the last x, d, P or X in the view, the last d/P/x on the board".dark_gray(),
        ));
        out
    };
    // With room, a blank line between the parts.
    let roomy = body(true);
    let text = if roomy.len() as u16 + 2 <= area.height { roomy } else { body(false) };
    let height = (text.len() as u16 + 2).min(area.height);
    let most = (text.len() as u16 + 2).saturating_sub(height);
    *scroll = (*scroll).min(most);
    let area =
        Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height };
    let more = if *scroll < most { " ↓ j more " } else { "" };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(Padding::horizontal(1))
        .title(" varde ")
        .title_bottom(Line::from(more.dark_gray()).right_aligned());
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(text).block(block).scroll((*scroll, 0)), area);
    most
}

/// A group's header: its name and count, dim when it is empty or the
/// board is, and `▸` when folded.
fn header_line(label: &str, count: usize, folded: bool, dim: bool) -> Line<'static> {
    if count == 0 || dim {
        return Line::from(format!("{label}  {count}{}", if folded { " ▸" } else { "" }).dark_gray());
    }
    Line::from(vec![
        Span::raw(label.to_string()).bold(),
        format!("  {count}{}", if folded { " ▸" } else { "" }).dark_gray(),
    ])
}

/// A task's row: its glyph, its title, and what else there is to know
/// on the right. The selected row is drawn plain, so its highlight is
/// one even bar; under a pane, every row is dim. Under Needs you, a turn
/// that is yours goes without saying.
#[allow(clippy::too_many_arguments)]
fn row_line(
    r: &Row,
    marked: bool,
    selected: bool,
    now: i64,
    width: usize,
    projects: bool,
    dim: bool,
    needs_you: bool,
) -> Line<'static> {
    let style = |tone: Tone| match (selected, dim) {
        (true, _) => Style::default(),
        (_, true) => tone_style(Tone::Quiet),
        _ => tone_style(tone),
    };
    let (glyph, tone) = look::glyph(r);
    let mut meta: Vec<Span> = Vec::new();
    let parts =
        look::meta(r, now, projects).into_iter().filter(|(t, _)| !(needs_you && t == look::YOUR_TURN));
    for (i, (text, tone)) in parts.enumerate() {
        if i > 0 {
            meta.push(Span::styled(look::SEP, style(Tone::Quiet)));
        }
        meta.push(Span::styled(clean(&text), style(tone)));
    }
    meta.push(Span::styled(format!("  {}", r.short), style(Tone::Quiet)));
    let meta_w: usize = meta.iter().map(Span::width).sum();
    let title_w = width.saturating_sub(3 + 2 + meta_w).max(12);
    let closed = matches!(r.state.as_str(), "done" | "parked" | "dropped");
    let mut line = vec![
        Span::raw(if marked { "*" } else { " " }).bold(),
        Span::styled(glyph, style(tone)),
        " ".into(),
        Span::styled(
            pad(&clean(&r.title), title_w),
            if closed || dim { style(Tone::Quiet) } else { Style::default() },
        ),
        "  ".into(),
    ];
    line.extend(meta);
    Line::from(line)
}

/// The colour a part of a row is drawn in.
pub(super) fn tone_style(tone: Tone) -> Style {
    let fg = match tone {
        Tone::Working | Tone::Landed => Color::Green,
        Tone::YourTurn | Tone::Edited => Color::Yellow,
        Tone::Asking | Tone::Failing => Color::Red,
        Tone::Quiet => Color::DarkGray,
    };
    Style::default().fg(fg)
}

/// The recall pane in `width` columns: the query and where it looked,
/// then each hit, the picked one highlighted; and the first and last
/// rows of the picked one. A project's cairn is named by its project,
/// and a hit's line wraps under its own words.
fn found_lines(found: &Found, width: usize) -> (Vec<Line<'static>>, (u16, u16)) {
    let scope = if found.all { "every project" } else { "this project" };
    let scope = format!("  in {scope}");
    let query = clip(&clean(&found.query), width.saturating_sub(8 + scope.width()).max(8));
    let mut lines = vec![Line::from(vec![format!("Recall: {query}").bold(), scope.dark_gray()])];
    if found.hits.is_empty() {
        lines.push(Line::from("nothing found".dark_gray()));
        lines.extend(wrap(crate::memory_cmd::SEARCHES, width).into_iter().map(|l| Line::from(l.dark_gray())));
    } else if found.everywhere {
        lines.push(Line::from("nothing in this project; the best from every project".dark_gray()));
    }
    let mut picked = (0, 0);
    for (i, h) in found.hits.iter().enumerate() {
        let at = i == found.at;
        let top = lines.len() as u16;
        let quiet = |s: String| if at { Span::raw(s) } else { Span::raw(s).dark_gray() };
        let project = clean(project_name(h.project.as_deref()));
        let (title, meta) = if h.task.is_empty() {
            (format!("{project} · project cairn"), String::new())
        } else {
            (clean(&h.title), format!("{}  {project} · {}", crate::memory_cmd::closed(&h.state), h.short))
        };
        let title = clip(&title, width.saturating_sub(meta.width()).max(8));
        let mut line = Line::from(vec![Span::raw(title).bold(), quiet(meta)]);
        if at {
            line = line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        lines.push(line);
        let lead = vec![Span::raw(format!("    {}: ", crate::memory_cmd::what_matched(&h.kind)))];
        let mut after = Vec::new();
        if h.stale {
            after.push(Span::raw(" ?").yellow());
        }
        if h.more > 0 {
            let whose = if h.task.is_empty() { "project cairn" } else { "task" };
            after.push(format!("  (+{} more in this {whose})", h.more).dark_gray());
        }
        bullet(&mut lines, lead, &crate::memory_cmd::clip(&h.snippet, 300), after, false, false, width);
        if at {
            picked = (top, lines.len() as u16 - 1);
        }
    }
    (lines, picked)
}

pub(super) use crate::tasks::tilde;

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

/// A message in at most two rows of `width`.
fn fit_message(s: &str, width: usize) -> Vec<String> {
    fit_rows(s, width, 2)
}

/// `s` in at most `rows` rows of `width`, broken between words: its long
/// paths shortened first, keeping their ends, so the words around them
/// stay whole; only then cut in the middle, so its end is still there.
pub(super) fn fit_rows(s: &str, width: usize, rows: usize) -> Vec<String> {
    let fits = |t: &str| wrap(t, width).len() <= rows;
    let mut out = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut paths: Vec<String> = out.split(' ').filter(|w| w.contains('/')).map(str::to_string).collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.width()));
    for p in paths {
        let (mut now, mut n) = (p.clone(), p.width());
        while !fits(&out) && n > 16 {
            n = n.saturating_sub(4).max(16);
            // A path in brackets keeps its opening one.
            let open = p.len() - p.trim_start_matches(['(', '"']).len();
            let short = format!("{}{}", &p[..open], left_clip(&p[open..], n - open));
            out = out.replacen(&now, &short, 1);
            now = short;
        }
    }
    let mut n = out.width();
    let whole = out.clone();
    while !fits(&out) && n > 3 {
        n -= 1;
        out = middle_clip(&whole, n);
    }
    wrap(&out, width)
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

/// `text` in rows of at most `width` columns, broken between words; a
/// word longer than a row is cut.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        if !row.is_empty() && row.width() + 1 + word.width() > width {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
        while row.width() > width {
            let cut: String = crate::list::clip(&row, width).trim_end_matches('…').to_string();
            let cut = if cut.is_empty() { row.chars().take(1).collect() } else { cut };
            row = row[cut.len()..].to_string();
            rows.push(cut);
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
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
        let m = fit_message(&said, 70);
        assert!(m.len() == 2 && m.iter().all(|r| r.width() <= 70), "{m:?}");
        let m = m.join(" ");
        assert!(m.starts_with("worktree kept at …") && m.contains("fix-it-k3m9qz"), "{m}");
        assert!(m.ends_with(" while a session runs in it; it goes once that ends"), "{m}");
        assert_eq!(fit_message("short, varde/x", 70), ["short, varde/x"]);
        // Short enough in characters, but not once broken between words:
        // cut in the middle, its end still there.
        let said = "the session of abcdef exited with code 1 before it said anything; varde attach abcdef --fresh starts another";
        let m = fit_message(said, 48);
        assert!(m.len() == 2 && m.iter().all(|r| r.width() <= 48), "{m:?}");
        assert!(m[0].starts_with("the session") && m[1].ends_with("--fresh starts another"), "{m:?}");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(tilde_in(&format!("at {home}/src/app; ok")), "at ~/src/app; ok");
    }
}
