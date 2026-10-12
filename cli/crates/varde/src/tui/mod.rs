//! `varde` with no arguments: the board, live, in the terminal. It reads
//! the same pre-grouped rows as `varde list --json`, so it decides
//! nothing about what is shown. RET suspends it and runs the session in
//! this terminal; it comes back when the agent exits. Inside tmux the
//! session gets a window of its own instead, and the board stays live.
//! Nothing in-process writes to the terminal while the board owns it:
//! notices come back as values, and a broken config file, which would
//! be reported on stderr, is shown in its message line.

mod diff;
mod draft;
mod draw;
mod session;
mod term;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;
use varde_core::board::{self, Board, Row};
use varde_core::plan::Want;
use varde_core::store::Store;
use varde_core::task;

use crate::{done, now_ms, process, tmux, Paths};
use diff::Diff;
use draft::{Draft, Edit};
use term::{enter, have_terminal, input, leave, quit_on_sigterm, terminated};

const BUSY: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(1000);

/// The groups whose tasks are still being worked on.
const OPEN_GROUPS: [&str; 4] = ["needs_you", "working", "open", "inbox"];

enum Item {
    Header { label: &'static str, count: usize, collapsed: bool },
    Row(Box<Row>),
}

enum Mode {
    Normal,
    /// The keys, over the board.
    Help,
    /// Writing a new task.
    New(Draft),
    /// Asked to confirm throwing the task away.
    Drop(String),
    /// Asked whether closing or parking stops a working session, or
    /// keeps it running.
    Stop(Close, Vec<String>),
    /// Asked to confirm starting a done or parked task, which reopens it.
    Reopen(Start, String),
    /// Typing a recall query.
    Recall(String),
    /// Typing the directory of the project to move the task to.
    Move(String, String),
    /// Typing the directory of the project a new task is in.
    Where(Draft, String),
    /// Asked whether a first run in tmux gets its own worktree, with the
    /// project picked for it.
    Worktree(Start, Asked, Option<PathBuf>),
    /// Picking the project of a first run in tmux: the choices, and which.
    Project(Start, Asked, Vec<String>, usize),
}

/// The task a question about starting was asked of, and its state then.
/// The answer is for that task, not whatever is selected by then.
struct Asked {
    task: String,
    state: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Close {
    Done,
    Park,
}

/// How to start a task: which conversation, and whether to go to it.
#[derive(Clone, Copy)]
struct Start {
    want: Want,
    go: bool,
}

/// What a key asks for that takes the screen from the board.
enum Away {
    Attach(Start),
    Edit(String),
    /// Edit the cairn of this project.
    EditProject(String),
    Diff(Diff),
}

/// What recall found: j/k pick a hit, RET goes to its task on the board,
/// s starts it, Tab searches this project or every one.
struct Found {
    query: String,
    /// Every project, rather than the one the board was started in.
    all: bool,
    /// Nothing in the project: these hits are from every project.
    everywhere: bool,
    hits: Vec<varde_core::memory::Hit>,
    at: usize,
}

struct App<'a> {
    paths: &'a Paths,
    /// The tmux the board runs in, which then hosts the sessions.
    tmux: Option<tmux::Tmux>,
    items: Vec<Item>,
    list: ListState,
    /// The selected task, kept across refreshes as rows move between groups.
    selected: Option<String>,
    /// Where the selection stays after its task moves away (closed,
    /// parked, accepted), so the next key acts on the next row.
    keep_at: Option<usize>,
    /// Tasks marked with space; `a`, `d` and `p` act on them all.
    marked: Vec<String>,
    mode: Mode,
    detail: bool,
    /// The detail pane shows the selected task's project cairn, not the task.
    project_detail: bool,
    /// How far the detail pane is scrolled.
    scroll: u16,
    /// Whether collapsed groups (Parked) are opened.
    expanded: bool,
    /// Recall results, shown in the detail pane until dismissed.
    found: Option<Found>,
    message: String,
    /// Unread tasks whose tmux window has rung; `None` until the first
    /// refresh, which rings nothing.
    rung: Option<HashSet<String>>,
    /// Clear the screen before the next frame.
    redraw: bool,
    /// Something changed what the board shows: read it again before the
    /// next key.
    stale: bool,
    quit: bool,
    /// A newer varde has migrated the store: the board starts again as it.
    upgraded: bool,
}

pub fn run(paths: &Paths) -> Result<()> {
    if !have_terminal() {
        anyhow::bail!("the board needs a terminal; varde list prints it");
    }
    let mut app = App::new(paths, tmux::here());
    app.refresh()?;
    // Read before the board takes the screen, so its warning is not
    // written over it.
    if let Some(w) = crate::config::warning() {
        app.message = format!("error: {w}");
    }
    quit_on_sigterm();
    let mut term = enter()?;
    let result = app.main(&mut term);
    leave();
    if app.upgraded {
        // A newer varde migrated the store under this board: the new
        // binary takes over, in the same terminal, with the same arguments.
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(crate::exe()).args(std::env::args_os().skip(1)).exec();
        anyhow::bail!("varde was upgraded, and starting the new board failed ({e}): run varde again");
    }
    result
}

/// Whether `e` says a newer varde has migrated the store.
fn upgraded(e: &anyhow::Error) -> bool {
    e.downcast_ref::<varde_core::store::NewerSchema>().is_some()
}

impl<'a> App<'a> {
    fn new(paths: &'a Paths, tmux: Option<tmux::Tmux>) -> App<'a> {
        App {
            paths,
            tmux,
            items: Vec::new(),
            list: ListState::default(),
            selected: None,
            keep_at: None,
            marked: Vec::new(),
            mode: Mode::Normal,
            detail: false,
            project_detail: false,
            scroll: 0,
            expanded: false,
            found: None,
            message: String::new(),
            rung: None,
            redraw: false,
            stale: false,
            quit: false,
            upgraded: false,
        }
    }

    fn main(&mut self, term: &mut DefaultTerminal) -> Result<()> {
        let mut refreshed = Instant::now();
        while !self.quit {
            if std::mem::take(&mut self.redraw) {
                term.clear()?;
            }
            term.draw(|f| self.draw(f))?;
            let Some(ready) = input(TICK.saturating_sub(refreshed.elapsed())) else { break };
            if terminated() {
                break;
            }
            // Every key already typed is taken before the board is read
            // again, and the screen drawn, so a held j is one frame and
            // not a refresh a key. A key that changed what the board shows
            // ends the run: the keys after it act on the board as it is.
            if ready {
                loop {
                    self.event(event::read()?, term)?;
                    if self.quit || self.stale || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
            if std::mem::take(&mut self.stale) || refreshed.elapsed() >= TICK {
                refreshed = Instant::now();
                // Nothing that goes wrong in the background closes the board.
                if let Err(e) = self.refresh() {
                    if upgraded(&e) {
                        self.upgraded = true;
                        self.quit = true;
                    }
                    self.message = format!("error: {e:#}");
                }
            }
        }
        Ok(())
    }

    fn event(&mut self, event: Event, term: &mut DefaultTerminal) -> Result<()> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                // A drop waits for the task's sessions to stop: the board
                // says so before it blocks.
                if let (Mode::Drop(task), KeyCode::Char('y')) = (&self.mode, key.code) {
                    let task = task.clone();
                    if self
                        .paths
                        .open(BUSY)
                        .and_then(|s| done::live_runs(&s, &task))
                        .is_ok_and(|r| !r.is_empty())
                    {
                        self.mode = Mode::Normal;
                        self.message = format!("stopping {}'s session…", task::short(&task));
                        term.draw(|f| self.draw(f))?;
                        self.drop_task(&task);
                        return Ok(());
                    }
                }
                if let Some(away) = self.key(key) {
                    self.away(away, term)?;
                }
            }
            Event::Paste(text) => self.paste(&text),
            _ => {}
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<()> {
        let store = self.paths.open(BUSY)?;
        let now = now_ms();
        let mut board = board::build(&store, now, crate::list::local_midnight_ms(), false, &process::alive)?;
        let kicked = crate::sweep::kick(self.paths, &board, now);
        self.watch_tmux(&store, &mut board, now);
        let Board { groups, .. } = board;
        self.items.clear();
        for g in groups {
            let collapsed = g.collapsed && !self.expanded;
            self.items.push(Item::Header { label: g.label, count: g.rows.len(), collapsed });
            if !collapsed {
                self.items.extend(g.rows.into_iter().map(|r| Item::Row(Box::new(r))));
            }
        }
        let index = match self.keep_at.take() {
            Some(at) => (at..self.items.len()).chain((0..at).rev()).find(|&i| self.row(i).is_some()),
            None => self
                .selected
                .as_ref()
                .and_then(|t| self.items.iter().position(|i| matches!(i, Item::Row(r) if &r.task == t)))
                .or_else(|| self.items.iter().position(|i| matches!(i, Item::Row(_)))),
        };
        let was = self.selected.take();
        self.list.select(index);
        self.selected = index.and_then(|i| self.row(i)).map(|r| r.task.clone());
        if self.selected != was {
            self.scroll = 0;
        }
        let mut marked = std::mem::take(&mut self.marked);
        marked.retain(|t| self.row_of(t).is_some());
        self.marked = marked;
        kicked
    }

    fn row(&self, i: usize) -> Option<&Row> {
        match self.items.get(i) {
            Some(Item::Row(r)) => Some(r),
            _ => None,
        }
    }

    fn rows(&self) -> impl Iterator<Item = &Row> {
        self.items.iter().filter_map(|i| match i {
            Item::Row(r) => Some(&**r),
            _ => None,
        })
    }

    fn row_of(&self, task: &str) -> Option<&Row> {
        self.rows().find(|r| r.task == task)
    }

    /// Whether closing `task` would stop a session in the middle of a
    /// turn: any of its live sessions, not only the one its row shows.
    fn mid_turn(&self, task: &str) -> bool {
        let runs = || -> Result<bool> {
            let store = self.paths.open(BUSY)?;
            for (run, ..) in done::live_runs(&store, task)? {
                let status = varde_core::store::view(&store.conn, &run)?.status;
                if status.is_some_and(|s| busy(s.name())) {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        self.row_of(task).is_some_and(|r| busy(r.status)) || runs().unwrap_or(false)
    }

    fn current(&self) -> Option<&Row> {
        self.list.selected().and_then(|i| self.row(i))
    }

    fn current_id(&self) -> Option<String> {
        self.current().map(|r| r.task.clone())
    }

    fn step(&mut self, down: bool) {
        let Some(from) = self.list.selected() else { return };
        let mut i = from;
        loop {
            i = match (down, i) {
                (true, i) if i + 1 < self.items.len() => i + 1,
                (false, i) if i > 0 => i - 1,
                _ => return,
            };
            if self.row(i).is_some() {
                return self.select(i);
            }
        }
    }

    /// To the first row, or the last.
    fn jump(&mut self, last: bool) {
        let rows = (0..self.items.len()).filter(|&i| self.row(i).is_some());
        let i = if last { rows.max() } else { rows.min() };
        if let Some(i) = i {
            self.select(i);
        }
    }

    fn select(&mut self, i: usize) {
        self.selected = self.row(i).map(|r| r.task.clone());
        self.list.select(Some(i));
        self.scroll = 0;
    }

    /// The selection stays where it is when its task moves away.
    fn keep_place(&mut self) {
        self.keep_at = self.list.selected();
    }

    fn key(&mut self, key: KeyEvent) -> Option<Away> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Help => {}
            Mode::New(mut draft) => match draft.key(key) {
                Edit::Typing => self.mode = Mode::New(draft),
                Edit::Create if draft.title.trim().is_empty() => self.message = "a task needs a title".into(),
                Edit::Create => {
                    let dir = self.current().and_then(|r| r.project.clone());
                    let dir = dir
                        .or_else(|| self.paths.open(BUSY).ok().and_then(|s| here_project(&s).ok().flatten()));
                    self.mode = Mode::Where(draft, dir.as_deref().map(draw::tilde).unwrap_or_default());
                }
                Edit::Cancel => {}
            },
            Mode::Where(draft, mut dir) => match key.code {
                KeyCode::Enter => self.create(&draft, &dir),
                KeyCode::Esc => self.mode = Mode::New(draft),
                _ => {
                    edit_line(&mut dir, key);
                    self.mode = Mode::Where(draft, dir);
                }
            },
            Mode::Recall(mut query) => match key.code {
                KeyCode::Enter => self.recall(&query, false),
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    query.pop();
                    self.mode = Mode::Recall(query);
                }
                KeyCode::Char(c) if !ctrl => {
                    query.push(c);
                    self.mode = Mode::Recall(query);
                }
                _ => self.mode = Mode::Recall(query),
            },
            Mode::Move(task, mut dir) => match key.code {
                KeyCode::Enter => self.move_task(&task, &dir),
                KeyCode::Esc => {}
                _ => {
                    edit_line(&mut dir, key);
                    self.mode = Mode::Move(task, dir);
                }
            },
            Mode::Project(start, asked, choices, i) => {
                let n = choices.len();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.mode = Mode::Project(start, asked, choices, (i + 1) % n)
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.mode = Mode::Project(start, asked, choices, (i + n - 1) % n)
                    }
                    KeyCode::Enter => self.answer(asked, start, None, Some(PathBuf::from(&choices[i]))),
                    _ => self.message = "cancelled".into(),
                }
            }
            Mode::Worktree(start, asked, project) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.answer(asked, start, Some(true), project),
                KeyCode::Char('n') => self.answer(asked, start, Some(false), project),
                _ => self.message = "cancelled".into(),
            },
            Mode::Drop(task) => match key.code {
                KeyCode::Char('y') => self.drop_task(&task),
                _ => self.message = "cancelled".into(),
            },
            Mode::Reopen(start, task) => match key.code {
                KeyCode::Char('y') if self.current_id().as_deref() == Some(task.as_str()) => {
                    return self.begin(start)
                }
                // Asked by `s` on a recall hit, which may be too old for the board.
                KeyCode::Char('y') if self.picked().as_deref() == Some(task.as_str()) => {
                    let reopened = self.paths.open(BUSY).and_then(|s| crate::start::reopen(&s.conn, &task));
                    if let Err(e) = reopened {
                        self.report(Err(e));
                        return None;
                    }
                    return self.start_on_board(&task, start);
                }
                _ => self.message = "cancelled".into(),
            },
            Mode::Stop(how, ids) => match key.code {
                KeyCode::Char('s') => self.close_now(how, ids, true),
                KeyCode::Char('k') => self.close_now(how, ids, false),
                _ => {
                    if ids.len() > 1 {
                        self.marked = ids;
                    }
                    self.message = "cancelled".into();
                }
            },
            Mode::Normal => {
                self.message.clear();
                if self.found.is_some() && key.code == KeyCode::Char('s') {
                    return self.start_hit();
                }
                if self.found.is_some() && self.recall_key(key.code) {
                    return None;
                }
                let start = |want, go| Some(Away::Attach(Start { want, go }));
                let away = match key.code {
                    KeyCode::Char('c') if ctrl => {
                        self.quit = true;
                        None
                    }
                    KeyCode::Char('l') if ctrl => {
                        self.redraw = true;
                        None
                    }
                    KeyCode::Char('d') | KeyCode::PageDown if ctrl || key.code == KeyCode::PageDown => {
                        self.scroll = self.scroll.saturating_add(10);
                        None
                    }
                    KeyCode::Char('u') | KeyCode::PageUp if ctrl || key.code == KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(10);
                        None
                    }
                    _ if ctrl => None,
                    KeyCode::Enter => start(Want::Continue, true),
                    KeyCode::Char('s') => start(Want::Continue, false),
                    KeyCode::Char('R') => start(Want::Fresh, true),
                    KeyCode::Char('F') => start(Want::Fork, true),
                    KeyCode::Char('e') => self.current_id().map(Away::Edit),
                    KeyCode::Char('E') => match self.current() {
                        Some(r) => match r.project.clone() {
                            Some(p) => Some(Away::EditProject(p)),
                            None => {
                                self.message =
                                    "this task has no project yet: its first start picks one".into();
                                None
                            }
                        },
                        None => None,
                    },
                    KeyCode::Char('=') => self.current_id().and_then(|id| self.changes(&id)),
                    code => {
                        self.normal(code);
                        None
                    }
                };
                return match away {
                    // Starting a closed task reopens it: one key too easily
                    // pressed for that.
                    Some(Away::Attach(start)) => match self.current() {
                        Some(r) if matches!(r.state.as_str(), "done" | "parked" | "dropped") => {
                            self.mode = Mode::Reopen(start, r.task.clone());
                            None
                        }
                        _ => self.begin(start),
                    },
                    away => away,
                };
            }
        }
        None
    }

    /// A key for the recall pane: j/k pick a hit, RET goes to its task,
    /// Tab widens or narrows the search; `s` is `start_hit`. Whether it took the key.
    fn recall_key(&mut self, code: KeyCode) -> bool {
        let Some(found) = self.found.as_mut() else { return false };
        let n = found.hits.len();
        match code {
            KeyCode::Char('j') | KeyCode::Down if n > 0 => found.at = (found.at + 1).min(n - 1),
            KeyCode::Char('k') | KeyCode::Up => found.at = found.at.saturating_sub(1),
            KeyCode::Tab => {
                let (query, all) = (found.query.clone(), !found.all);
                self.recall(&query, all);
            }
            KeyCode::Enter => {
                if let Some(hit) = found.hits.get(found.at) {
                    let (task, short) = (hit.task.clone(), hit.short.clone());
                    self.go_to(&task, &short);
                }
            }
            _ => return false,
        }
        true
    }

    /// The task of the picked recall hit.
    fn picked(&self) -> Option<String> {
        self.found.as_ref().and_then(|f| f.hits.get(f.at)).map(|h| h.task.clone())
    }

    /// `s` in the recall pane: start the picked hit's task, asking first
    /// if it is closed, as RET on its row does.
    fn start_hit(&mut self) -> Option<Away> {
        let task = self.picked()?;
        let state: String = self.paths.open(BUSY).ok().and_then(|s| {
            s.conn.query_row("SELECT state FROM task WHERE id = ?1", [&task], |r| r.get(0)).ok()
        })?;
        let start = Start { want: Want::Continue, go: true };
        if matches!(state.as_str(), "done" | "parked" | "dropped") {
            self.mode = Mode::Reopen(start, task);
            return None;
        }
        self.start_on_board(&task, start)
    }

    /// Close the recall pane on `task`, selected on the board now, so
    /// that starting acts on it, and start it.
    fn start_on_board(&mut self, task: &str, start: Start) -> Option<Away> {
        self.found = None;
        self.selected = Some(task.to_string());
        self.keep_at = None;
        if let Err(e) = self.refresh() {
            self.report(Err(e));
            return None;
        }
        if self.current_id().as_deref() != Some(task) {
            self.message = format!("{} is not on the board: varde attach {0}", task::short(task));
            return None;
        }
        self.begin(start)
    }

    /// Select `task` on the board, opening Parked for it if need be.
    fn go_to(&mut self, task: &str, short: &str) {
        let state: Option<String> = self.paths.open(BUSY).ok().and_then(|s| {
            s.conn.query_row("SELECT state FROM task WHERE id = ?1", [task], |r| r.get(0)).ok()
        });
        match state.as_deref() {
            Some("parked") => self.expanded = true,
            _ if self.row_of(task).is_some() => {}
            Some(state) => {
                self.message = format!("{short} is {state} and not on the board: varde show {short}");
                return;
            }
            None => return,
        }
        self.found = None;
        self.selected = Some(task.to_string());
        self.keep_at = None;
        self.stale = true;
    }

    /// A key on the board that stays on the board.
    fn normal(&mut self, code: KeyCode) {
        match code {
            // As in the help: a pane open is what `q` closes.
            KeyCode::Char('q') if self.found.is_some() => self.found = None,
            KeyCode::Char('q') if self.detail => self.detail = false,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if !self.marked.is_empty() => self.marked.clear(),
            KeyCode::Esc if self.found.is_some() => self.found = None,
            KeyCode::Esc if self.detail => self.detail = false,
            KeyCode::Esc => self.message = "q quits".into(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('/') => self.mode = Mode::Recall(String::new()),
            KeyCode::Char('j') | KeyCode::Down => self.step(true),
            KeyCode::Char('k') | KeyCode::Up => self.step(false),
            KeyCode::Home => self.jump(false),
            KeyCode::Char('G') | KeyCode::End => self.jump(true),
            KeyCode::Char('v') => self.show_detail(false),
            KeyCode::Char('V') => self.show_detail(true),
            KeyCode::Char('n') => self.mode = Mode::New(Draft::default()),
            KeyCode::Char('m') => {
                if let Some(r) = self.current() {
                    let dir = r.project.as_deref().map(draw::tilde).unwrap_or_default();
                    self.mode = Mode::Move(r.task.clone(), dir);
                }
            }
            KeyCode::Char(' ') => self.mark(),
            KeyCode::Char('a') => self.accept(),
            KeyCode::Char('d') => self.close(Close::Done),
            KeyCode::Char('p') => self.close(Close::Park),
            KeyCode::Char('x') => {
                if let Some(id) = self.current_id() {
                    self.mode = Mode::Drop(id);
                }
            }
            KeyCode::Char('g') => {
                self.redraw = true;
                self.stale = true;
                self.message = "refreshed".into();
            }
            KeyCode::Tab => {
                self.expanded = !self.expanded;
                self.stale = true;
            }
            _ => {}
        }
    }

    /// Open the detail pane on the task, or on its project cairn; the same
    /// key again closes it.
    fn show_detail(&mut self, project: bool) {
        self.detail = !(self.detail && self.project_detail == project);
        self.project_detail = project;
        self.scroll = 0;
    }

    /// Pasted text goes into the prompt being typed, and nowhere else.
    fn paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::New(draft) => draft.paste(text),
            Mode::Recall(query) => query.push_str(&text.split_whitespace().collect::<Vec<_>>().join(" ")),
            Mode::Move(_, dir) | Mode::Where(_, dir) => dir.push_str(text.trim()),
            Mode::Normal => self.message = "pasted text goes into a prompt: n new task, / recall".into(),
            _ => {}
        }
    }

    /// Leave the screen to `away`, and take it back after.
    fn away(&mut self, away: Away, term: &mut DefaultTerminal) -> Result<()> {
        leave();
        let result = match away {
            Away::Attach(start) => self.attach_here(start.want),
            Away::Edit(id) => {
                crate::memory_cmd::edit(self.paths, &id, false, false).map(|s| saved("cairn", s))
            }
            Away::EditProject(project) => {
                crate::memory_cmd::edit_project(self.paths, Path::new(&project), false, false)
                    .map(|s| saved("project cairn", s))
            }
            Away::Diff(diff) => self.diff(&diff),
        };
        *term = enter()?;
        self.redraw = true;
        self.report(result);
        Ok(())
    }

    /// Say how an action went. Any action may have changed the store.
    fn report(&mut self, result: Result<String>) {
        self.stale = true;
        self.message = match result {
            Ok(m) => m,
            Err(e) => format!("error: {e:#}"),
        };
    }

    /// Create the task in the project `dir` is in.
    fn create(&mut self, draft: &Draft, dir: &str) {
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            let project = crate::start::project_of(&store, Path::new(&home_in(dir)))?.display().to_string();
            let body = Some(draft.body.trim()).filter(|b| !b.is_empty());
            let id = task::create(&store, draft.title.trim(), body, Some(&project), None, now_ms())?;
            self.selected = Some(id.clone());
            Ok(format!("created {}; RET starts it", task::short(&id)))
        })();
        self.report(result);
    }

    /// Search the cairns, in the project the board was started in, as
    /// `varde recall` does, or in every one.
    fn recall(&mut self, query: &str, all: bool) {
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            let here = here_project(&store).ok().flatten();
            let all = all || here.is_none();
            let scope = if all { None } else { here.as_deref() };
            let asker = varde_core::memory::Asker { source: "user", run: None };
            let found = varde_core::memory::recall(&store, query, scope, None, 10, asker, now_ms())?;
            let everywhere = found.everywhere;
            self.found = Some(Found { query: query.to_string(), all, everywhere, hits: found.hits, at: 0 });
            self.scroll = 0;
            Ok(String::new())
        })();
        self.report(result);
    }

    /// Move `task` to the project `dir` is in, as `varde set --project`
    /// does; a refusal is the message.
    fn move_task(&mut self, task: &str, dir: &str) {
        let dir = home_in(dir);
        let result = self.paths.open(BUSY).and_then(|s| crate::tasks::move_to(&s, task, Path::new(&dir)));
        self.report(result);
    }

    fn mark(&mut self) {
        let Some(id) = self.current_id() else { return };
        match self.marked.iter().position(|t| *t == id) {
            Some(i) => {
                self.marked.remove(i);
            }
            None => self.marked.push(id),
        }
        self.step(true);
    }

    /// The marked tasks, or else the one at point. Taking them clears the marks.
    fn targets(&mut self) -> Vec<String> {
        if self.marked.is_empty() {
            self.current_id().into_iter().collect()
        } else {
            std::mem::take(&mut self.marked)
        }
    }

    /// Do `act` to each of `ids`, reporting its own message for one task
    /// and a count for several.
    fn each(&mut self, ids: &[String], verb: &str, act: impl Fn(&Store, &str) -> Result<String>) {
        if ids.is_empty() {
            return;
        }
        self.keep_place();
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            let mut last = String::new();
            for id in ids {
                last = act(&store, id)?;
            }
            Ok(if ids.len() == 1 { last } else { format!("{verb} {} tasks", ids.len()) })
        })();
        self.report(result);
    }

    fn accept(&mut self) {
        let ids = self.targets();
        let inbox: Vec<String> =
            ids.into_iter().filter(|id| self.row_of(id).is_some_and(|r| r.inbox)).collect();
        if inbox.is_empty() {
            self.message = "nothing to accept: a is for Inbox tasks".into();
            return;
        }
        self.each(&inbox, "accepted", |s, id| task::accept(s, id).map(|_| "accepted".into()));
    }

    /// Close or park the targets, asking first when that stops a session
    /// in the middle of something.
    fn close(&mut self, how: Close) {
        let ids = self.targets();
        if ids.iter().any(|id| self.mid_turn(id)) {
            self.mode = Mode::Stop(how, ids);
        } else {
            self.close_now(how, ids, true);
        }
    }

    fn close_now(&mut self, how: Close, ids: Vec<String>, stop: bool) {
        match how {
            Close::Done => self
                .each(&ids, "closed", |s, id| done::close(s, id, closing(!stop)).map(|r| done::describe(&r))),
            Close::Park => {
                self.each(&ids, "parked", |s, id| done::shelve(s, id, !stop).map(|r| done::describe_park(&r)))
            }
        }
    }

    fn drop_task(&mut self, id: &str) {
        self.keep_place();
        let result =
            self.paths.open(BUSY).and_then(|s| done::reject(&s, id)).map(|r| done::describe_drop(&r));
        self.report(result);
    }
}

/// What an edit of `what` did.
fn saved(what: &str, changed: bool) -> String {
    if changed {
        format!("{what} saved")
    } else {
        format!("{what} unchanged")
    }
}

/// Mid-turn: stopping it now loses what it is doing.
fn busy(status: &str) -> bool {
    matches!(status, "working" | "delegated" | "blocked")
}

/// The project the board was started in, as `varde recall` takes it;
/// `None` outside a repository.
fn here_project(store: &Store) -> Result<Option<String>> {
    let root = varde_core::project::of(&store.conn, &std::env::current_dir()?);
    Ok(root.join(".git").exists().then(|| root.display().to_string()))
}

/// A directory as typed, with a leading `~` for the home directory.
fn home_in(dir: &str) -> String {
    match dir.trim().strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", std::env::var("HOME").unwrap_or_default())
        }
        _ => dir.trim().to_string(),
    }
}

/// A key typed into a one-line prompt: Backspace, C-u clearing it, or a
/// character.
fn edit_line(line: &mut String, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Backspace => {
            line.pop();
        }
        KeyCode::Char('u') if ctrl => line.clear(),
        KeyCode::Char(c) if !ctrl => line.push(c),
        _ => {}
    }
}

/// Closing from the board: never merging or removing.
fn closing(keep_running: bool) -> done::Options {
    done::Options { remove: false, force: false, discard: false, keep_running }
}

#[cfg(test)]
mod tests {
    use super::draw::{render, FOOTER};
    use super::*;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        render(w, h, |f| app.draw(f))
    }

    fn key(app: &mut App, code: KeyCode) -> Option<Away> {
        let away = app.key(KeyEvent::new(code, KeyModifiers::NONE));
        app.refresh().unwrap();
        away
    }

    /// Paths to a store of its own, gone with the directory.
    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { home: dir.path().to_path_buf() };
        (dir, paths)
    }

    /// A board over a store of its own with these tasks, the last on top
    /// of Open.
    fn board<'a>(paths: &'a Paths, titles: &[&str]) -> App<'a> {
        let store = paths.open(BUSY).unwrap();
        for (i, t) in titles.iter().enumerate() {
            task::create(&store, t, None, None, None, 1_000 + i as i64).unwrap();
        }
        let mut app = App::new(paths, None);
        app.refresh().unwrap();
        app
    }

    #[test]
    fn esc_never_quits_and_a_paste_is_never_keys() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        key(&mut app, KeyCode::Esc);
        assert!(!app.quit);
        assert_eq!(app.message, "q quits");
        app.paste("qqq");
        assert!(!app.quit);
        key(&mut app, KeyCode::Char('?'));
        assert!(screen(&mut app, 80, 24).contains("start a fresh conversation"));
        key(&mut app, KeyCode::Char('q'));
        assert!(!app.quit, "a key closes the help, and only that");
        key(&mut app, KeyCode::Char('v'));
        key(&mut app, KeyCode::Char('q'));
        assert!(!app.quit && !app.detail, "q closes the detail pane, and only that");
        key(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn accept_is_for_the_inbox_and_the_selection_keeps_its_place() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Open one"]);
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.message, "nothing to accept: a is for Inbox tasks");
        {
            let store = paths.open(BUSY).unwrap();
            for t in ["In one", "In two"] {
                let id = task::create(&store, t, None, None, None, 5_000).unwrap();
                store.conn.execute("UPDATE task SET inbox = 1 WHERE id = ?1", [&id]).unwrap();
            }
        }
        app.refresh().unwrap();
        while app.current().is_some_and(|r| !r.inbox) {
            app.step(true);
        }
        let first = app.current_id().unwrap();
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.message, "accepted");
        let now = app.current().unwrap();
        assert!(now.task != first && now.inbox, "on the next Inbox row, not the accepted task");
    }

    fn state(paths: &Paths, id: &str) -> String {
        let store = paths.open(BUSY).unwrap();
        store.conn.query_row("SELECT state FROM task WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn parking_a_busy_task_asks_to_stop_or_keep_and_anything_else_cancels() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Busy"]);
        let id = app.current_id().unwrap();
        app.mode = Mode::Stop(Close::Park, vec![id.clone()]);
        let s = screen(&mut app, 120, 10);
        assert!(s.contains("Park it: [s]top it, [k]eep it running, Esc cancels"), "{s}");
        key(&mut app, KeyCode::Char('n'));
        assert_eq!((app.message.as_str(), state(&paths, &id).as_str()), ("cancelled", "open"));
        app.mode = Mode::Stop(Close::Done, vec![id.clone()]);
        assert!(screen(&mut app, 120, 10).contains("Close it: [s]top"));
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(state(&paths, &id), "done");
    }

    #[test]
    fn closing_asks_when_any_session_is_mid_turn_not_only_the_one_shown() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Two sessions"]);
        let id = app.current_id().unwrap();
        let mut sleeper = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        {
            // Both live, each in a process of its own: a fork at work, and
            // the original waiting with a newer event, which the row shows.
            let store = paths.open(BUSY).unwrap();
            let me = std::process::id();
            for (run, kinds, me) in [
                ("FORK", &[("started", 5), ("prompt", 10)], sleeper.id()),
                ("ORIG", &[("started", 4), ("turn_end", 20)], me),
            ] {
                let start = process::start_time(me).unwrap_or(0);
                store
                    .conn
                    .execute(
                        "INSERT INTO run (id, task, agent, native_id, relation, started, launch, pid, pid_start)
                         VALUES (?1, ?2, 'claude', ?1, 'fresh', 1, 'managed', ?3, ?4)",
                        rusqlite::params![run, id, me, start],
                    )
                    .unwrap();
                for (kind, ts) in kinds {
                    store
                        .conn
                        .execute(
                            "INSERT INTO event (run, kind, ts, raw) VALUES (?1, ?2, ?3, '{}')",
                            rusqlite::params![run, kind, ts],
                        )
                        .unwrap();
                }
            }
        }
        app.refresh().unwrap();
        assert_eq!(app.row_of(&id).unwrap().status, "waiting");
        key(&mut app, KeyCode::Char('d'));
        assert!(matches!(app.mode, Mode::Stop(Close::Done, _)), "d asks first");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.mode, Mode::Stop(Close::Park, _)), "p asks first");
        key(&mut app, KeyCode::Esc);
        assert_eq!(state(&paths, &id), "open");
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
    }

    #[test]
    fn prompts_and_messages_too_long_for_80_columns_take_two_rows() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two"]);
        let ids: Vec<String> = app.rows().map(|r| r.task.clone()).collect();
        app.mode = Mode::Stop(Close::Park, ids);
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("[k]eep them running, Esc cancels"), "{s}");
        app.mode = Mode::Drop(app.current_id().unwrap());
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("Throw") && s.contains("brings it back.  [y/N]"), "{s}");
        app.mode = Mode::Normal;
        app.message = "another session of this task is live in /srv/work/repos/alpha; started abcdef in a window of its own".into();
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("another session") && s.contains("a window of its own"), "{s}");
        // A short one keeps to one row, under a board one row taller.
        app.message = "refreshed".into();
        let s = screen(&mut app, 80, 24);
        assert!(s.lines().nth(23).unwrap().starts_with("refreshed"), "{s}");
    }

    #[test]
    fn starting_a_closed_task_asks_first() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Finished"]);
        let id = app.current_id().unwrap();
        done::close(&paths.open(BUSY).unwrap(), &id, closing(true)).unwrap();
        app.refresh().unwrap();
        assert_eq!(app.current_id().as_deref(), Some(id.as_str()));
        assert!(key(&mut app, KeyCode::Enter).is_none());
        assert!(screen(&mut app, 100, 10).contains("is done: reopen and start it?  [y/N]"));
        key(&mut app, KeyCode::Char('n'));
        assert_eq!((app.message.as_str(), state(&paths, &id).as_str()), ("cancelled", "done"));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(key(&mut app, KeyCode::Char('y')), Some(Away::Attach(_))), "y starts it");
    }

    #[test]
    fn recall_hits_are_picked_and_taken_to_on_the_board() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Cache by locale", "Something else", "Locale fallback"]);
        {
            let store = paths.open(BUSY).unwrap();
            let ids: Vec<String> = store
                .conn
                .prepare("SELECT id FROM task")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for id in ids {
                let text =
                    "Status: Looked at the locale.\nFindings:\n- The locale matters.\nDecisions:\nOpen:\n";
                let cairn = varde_core::cairn::Cairn::from_edit(text, &Default::default());
                varde_core::memory::put_cairn(&store.conn, &id, &cairn, 0, "user", None, now_ms()).unwrap();
            }
        }
        let before = app.current_id();
        app.recall("locale", true);
        let found = app.found.as_ref().unwrap();
        assert!(found.hits.len() >= 2);
        let second = found.hits[1].task.clone();
        // Its keys fit on the query's line at 80 columns.
        let f = app.found.as_mut().unwrap();
        (f.query, f.all) = ("CacheKey region".into(), false);
        let s = screen(&mut app, 80, 24);
        assert!(s.lines().any(|l| l.contains("recall: CacheKey region") && l.contains("s start")), "{s}");
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current_id(), before, "j picks a hit, not a board row");
        assert!(key(&mut app, KeyCode::Enter).is_none(), "RET on a hit starts nothing");
        assert!(app.found.is_none());
        assert_eq!(app.current_id(), Some(second));
    }

    #[test]
    fn s_on_a_recall_hit_starts_its_task_even_one_gone_from_the_board() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Cache by locale", "Something else"]);
        let ids: Vec<String> = app.rows().map(|r| r.task.clone()).collect();
        {
            let store = paths.open(BUSY).unwrap();
            for id in &ids {
                let text = "Status: Looked.\nFindings:\n- The locale matters.\nDecisions:\nOpen:\n";
                let cairn = varde_core::cairn::Cairn::from_edit(text, &Default::default());
                varde_core::memory::put_cairn(&store.conn, id, &cairn, 0, "user", None, now_ms()).unwrap();
            }
        }
        let old = ids
            .iter()
            .find(|id| app.row_of(id).is_some_and(|r| r.title == "Cache by locale"))
            .unwrap()
            .clone();
        // Done long ago: off the board.
        paths
            .open(BUSY)
            .unwrap()
            .conn
            .execute("UPDATE task SET state = 'done', closed = 1 WHERE id = ?1", [&old])
            .unwrap();
        app.refresh().unwrap();
        assert!(app.row_of(&old).is_none());

        app.recall("locale", true);
        let at = app.found.as_ref().unwrap().hits.iter().position(|h| h.task == old).unwrap();
        app.found.as_mut().unwrap().at = at;
        assert!(key(&mut app, KeyCode::Char('s')).is_none(), "a closed one asks first");
        assert!(screen(&mut app, 100, 10).contains("reopen and start it?  [y/N]"));
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(state(&paths, &old), "done");
        assert!(key(&mut app, KeyCode::Char('s')).is_none());
        assert!(matches!(key(&mut app, KeyCode::Char('y')), Some(Away::Attach(_))), "y starts it");
        assert_eq!(state(&paths, &old), "open");
        assert!(app.found.is_none());
        assert_eq!(app.current_id().as_deref(), Some(old.as_str()), "the start is of the hit");

        // An open one starts straight away.
        let other = ids.into_iter().find(|id| *id != old).unwrap();
        app.recall("locale", true);
        let at = app.found.as_ref().unwrap().hits.iter().position(|h| h.task == other).unwrap();
        app.found.as_mut().unwrap().at = at;
        assert!(matches!(key(&mut app, KeyCode::Char('s')), Some(Away::Attach(_))));
        assert_eq!(app.current_id(), Some(other));
    }

    #[test]
    fn a_store_a_newer_varde_migrated_is_told_apart() {
        let (_dir, paths) = temp_paths();
        paths.open(BUSY).unwrap().conn.pragma_update(None, "user_version", 999).unwrap();
        let e = paths.open(BUSY).err().unwrap();
        assert!(upgraded(&e), "{e:#}");
        assert!(!upgraded(&anyhow::anyhow!("something else")));
    }

    #[test]
    fn the_project_chooser_keeps_its_keys_in_view() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Where"]);
        let long = format!("/srv/{}/project", "deep/".repeat(30));
        app.mode = Mode::Project(Start { want: Want::Continue, go: true }, asked("T"), vec![long], 0);
        let s = screen(&mut app, 80, 10);
        assert!(s.contains("…") && s.contains("project  1/1  j/k other  RET start  Esc cancel"), "{s}");
    }

    #[test]
    fn g_and_home_jump_to_the_ends() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two", "Three"]);
        let first = app.current_id();
        key(&mut app, KeyCode::Char('G'));
        assert_eq!(app.current().unwrap().title, "One", "the oldest is last");
        key(&mut app, KeyCode::Home);
        assert_eq!(app.current_id(), first);
    }

    #[test]
    fn home_shows_the_first_group_header_again() {
        let (_dir, paths) = temp_paths();
        let titles: Vec<String> = (0..30).map(|i| format!("Task {i}")).collect();
        let mut app = board(&paths, &titles.iter().map(String::as_str).collect::<Vec<_>>());
        let Item::Header { label, .. } = &app.items[0] else { panic!("a header first") };
        let header = format!("{label} (");
        // The detail pane shrinks the list, which scrolls; closing it
        // must not leave the top scrolled off.
        for code in [KeyCode::End, KeyCode::Char('v'), KeyCode::Char('v'), KeyCode::Home] {
            key(&mut app, code);
            screen(&mut app, 80, 24);
        }
        let s = screen(&mut app, 80, 24);
        assert!(s.lines().nth(1).unwrap().contains(&header), "{s}");
    }

    #[test]
    fn a_diff_with_nothing_in_it_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { home: dir.path().join("state") };
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap();
            assert!(ok.success());
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        git(&["add", "a"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a"]);
        let mut app = board(&paths, &["Shared"]);
        {
            let store = paths.open(BUSY).unwrap();
            store.conn.execute("UPDATE task SET project = ?1", [repo.to_string_lossy()]).unwrap();
        }
        app.refresh().unwrap();
        assert!(key(&mut app, KeyCode::Char('=')).is_none());
        assert!(
            app.message.starts_with("nothing uncommitted in") && app.message.contains("share"),
            "{}",
            app.message
        );
        // A new size: a same-size rewrite within the clock's tick looks
        // unchanged to git.
        std::fs::write(repo.join("a"), "22").unwrap();
        let Some(Away::Diff(diff)) = key(&mut app, KeyCode::Char('=')) else { panic!("no diff") };
        assert!(diff.shared);
        // The diff stays on screen until a key, with what it was.
        let footer = diff.footer();
        assert!(
            footer.contains("uncommitted in") && footer.ends_with("Any key returns to the board."),
            "{footer}"
        );
        let own = Diff { dir: diff.dir.clone(), from: None, shared: false, index: None };
        assert_eq!(own.footer(), "Any key returns to the board.");
    }

    #[test]
    fn a_diff_shows_untracked_files_and_leaves_the_index_alone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { home: dir.path().join("state") };
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
        git(&["add", "a", ".gitignore"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a"]);
        let base = git(&["rev-parse", "HEAD"]).trim().to_string();
        let mut app = board(&paths, &["Shared"]);
        {
            let store = paths.open(BUSY).unwrap();
            store.conn.execute("UPDATE task SET project = ?1", [repo.to_string_lossy()]).unwrap();
        }
        app.refresh().unwrap();
        std::fs::write(repo.join("ignored"), "x").unwrap();
        assert!(key(&mut app, KeyCode::Char('=')).is_none(), "an ignored file is no change");
        std::fs::write(repo.join("made.rs"), "new file").unwrap();
        let Some(Away::Diff(mut diff)) = key(&mut app, KeyCode::Char('=')) else { panic!("no diff") };
        let shown = |diff: &Diff| {
            String::from_utf8(diff.git().arg("diff").args(&diff.from).output().unwrap().stdout).unwrap()
        };
        assert!(shown(&diff).contains("+new file") && !shown(&diff).contains("ignored"));
        // From where a worktree branched off too.
        diff.from = Some(base);
        assert!(shown(&diff).contains("+new file"));
        assert_eq!(git(&["status", "--porcelain"]), "?? made.rs\n");
    }

    #[test]
    fn a_wide_title_keeps_the_columns_in_place() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Plain title", "Ünïcødé 日本語のタイトル 🚀 and a long tail of words"]);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        // The project column, `-` here, is at the same place on every row.
        let project_at = |y: u16| (0..100).rev().find(|&x| buf[(x, y)].symbol() == "-");
        let rows: Vec<u16> = (0..10).filter(|&y| (0..100).any(|x| buf[(x, y)].symbol() == "…")).collect();
        let plain = (0..10).find(|&y| (0..100).any(|x| buf[(x, y)].symbol() == "P")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(project_at(rows[0]), project_at(plain));
    }

    #[test]
    fn a_title_stored_with_escapes_never_reaches_the_terminal_raw() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Plain"]);
        let id = app.current_id().unwrap();
        {
            // As stored before titles were cleaned.
            let store = paths.open(BUSY).unwrap();
            let raw = "\x1b]0;PWNED\x07\x1b[2JEvil\ttitle";
            store.conn.execute("UPDATE task SET title = ?2, body = ?2 WHERE id = ?1", [&id, raw]).unwrap();
        }
        app.refresh().unwrap();
        key(&mut app, KeyCode::Char('v'));
        let s = screen(&mut app, 100, 24);
        assert!(!s.contains(|c: char| c.is_control() && c != '\n'), "{s:?}");
        assert!(s.contains("[2JEvil title"), "{s}");
    }

    fn asked(task: &str) -> Asked {
        Asked { task: task.into(), state: "open".into() }
    }

    fn runs(paths: &Paths, task: &str) -> i64 {
        let store = paths.open(BUSY).unwrap();
        store.conn.query_row("SELECT count(*) FROM run WHERE task = ?1", [task], |r| r.get(0)).unwrap()
    }

    /// A tmux server of the test's own, killed when dropped, whose windows
    /// run `cat`, so nothing typed into one runs.
    struct Server(PathBuf);

    impl Server {
        fn start(dir: &Path) -> Option<Server> {
            let s = Server(dir.join("tmux.sock"));
            (s.tmux(&["-f", "/dev/null", "new-session", "-d", "-s", "board", "-c", "/", "cat"])
                && s.tmux(&["set", "-g", "default-command", "cat"]))
            .then_some(s)
        }

        fn tmux(&self, args: &[&str]) -> bool {
            let mut c = std::process::Command::new("tmux");
            c.arg("-S").arg(&self.0).args(args).env_remove("TMUX");
            c.output().is_ok_and(|o| o.status.success())
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.tmux(&["kill-server"]);
        }
    }

    #[test]
    fn an_answer_starts_the_task_it_was_asked_of_not_the_selected_one() {
        let (dir, paths) = temp_paths();
        let mut app = board(&paths, &["Gone", "Asked", "Other"]);
        let ids: Vec<String> = app.rows().map(|r| r.task.clone()).collect();
        let (other, wanted, gone) = (&ids[0], &ids[1], &ids[2]);
        // Parked elsewhere while the question was up: nothing starts.
        done::shelve(&paths.open(BUSY).unwrap(), gone, false).unwrap();
        app.mode = Mode::Worktree(Start { want: Want::Continue, go: false }, asked(gone), None);
        key(&mut app, KeyCode::Char('y'));
        assert!(app.message.ends_with("is parked now; not started"), "{}", app.message);

        let Some(_server) = Server::start(dir.path()) else { return eprintln!("no tmux; skipped") };
        app.tmux = Some(tmux::at(dir.path().join("tmux.sock")));
        assert_eq!(app.current_id().as_ref(), Some(other));
        let project = dir.path().join("p");
        std::fs::create_dir(&project).unwrap();
        let start = Start { want: Want::Continue, go: false };
        app.mode = Mode::Project(start, asked(wanted), vec![project.display().to_string()], 0);
        key(&mut app, KeyCode::Enter);
        assert!(app.message.contains("in a window of its own"), "{}", app.message);
        assert_eq!((runs(&paths, wanted), runs(&paths, other), runs(&paths, gone)), (1, 0, 0));
    }

    #[test]
    fn the_board_closes_only_its_stores_windows_of_tasks_it_knows_closed() {
        let (dir, paths) = temp_paths();
        let mut app = board(&paths, &["Parked"]);
        let parked = app.current_id().unwrap();
        done::shelve(&paths.open(BUSY).unwrap(), &parked, false).unwrap();
        let Some(server) = Server::start(dir.path()) else { return eprintln!("no tmux; skipped") };
        app.tmux = Some(tmux::at(dir.path().join("tmux.sock")));
        let home = app.home();
        // Idle windows a board opened: of this store's parked task, of
        // another store's, and of a task this store has never seen.
        for (name, task, store) in [
            ("mine", parked.as_str(), home.as_str()),
            ("theirs", &parked, "/other"),
            ("unknown", "01X", &home),
        ] {
            assert!(server.tmux(&["new-window", "-d", "-n", name, "sh"]));
            for (k, v) in [("@varde_task", task), ("@varde_owned", "1"), ("@varde_home", store)] {
                assert!(server.tmux(&["set-option", "-p", "-t", &format!("board:{name}"), k, v]));
            }
        }
        let names = || {
            let out = std::process::Command::new("tmux")
                .arg("-S")
                .arg(&server.0)
                .args(["list-windows", "-a", "-F", "#{window_name}"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        // The shells start before they count as idle.
        for _ in 0..100 {
            app.refresh().unwrap();
            if !names().contains("mine") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let left = names();
        assert!(!left.contains("mine") && left.contains("theirs") && left.contains("unknown"), "{left}");
    }

    #[test]
    fn a_key_reads_the_board_again_only_when_it_changed_it() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two"]);
        for code in [KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Char('v'), KeyCode::Char(' ')] {
            app.key(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(!app.stale, "{code:?}");
        }
        app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(app.stale);
    }

    #[test]
    fn no_terminal_is_too_small_to_draw_on() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two"]);
        for code in [KeyCode::Char('n'), KeyCode::Char('?'), KeyCode::Char('v')] {
            app.mode = Mode::Normal;
            key(&mut app, code);
            for (w, h) in [(1, 1), (5, 3), (20, 6), (30, 7), (80, 2)] {
                screen(&mut app, w, h);
            }
        }
        app.mode =
            Mode::Project(Start { want: Want::Continue, go: true }, asked("T"), vec!["/srv/p".into()], 0);
        app.recall("one", true);
        for (w, h) in [(1, 1), (5, 3), (20, 6)] {
            screen(&mut app, w, h);
        }
    }

    #[test]
    fn the_footer_and_help_fit_80_columns_and_the_cairn_shows_at_80x24() {
        assert!(FOOTER.chars().count() <= 80);
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two"]);
        {
            let store = paths.open(BUSY).unwrap();
            let id = app.current_id().unwrap();
            let text =
                "Status: Cause found.\nFindings:\n- The cache key omits the locale.\nDecisions:\nOpen:\n";
            let cairn = varde_core::cairn::Cairn::from_edit(text, &Default::default());
            varde_core::memory::put_cairn(&store.conn, &id, &cairn, 0, "user", None, now_ms()).unwrap();
            store
                .conn
                .execute(
                    "UPDATE task SET project = ?2, body = ?3 WHERE id = ?1",
                    [
                        &id,
                        "/a/long/path/to/a/project/that/fills/the/pane",
                        "A body\nof lines\nthat goes on\nand on\nand on",
                    ],
                )
                .unwrap();
        }
        app.refresh().unwrap();
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("? help"), "{s}");
        key(&mut app, KeyCode::Char('v'));
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("The cache key omits the locale."), "the cairn is in view:\n{s}");
        key(&mut app, KeyCode::Char('?'));
        let help = screen(&mut app, 80, 24);
        assert!(help.contains("diff of its work") && help.contains("redraw the screen"), "{help}");
        assert!(help.contains("move to another project"), "{help}");
    }

    #[test]
    fn v_shows_the_project_cairn_and_e_edits_it() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        key(&mut app, KeyCode::Char('V'));
        assert!(screen(&mut app, 80, 24).contains("no project yet"));
        assert!(key(&mut app, KeyCode::Char('E')).is_none(), "nothing to edit without a project");
        {
            let store = paths.open(BUSY).unwrap();
            store.conn.execute("UPDATE task SET project = '/p'", []).unwrap();
            let text = "Conventions:\nGotchas:\n- `launch.rs` tests fail on macOS.\nHow-to:\n";
            let doc = varde_core::cairn::Cairn::from_edit(text, &varde_core::cairn::Cairn::project());
            let w = varde_core::memory::Write {
                author: "user",
                run: None,
                reason: None,
                gone: &[],
                default_gone: "",
                capped: &[],
                now: now_ms(),
            };
            varde_core::memory::save_doc(&store.conn, varde_core::memory::Owner::Project("/p"), 0, &doc, &w)
                .unwrap();
        }
        app.refresh().unwrap();
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("project cairn") && s.contains("tests fail on macOS"), "{s}");
        key(&mut app, KeyCode::Char('v'));
        assert!(!screen(&mut app, 80, 24).contains("tests fail on macOS"), "v shows the task again");
        key(&mut app, KeyCode::Char('v'));
        assert!(!app.detail, "the same key closes it");
        assert!(matches!(key(&mut app, KeyCode::Char('E')), Some(Away::EditProject(p)) if p == "/p"));
    }

    #[test]
    fn n_asks_the_project_of_a_new_task_offering_the_one_at_point() {
        let (dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        let dir = dir.path().canonicalize().unwrap();
        let (old, new) = (dir.join("old"), dir.join("new"));
        for d in [&old, &new] {
            std::fs::create_dir(d).unwrap();
        }
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET project = ?1", [old.to_string_lossy()]).unwrap();
        app.refresh().unwrap();
        let project = |title: &str| -> String {
            store.conn.query_row("SELECT project FROM task WHERE title = ?1", [title], |r| r.get(0)).unwrap()
        };

        // A blank title is refused before the project is asked.
        key(&mut app, KeyCode::Char('n'));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal) && app.message == "a task needs a title", "{}", app.message);

        key(&mut app, KeyCode::Char('n'));
        app.paste("Two");
        key(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, 200, 24).contains(&format!("Project for this task: {}", old.display())));
        // Esc goes back to the form, the title kept.
        key(&mut app, KeyCode::Esc);
        assert!(matches!(&app.mode, Mode::New(d) if d.title == "Two"));
        key(&mut app, KeyCode::Enter);
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste(&new.to_string_lossy());
        key(&mut app, KeyCode::Enter);
        assert_eq!(project("Two"), new.to_string_lossy());
        assert!(app.message.starts_with("created "), "{}", app.message);

        // A directory that is not there creates nothing.
        key(&mut app, KeyCode::Char('n'));
        app.paste("Three");
        key(&mut app, KeyCode::Enter);
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste(&dir.join("gone").to_string_lossy());
        key(&mut app, KeyCode::Enter);
        assert!(app.message.contains("no directory"), "{}", app.message);
        assert_eq!(store.conn.query_row("SELECT count(*) FROM task", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }

    #[test]
    fn m_moves_the_task_to_another_project_unless_varde_refuses() {
        let (dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        // Resolved, as varde records a project (macOS's temp dir is a symlink).
        let dir = dir.path().canonicalize().unwrap();
        let (old, new) = (dir.join("old"), dir.join("new"));
        for d in [&old, &new] {
            std::fs::create_dir(d).unwrap();
        }
        let id = app.current_id().unwrap();
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET project = ?1", [old.to_string_lossy()]).unwrap();
        app.refresh().unwrap();
        let project =
            || -> String { store.conn.query_row("SELECT project FROM task", [], |r| r.get(0)).unwrap() };

        key(&mut app, KeyCode::Char('m'));
        assert!(screen(&mut app, 200, 24).contains(&format!("Move to project: {}", old.display())));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste(&new.to_string_lossy());
        key(&mut app, KeyCode::Enter);
        assert_eq!(project(), new.to_string_lossy());
        assert!(app.message.starts_with("moved "), "{}", app.message);

        // A worktree of the task's keeps it where it is, and says why.
        store
            .conn
            .execute(
                "INSERT INTO worktree (path, task, repo, branch, base, state, created)
                 VALUES ('/wt', ?1, ?2, 'b', 'main', 'active', 1)",
                [&id, &new.to_string_lossy().to_string()],
            )
            .unwrap();
        key(&mut app, KeyCode::Char('m'));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste(&old.to_string_lossy());
        key(&mut app, KeyCode::Enter);
        assert_eq!(project(), new.to_string_lossy());
        assert!(app.message.contains("has an active worktree"), "{}", app.message);
        assert!(screen(&mut app, 80, 24).contains("has an active worktree"));
    }
}
