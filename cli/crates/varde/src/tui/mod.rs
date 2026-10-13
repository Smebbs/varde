//! `varde` with no arguments: the board, live, in the terminal. It reads
//! the same pre-grouped rows as `varde list --json`, so it decides
//! nothing about what is shown. RET asks what a first start needs to
//! know, then suspends it and runs the session in this terminal; it
//! comes back when the agent exits. Inside tmux the
//! session gets a window of its own instead, and the board stays live.
//! Nothing in-process writes to the terminal while the board owns it:
//! notices come back as values, and a broken config file, which would
//! be reported on stderr, is shown in its message line.

mod cairn;
mod diff;
mod draft;
mod draw;
mod field;
mod keys;
mod session;
mod term;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;
use varde_core::board::{self, Group, Row};
use varde_core::plan::Want;
use varde_core::store::Store;
use varde_core::task;

use crate::{done, now_ms, process, tmux, Paths};
use diff::Diff;
use draft::{Draft, Edit};
use field::{Field, Words};
use keys::{Act, Keys};
use term::{enter, have_terminal, input, leave, quit_on_sigterm, terminated};

const BUSY: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(1000);

/// The groups whose tasks are still being worked on.
const OPEN_GROUPS: [&str; 4] = ["needs_you", "working", "open", "inbox"];

/// A line of the board: a group's header, or a row of it, by where
/// they are in `App::groups`.
#[derive(Clone, Copy)]
enum Item {
    Header(usize),
    Row(usize, usize),
}

enum Mode {
    Normal,
    /// The keys, over the board.
    Help,
    /// Writing a new task.
    New(Draft),
    /// Asked to confirm throwing the tasks away.
    Drop(Vec<String>),
    /// Asked whether closing or parking stops a working session, or
    /// keeps it running.
    Stop(Close, Vec<String>),
    /// Asked to confirm starting a done or parked task, which reopens it.
    Reopen(Start, String),
    /// Typing a recall query.
    Recall(Field),
    /// Typing the directory of the project to move the task to.
    Move(String, Field),
    /// Typing the directory of the project a new task is in.
    Where(Draft, Field),
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
    /// Run this task's session, prepared as `run`, in this terminal.
    Attach {
        task: String,
        run: String,
        notices: Vec<String>,
    },
    Edit(String),
    /// Edit the cairn of this project.
    EditProject(String),
    Diff(Diff),
    /// Write the new task in `$EDITOR`, and come back to the form.
    Draft(Draft),
}

/// What recall found: j/k pick a hit, RET (or `v`) opens the view on it,
/// `s` starts it, and the keys that act on a task act on it; `.`
/// searches this project or every one.
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
    /// In a tmux popup: going to a session quits the board, closing it.
    popup: bool,
    /// The board as last read, every group in its fixed order.
    groups: Vec<Group>,
    items: Vec<Item>,
    list: ListState,
    /// The selected task, kept across refreshes as rows move between groups.
    selected: Option<String>,
    /// Or the folded group whose header is selected.
    group_at: Option<&'static str>,
    /// The groups folded or opened with `z`; the rest are as varde has them.
    folds: HashMap<&'static str, bool>,
    /// The task each group was folded from, which unfolding it selects.
    folded_at: HashMap<&'static str, String>,
    /// What the store had when the view last read it: it reads it again
    /// only when that changed, or a minute on.
    seen: Option<Seen>,
    /// The first `g` of `gg` was typed.
    pending_g: bool,
    /// The lines the list and the pane under it had when last drawn, for
    /// half a page.
    page: u16,
    pane: u16,
    /// Where the selection goes after its task moves away (closed,
    /// parked, accepted): the tasks nearest it, nearest first, of which
    /// the first still open is taken.
    keep: Vec<String>,
    /// Tasks marked with space; `a`, `d` and `p` act on them all.
    marked: Vec<String>,
    /// The tasks last closed on this board, with d, P or x: `u` reopens
    /// them.
    closed: Vec<String>,
    /// They were closed from the view, where `u` reopens them too.
    closed_in_view: bool,
    /// The selection was moved since the board opened: until it is, the
    /// first Tab goes to the first task that needs you, where it is.
    placed: bool,
    /// How far the help is scrolled, and how far it can be.
    help_scroll: u16,
    help_most: u16,
    /// The question being asked is about the marked tasks: cancelling it
    /// marks them again.
    asked_marks: bool,
    /// The key is for the recall hit just gone to, not the marked tasks.
    on_hit: bool,
    mode: Mode,
    /// The view is open, and has the keys.
    detail: bool,
    /// The view is on its Project tab: the task's project cairn.
    project_detail: bool,
    /// How far the detail pane is scrolled.
    scroll: u16,
    /// The project the board was opened in; `None` outside a repository.
    here: Option<String>,
    /// Whether the board keeps to `here`, when it has tasks there.
    scoped: bool,
    /// Whether `here` has tasks on the board.
    here_has: bool,
    /// What keeping to `here` left out, while it does.
    elsewhere: Option<board::Elsewhere>,
    /// Recall results, shown in the detail pane until dismissed.
    found: Option<Found>,
    message: String,
    /// The directories Tab could complete a directory prompt to, shown
    /// until a key other than Tab.
    menu: Option<Menu>,
    /// Drafts put away with Esc: the new task's, and one for each task
    /// being edited. `n` (or `e` on its task) takes one up again, so
    /// nothing typed is lost.
    kept: Vec<Draft>,
    /// A task just made or moved, to show even when that means every
    /// project.
    reveal: Option<String>,
    /// The cairn the detail pane shows, which takes the keys while open.
    view: cairn::View,
    /// The board's keys, as the config binds them.
    keys: Keys,
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

/// `popup`: the board runs in a tmux popup, which closes once a key
/// takes you to a session. Outside tmux there is no popup to close.
pub fn run(paths: &Paths, popup: bool) -> Result<()> {
    if !have_terminal() {
        anyhow::bail!("the board needs a terminal; varde list prints it");
    }
    let mut app = App::new(paths, tmux::here());
    app.popup = popup && app.tmux.is_some();
    app.here = paths.open(BUSY).ok().and_then(|s| here_project(&s).ok().flatten());
    app.scoped = crate::config::get().board.scope == crate::config::Scope::Project;
    app.keys = Keys::of(&crate::config::get().keys);
    app.refresh()?;
    // Read before the board takes the screen, so its warning is not
    // written over it.
    if let Some(w) = crate::config::warning() {
        app.message = format!("error: {w}");
    }
    app.take_draft();
    if app.message.is_empty() {
        app.message = crate::doctor::unhooked(&crate::start::default_agent()).unwrap_or_default();
    }
    quit_on_sigterm();
    // In tmux, its pane is the board's while it runs: `varde next --board`
    // comes back to it.
    let pane = app.tmux.as_ref().and_then(|t| {
        let (pane, _) = tmux::this_pane()?;
        t.mark_board(&pane, &paths.tag()).ok().map(|_| pane)
    });
    let mut term = enter()?;
    let result = app.main(&mut term);
    leave();
    if let (Some(t), Some(pane)) = (&app.tmux, pane) {
        let _ = t.mark_board(&pane, "");
    }
    // What is being typed, or was put away, waits for the next board.
    app.put_draft();
    if app.upgraded {
        // A newer varde migrated the store under this board: the new
        // binary takes over, in the same terminal, with the same arguments.
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(crate::exe()).args(std::env::args_os().skip(1)).exec();
        anyhow::bail!("varde was upgraded, and starting the new board failed ({e}): run varde again");
    }
    result
}

/// What is wrong with the config's `[keys]`, if anything.
pub fn check_keys(keys: &std::collections::BTreeMap<String, String>) -> std::result::Result<(), String> {
    keys::check(keys)
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
            popup: false,
            groups: Vec::new(),
            items: Vec::new(),
            list: ListState::default(),
            selected: None,
            group_at: None,
            folds: HashMap::new(),
            folded_at: HashMap::new(),
            seen: None,
            pending_g: false,
            page: 0,
            pane: 0,
            keep: Vec::new(),
            marked: Vec::new(),
            closed: Vec::new(),
            closed_in_view: false,
            placed: false,
            help_scroll: 0,
            help_most: 0,
            asked_marks: false,
            on_hit: false,
            mode: Mode::Normal,
            detail: false,
            project_detail: false,
            scroll: 0,
            here: None,
            scoped: true,
            here_has: false,
            elsewhere: None,
            found: None,
            message: String::new(),
            menu: None,
            kept: Vec::new(),
            reveal: None,
            view: cairn::View::default(),
            keys: Keys::default(),
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
                // A drop waits for the tasks' sessions to stop: the board
                // says so before it blocks.
                if let (Mode::Drop(ids), KeyCode::Char('y' | 'Y')) = (&self.mode, key.code) {
                    let ids = ids.clone();
                    let live = |id: &String| {
                        self.paths
                            .open(BUSY)
                            .and_then(|s| done::live_runs(&s, id))
                            .is_ok_and(|r| !r.is_empty())
                    };
                    if ids.iter().any(live) {
                        self.mode = Mode::Normal;
                        self.message = match ids.as_slice() {
                            [id] => format!("stopping {}'s session…", task::short(id)),
                            _ => "stopping their sessions…".into(),
                        };
                        term.draw(|f| self.draw(f))?;
                        self.drop_tasks(&ids);
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
        let seen = Seen::of(&store, self.selected.as_deref())?;
        if self.seen.as_ref() != Some(&seen) {
            self.seen = Some(seen);
            self.view.reload();
        }
        let now = now_ms();
        let mut board = board::build(&store, now, crate::list::local_midnight_ms(), false, &process::alive)?;
        let kicked = crate::sweep::kick(self.paths, &board, now);
        self.watch_tmux(&store, &mut board, now);
        // Kept to the project only now: the sweep and tmux see every task.
        self.here_has = self.here.as_ref().is_some_and(|p| board.has(p));
        // A task just made, moved or picked in another project is shown,
        // not hidden by the board keeping to this one.
        if let Some(task) = self.reveal.take() {
            let row = board.groups.iter().flat_map(|g| &g.rows).find(|r| r.task == task);
            if self.scoped && row.is_some_and(|r| r.project.as_deref() != self.here.as_deref()) {
                self.scoped = false;
                self.message.push_str("; showing every project (. keeps to this one)");
            }
        }
        self.elsewhere = match &self.here {
            Some(p) if self.scoped && self.here_has => Some(board.only(p)),
            _ => None,
        };
        self.groups = board.groups;
        self.layout();
        let mut marked = std::mem::take(&mut self.marked);
        marked.retain(|t| self.row_of(t).is_some());
        self.marked = marked;
        kicked
    }

    /// Whether group `g` shows only its header: as varde has it, unless
    /// `z` said otherwise. An empty group has nothing to fold.
    fn folded(&self, g: &Group) -> bool {
        !g.rows.is_empty() && self.folds.get(g.key).copied().unwrap_or(g.collapsed)
    }

    /// Lay the board out from `groups`, keeping the selection. Every
    /// group is there, in its fixed order, so nothing jumps: an empty one
    /// is its header. Dropped tasks are only read when asked for, so
    /// their group is there only with some.
    fn layout(&mut self) {
        self.items.clear();
        for (gi, g) in self.groups.iter().enumerate() {
            if g.rows.is_empty() && g.key == "dropped" {
                continue;
            }
            self.items.push(Item::Header(gi));
            if !self.folded(g) {
                self.items.extend((0..g.rows.len()).map(|ri| Item::Row(gi, ri)));
            }
        }
        let n = self.items.len();
        let keep = std::mem::take(&mut self.keep);
        let open = |i: usize| OPEN_GROUPS.contains(&self.group(i).key);
        // An open task first; only with none, a closed one.
        let kept = [true, false].into_iter().find_map(|only_open| {
            keep.iter().find_map(|t| {
                (0..n).find(|&i| (open(i) || !only_open) && self.row(i).is_some_and(|r| &r.task == t))
            })
        });
        let index = match kept {
            Some(i) => Some(i),
            None => self
                .group_at
                .and_then(|k| (0..n).find(|&i| self.selectable(i) && self.group(i).key == k))
                .or_else(|| {
                    let t = self.selected.as_ref()?;
                    (0..n).find(|&i| self.row(i).is_some_and(|r| &r.task == t))
                })
                .or_else(|| (0..n).find(|&i| self.selectable(i))),
        };
        let was = self.selected.take();
        self.list.select(index);
        self.selected = index.and_then(|i| self.row(i)).map(|r| r.task.clone());
        self.group_at = index.filter(|&i| self.row(i).is_none()).map(|i| self.group(i).key);
        if self.selected != was {
            self.scroll = 0;
        }
    }

    fn row(&self, i: usize) -> Option<&Row> {
        match self.items.get(i) {
            Some(Item::Row(g, r)) => Some(&self.groups[*g].rows[*r]),
            _ => None,
        }
    }

    /// The group of the line at `i`.
    fn group(&self, i: usize) -> &Group {
        match self.items[i] {
            Item::Header(g) | Item::Row(g, _) => &self.groups[g],
        }
    }

    /// What the selection can be on: a task, or a folded group's header.
    fn selectable(&self, i: usize) -> bool {
        match self.items.get(i) {
            Some(Item::Row(..)) => true,
            Some(Item::Header(g)) => self.folded(&self.groups[*g]),
            None => false,
        }
    }

    fn rows(&self) -> impl Iterator<Item = &Row> {
        self.items.iter().filter_map(|i| match i {
            Item::Row(g, r) => Some(&self.groups[*g].rows[*r]),
            _ => None,
        })
    }

    fn row_of(&self, task: &str) -> Option<&Row> {
        self.rows().find(|r| r.task == task)
    }

    /// Whether closing `task` would stop a session at work: in the middle
    /// of a turn, or with agents and shells its last turn left running;
    /// any of its live sessions, not only the one its row shows.
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
        self.row_of(task).is_some_and(|r| r.busy) || runs().unwrap_or(false)
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
            if self.selectable(i) {
                return self.select(i);
            }
        }
    }

    /// To the first row, or the last.
    fn jump(&mut self, last: bool) {
        let rows = (0..self.items.len()).filter(|&i| self.selectable(i));
        let i = if last { rows.max() } else { rows.min() };
        if let Some(i) = i {
            self.select(i);
        }
    }

    /// `}` to the next group with anything in it; `{` to the top of this
    /// one, or else of the one before.
    fn group_step(&mut self, down: bool) {
        let Some(at) = self.list.selected() else { return };
        // The first line the selection can be on in each group.
        let mut tops: Vec<usize> = Vec::new();
        for i in (0..self.items.len()).filter(|&i| self.selectable(i)) {
            if tops.last().is_none_or(|&t| self.group(t).key != self.group(i).key) {
                tops.push(i);
            }
        }
        let to = if down { tops.into_iter().find(|&t| t > at) } else { tops.into_iter().rfind(|&t| t < at) };
        if let Some(i) = to {
            self.select(i);
        }
    }

    /// C-d and C-u: half the list down or up, or with a pane open, half
    /// the pane.
    fn half(&mut self, down: bool) {
        if self.detail || self.found.is_some() {
            let n = (self.pane / 2).max(1);
            self.scroll = if down { self.scroll.saturating_add(n) } else { self.scroll.saturating_sub(n) };
            return;
        }
        let Some(at) = self.list.selected() else { return };
        let (len, n) = (self.items.len(), (self.page as usize / 2).max(1));
        let to = if down { (at + n).min(len.saturating_sub(1)) } else { at.saturating_sub(n) };
        // The nearest line it can be on, short of there before beyond it.
        let short: Vec<usize> = if down { (at + 1..=to).rev().collect() } else { (to..at).collect() };
        let beyond: Vec<usize> = if down { (to + 1..len).collect() } else { (0..to).rev().collect() };
        if let Some(i) = short.into_iter().chain(beyond).find(|&i| self.selectable(i)) {
            self.select(i);
        }
    }

    /// Tab: to the next task that needs you, round to the first; S-Tab
    /// back. Its group opens if it was folded.
    fn next_needs(&mut self, back: bool) {
        let Some(gi) = self.groups.iter().position(|g| g.key == "needs_you") else { return };
        let n = self.groups[gi].rows.len();
        if n == 0 {
            self.message = match self.elsewhere {
                Some(e) if e.need_you > 0 => {
                    format!("nothing here needs you; {} in other projects: . shows them", e.need_you)
                }
                _ => "nothing needs you".into(),
            };
            return;
        }
        let at = match self.list.selected().and_then(|i| self.items.get(i)) {
            Some(Item::Row(g, r)) if *g == gi => Some(*r),
            _ => None,
        };
        let to = match (at, back) {
            // Where the board opened, the first Tab stays on the first.
            (Some(0), false) if !self.placed => 0,
            (Some(r), false) => (r + 1) % n,
            (Some(r), true) => (r + n - 1) % n,
            (None, false) => 0,
            (None, true) => n - 1,
        };
        self.folds.remove("needs_you");
        self.placed = true;
        (self.selected, self.group_at) = (Some(self.groups[gi].rows[to].task.clone()), None);
        self.layout();
        if n > 1 {
            self.message = format!("needs you: {} of {n}", to + 1);
        }
    }

    /// `z`: fold the group at point, or open the folded one on the task
    /// it was folded from.
    fn fold(&mut self) {
        let Some(at) = self.list.selected() else { return };
        let (gi, open) = match self.items[at] {
            Item::Header(g) => (g, true),
            Item::Row(g, _) => (g, false),
        };
        let g = &self.groups[gi];
        self.folds.insert(g.key, !open);
        if open {
            let from = self.folded_at.remove(g.key).filter(|t| g.rows.iter().any(|r| r.task == *t));
            (self.selected, self.group_at) = (from.or_else(|| g.rows.first().map(|r| r.task.clone())), None);
        } else {
            if let Some(t) = self.selected.clone() {
                self.folded_at.insert(g.key, t);
            }
            (self.selected, self.group_at) = (None, Some(g.key));
        }
        self.layout();
    }

    fn select(&mut self, i: usize) {
        self.placed = true;
        self.selected = self.row(i).map(|r| r.task.clone());
        self.group_at = (self.selected.is_none() && i < self.items.len()).then(|| self.group(i).key);
        self.list.select(Some(i));
        self.scroll = 0;
    }

    /// When `acted` move away, the selection goes to the task still open
    /// nearest it, the one after it on a tie: never to one of them.
    fn keep_place(&mut self, acted: &[String]) {
        let at = self.list.selected().unwrap_or(0);
        let mut near: Vec<(usize, &String)> = (0..self.items.len())
            .filter_map(|i| Some((i, &self.row(i)?.task)))
            .filter(|(_, t)| !acted.contains(t))
            .collect();
        near.sort_by_key(|(i, _)| (i.abs_diff(at), *i < at));
        self.keep = near.into_iter().map(|(_, t)| t.clone()).collect();
    }

    /// J and K in the view: the next or previous task, past the groups'
    /// headers, a folded group opened as Tab opens one.
    fn step_task(&mut self, down: bool) {
        let tasks: Vec<(&'static str, &String)> =
            self.groups.iter().flat_map(|g| g.rows.iter().map(move |r| (g.key, &r.task))).collect();
        let at = match (&self.selected, self.group_at) {
            (Some(t), _) => tasks.iter().position(|(_, x)| *x == t).map(|i| (i, true)),
            // On a folded group: its first task is next.
            (None, Some(k)) => tasks.iter().position(|(g, _)| *g == k).map(|i| (i, false)),
            (None, None) => None,
        };
        let to = match (at, down) {
            (Some((i, on)), true) => i + usize::from(on),
            (Some((i, _)), false) => match i.checked_sub(1) {
                Some(i) => i,
                None => return,
            },
            (None, _) => 0,
        };
        let Some((group, task)) = tasks.get(to).map(|(g, t)| (*g, (*t).clone())) else { return };
        if self.groups.iter().any(|g| g.key == group && self.folded(g)) {
            self.folds.insert(group, false);
        }
        (self.selected, self.group_at, self.placed) = (Some(task), None, true);
        self.layout();
    }

    fn key(&mut self, key: KeyEvent) -> Option<Away> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let menu = self.menu.take();
        // What was said over a form or prompt lasts until the next key.
        if matches!(self.mode, Mode::New(_) | Mode::Where(..) | Mode::Move(..) | Mode::Recall(_)) {
            self.message.clear();
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            // Taller than the screen, it scrolls; any other key closes it.
            Mode::Help if self.help_most > 0 => {
                let (at, half) = (self.help_scroll, (self.page / 2).max(1));
                self.help_scroll = match key.code {
                    KeyCode::Char('j') | KeyCode::Down if !ctrl => at + 1,
                    KeyCode::Char('k') | KeyCode::Up if !ctrl => at.saturating_sub(1),
                    KeyCode::Char('d') if ctrl => at + half,
                    KeyCode::Char('u') if ctrl => at.saturating_sub(half),
                    KeyCode::PageDown => at + half,
                    KeyCode::PageUp => at.saturating_sub(half),
                    _ => return None,
                }
                .min(self.help_most);
                self.mode = Mode::Help;
            }
            Mode::Help => {}
            Mode::New(mut draft) => match draft.key(key) {
                Edit::Typing => self.mode = Mode::New(draft),
                Edit::Editor => return Some(Away::Draft(draft)),
                // A task with only a context, new or edited, is titled from it.
                Edit::Create if draft.is_empty() => {
                    self.message = "a task needs a title or a context".into();
                    self.mode = Mode::New(draft);
                }
                Edit::Create if draft.task.is_some() => self.save(draft),
                Edit::Create => match self.new_task_project() {
                    Some(dir) => self.create(draft, &dir),
                    None => {
                        let dir =
                            self.current().and_then(|r| r.project.clone()).or_else(|| self.here.clone());
                        self.mode = Mode::Where(
                            draft,
                            Field::new(&dir.as_deref().map(draw::tilde).unwrap_or_default()),
                        );
                    }
                },
                Edit::Cancel => self.keep(draft),
            },
            Mode::Where(draft, mut dir) => match key.code {
                KeyCode::Enter => {
                    let typed = dir.text().to_string();
                    self.create(draft, &typed);
                }
                KeyCode::Esc => self.mode = Mode::New(draft),
                KeyCode::Char('g') if ctrl => self.mode = Mode::New(draft),
                KeyCode::Tab | KeyCode::BackTab => {
                    self.menu = tab(&mut dir, menu, key.code == KeyCode::BackTab);
                    self.mode = Mode::Where(draft, dir);
                }
                _ => {
                    dir.key(key, Words::Path);
                    self.mode = Mode::Where(draft, dir);
                }
            },
            Mode::Recall(mut query) => match key.code {
                KeyCode::Enter => self.recall(query.text(), false),
                KeyCode::Esc => {}
                KeyCode::Char('g') if ctrl => {}
                _ => {
                    query.key(key, Words::Prose);
                    self.mode = Mode::Recall(query);
                }
            },
            Mode::Move(task, mut dir) => match key.code {
                KeyCode::Enter => self.move_task(&task, dir.text()),
                KeyCode::Esc => {}
                KeyCode::Char('g') if ctrl => {}
                KeyCode::Tab | KeyCode::BackTab => {
                    self.menu = tab(&mut dir, menu, key.code == KeyCode::BackTab);
                    self.mode = Mode::Move(task, dir);
                }
                _ => {
                    dir.key(key, Words::Path);
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
                    KeyCode::Enter => {
                        return self.answer(asked, start, None, Some(PathBuf::from(&choices[i])))
                    }
                    _ => self.message = "cancelled".into(),
                }
            }
            Mode::Worktree(start, asked, project) => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    return self.answer(asked, start, Some(true), project)
                }
                KeyCode::Char('n' | 'N') => return self.answer(asked, start, Some(false), project),
                _ => self.message = "cancelled".into(),
            },
            Mode::Drop(ids) => match key.code {
                KeyCode::Char('y' | 'Y') => self.drop_tasks(&ids),
                _ => self.unasked(ids),
            },
            Mode::Reopen(start, task) => match key.code {
                KeyCode::Char('y' | 'Y') if self.current_id().as_deref() == Some(task.as_str()) => {
                    return self.begin(start)
                }
                // Asked by `s` on a recall hit, which may be too old for the board.
                KeyCode::Char('y' | 'Y') if self.picked().as_deref() == Some(task.as_str()) => {
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
                KeyCode::Char('y' | 'Y') => self.close_now(how, ids, true),
                KeyCode::Char('n' | 'N') => self.close_now(how, ids, false),
                _ => self.unasked(ids),
            },
            Mode::Normal => {
                self.message.clear();
                self.on_hit = false;
                let pending_g = std::mem::take(&mut self.pending_g);
                if let Some(away) = self.recall_key(key) {
                    return away;
                }
                // The cairn view takes the keys that work on its lines; the
                // rest do what they do on the board.
                if self.detail && self.found.is_none() && !self.on_hit {
                    if let Some(away) = self.view_key(key, pending_g) {
                        return away;
                    }
                }
                let act = match key.code {
                    KeyCode::Char('c') if ctrl => {
                        self.quit = true;
                        return None;
                    }
                    KeyCode::Char('l') if ctrl => Some(Act::Refresh),
                    KeyCode::Char('d') if ctrl => Some(Act::HalfDown),
                    KeyCode::Char('u') if ctrl => Some(Act::HalfUp),
                    _ if ctrl => None,
                    KeyCode::Esc => {
                        self.escape();
                        return None;
                    }
                    code => self.keys.act(code),
                };
                // `g` goes to the top as vi's does: twice.
                if act == Some(Act::First) && key.code == KeyCode::Char('g') && !pending_g {
                    self.pending_g = true;
                    return None;
                }
                let start = |want, go| Some(Start { want, go });
                let start = match act {
                    // On a folded group, RET opens it.
                    Some(Act::Start) if self.group_at.is_some() => {
                        self.fold();
                        None
                    }
                    Some(Act::Start) => start(Want::Continue, true),
                    Some(Act::Background) => start(Want::Continue, false),
                    Some(Act::Fresh) => start(Want::Fresh, true),
                    Some(Act::Fork) => start(Want::Fork, true),
                    Some(Act::Edit) => return self.edit(),
                    Some(Act::Editor) => return self.editor(),
                    Some(Act::Diff) => return self.current_id().and_then(|id| self.changes(&id)),
                    Some(act) => {
                        self.normal(act);
                        None
                    }
                    None => None,
                };
                return match (start, self.current()) {
                    // Starting a closed task reopens it: one key too easily
                    // pressed for that.
                    (Some(start), Some(r)) if matches!(r.state.as_str(), "done" | "parked" | "dropped") => {
                        self.mode = Mode::Reopen(start, r.task.clone());
                        None
                    }
                    (Some(start), _) => self.begin(start),
                    (None, _) => None,
                };
            }
        }
        None
    }

    /// A key for the recall pane, while it is open: `Some` with what it
    /// asks for when the pane took it, `None` for the board's. j/k pick a
    /// hit; RET or `v` open the view on it, on the Project tab for a
    /// project's cairn; `s` is `start_hit`; `.` widens or narrows the
    /// search, as it does the board. The keys that act on a task go to the
    /// hit first, and then act on it: never on the row the pane hides.
    fn recall_key(&mut self, key: KeyEvent) -> Option<Option<Away>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let act = if ctrl { None } else { self.keys.act(key.code) };
        let found = self.found.as_mut()?;
        let n = found.hits.len();
        match (key.code, act) {
            (KeyCode::Char('j') | KeyCode::Down, _) if !ctrl && n > 0 => found.at = (found.at + 1).min(n - 1),
            (KeyCode::Char('k') | KeyCode::Up, _) if !ctrl => found.at = found.at.saturating_sub(1),
            (_, Some(Act::Scope)) => {
                let (query, all) = (found.query.clone(), !found.all);
                self.recall(&query, all);
            }
            (_, Some(Act::Start | Act::View)) => self.view_hit(),
            (_, Some(Act::Background)) => return Some(self.start_hit()),
            (
                _,
                Some(
                    Act::Fresh
                    | Act::Fork
                    | Act::Edit
                    | Act::Editor
                    | Act::Diff
                    | Act::Done
                    | Act::Park
                    | Act::Drop
                    | Act::Accept
                    | Act::Move
                    | Act::Mark,
                ),
            ) => {
                if !self.go_to_hit() {
                    return Some(None);
                }
                self.on_hit = true;
                return None;
            }
            _ => return None,
        }
        Some(None)
    }

    /// The picked hit's task selected on the board, the recall pane
    /// closed, for a key to act on it; whether it is. A project's cairn
    /// has no task to act on.
    fn go_to_hit(&mut self) -> bool {
        let Some(hit) = self.found.as_ref().and_then(|f| f.hits.get(f.at)) else { return false };
        if hit.task.is_empty() {
            self.message = "a project cairn has no task to act on: RET views it".into();
            return false;
        }
        let (task, short) = (hit.task.clone(), hit.short.clone());
        self.go_to(&task, &short);
        self.found.is_none() && self.current_id().as_deref() == Some(task.as_str())
    }

    /// RET on a hit: the view, on its task, or on the Project tab of a
    /// task of its project for a project's cairn.
    fn view_hit(&mut self) {
        let Some(hit) = self.found.as_ref().and_then(|f| f.hits.get(f.at)) else { return };
        let (task, short, project) = (hit.task.clone(), hit.short.clone(), hit.project.clone());
        if task.is_empty() {
            let Some(p) = project else { return };
            if !self.go_to_project(&p) {
                return;
            }
        } else {
            self.go_to(&task, &short);
            if self.current_id().as_deref() != Some(task.as_str()) {
                return;
            }
        }
        self.found = None;
        (self.detail, self.project_detail, self.scroll) = (true, task.is_empty(), 0);
        self.view.open();
    }

    /// Select a task of project `p`, whose cairn the view's Project tab
    /// then shows: the one selected if it is of `p`, else the first on the
    /// board, every project shown for it if need be. Whether there is one.
    fn go_to_project(&mut self, p: &str) -> bool {
        let of = |app: &Self| {
            let mine = |r: &&Row| r.project.as_deref() == Some(p);
            app.current().filter(mine).or_else(|| app.rows().find(mine)).map(|r| r.task.clone())
        };
        let mut task = of(self);
        if task.is_none() && self.scoped {
            self.scoped = false;
            if let Err(e) = self.refresh() {
                self.report(Err(e));
            }
            task = of(self);
            if task.is_some() {
                self.message = "showing every project (. keeps to this one)".into();
            } else {
                (self.scoped, self.stale) = (true, true);
            }
        }
        let Some(task) = task else {
            let name = crate::list::project_name(Some(p));
            self.message = format!("no task of {name} on the board: varde show --project {}", draw::tilde(p));
            return false;
        };
        (self.selected, self.group_at) = (Some(task), None);
        self.layout();
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
        if task.is_empty() {
            self.message = "a project cairn has no session to start: RET views it".into();
            return None;
        }
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
        self.reveal = Some(task.to_string());
        self.keep.clear();
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
        let Some(state) = state else { return };
        let (found, scoped, folds) = (self.found.take(), self.scoped, self.folds.clone());
        if state == "parked" {
            self.folds.insert("parked", false);
        }
        self.selected = Some(task.to_string());
        self.reveal = Some(task.to_string());
        self.keep.clear();
        if let Err(e) = self.refresh() {
            self.report(Err(e));
        }
        if self.current_id().as_deref() != Some(task) {
            // An old done or dropped task: the pane stays, as it was.
            (self.found, self.scoped, self.folds) = (found, scoped, folds);
            self.message = format!("{short} is {state} and not on the board: varde show {short}");
            self.stale = true;
        }
    }

    /// Esc on the board: unmark, else close the pane; it quits only in a
    /// popup, which Esc closes as it does any, or when `[keys]` binds it
    /// to. The view has its own, which unmarks first too.
    fn escape(&mut self) {
        if !self.marked.is_empty() {
            self.marked.clear();
        } else if self.found.is_some() {
            self.found = None;
        } else if self.detail {
            self.detail = false;
        } else if self.popup || self.keys.act(KeyCode::Esc) == Some(Act::Quit) {
            self.quit = true;
        } else {
            self.message = format!("{} quits", self.keys.label(Act::Quit));
        }
    }

    /// A key on the board that stays on the board.
    fn normal(&mut self, act: Act) {
        match act {
            // As in the help: a pane open is what `q` closes.
            Act::Quit if self.found.is_some() => self.found = None,
            Act::Quit if self.detail => self.detail = false,
            Act::Quit => self.quit = true,
            Act::Help => (self.mode, self.help_scroll) = (Mode::Help, 0),
            Act::Recall => self.mode = Mode::Recall(Field::default()),
            Act::Down => self.step(true),
            Act::Up => self.step(false),
            Act::First => self.jump(false),
            Act::Last => self.jump(true),
            Act::NextGroup => self.group_step(true),
            Act::PrevGroup => self.group_step(false),
            Act::HalfDown => self.half(true),
            Act::HalfUp => self.half(false),
            Act::NextNeeds => self.next_needs(false),
            Act::PrevNeeds => self.next_needs(true),
            Act::Fold => self.fold(),
            Act::View => self.show_detail(false),
            Act::Cairn => self.show_detail(true),
            Act::New => {
                let draft = match self.kept.iter().position(|d| d.task.is_none()) {
                    Some(i) => {
                        self.message = "your new task, as you left it".into();
                        Draft { restored: true, ..self.kept.remove(i) }
                    }
                    None => Draft::default(),
                };
                self.mode = Mode::New(draft);
            }
            Act::Move => {
                if let Some(r) = self.current() {
                    let dir = r.project.as_deref().map(draw::tilde).unwrap_or_default();
                    self.mode = Mode::Move(r.task.clone(), Field::new(&dir));
                }
            }
            Act::Mark => self.mark(),
            Act::Accept => self.accept(),
            Act::Reopen => self.reopen_last(),
            Act::Done => self.close(Close::Done),
            Act::Park => self.close(Close::Park),
            Act::Drop => {
                let ids = self.targets();
                if !ids.is_empty() {
                    self.mode = Mode::Drop(ids);
                }
            }
            Act::Refresh => {
                self.redraw = true;
                self.stale = true;
            }
            Act::Scope => self.toggle_scope(),
            Act::Start | Act::Background | Act::Fresh | Act::Fork | Act::Edit | Act::Editor | Act::Diff => {}
        }
    }

    /// The footer's keys, for the selected row, in `width` columns.
    fn footer(&self, width: usize) -> Vec<(String, &'static str)> {
        let row = self.current();
        let needs = self.groups.iter().any(|g| g.key == "needs_you" && !g.rows.is_empty());
        let state = row.map(|r| r.state.as_str()).or(self.group_at.map(|_| "folded"));
        let reopen = !self.closed.is_empty();
        self.keys.footer(state, row.is_some_and(|r| r.inbox), needs, reopen, self.marked.len(), width)
    }

    /// Keep to the project the board was opened in, or show every one.
    fn toggle_scope(&mut self) {
        let Some(here) = &self.here else {
            self.message = "the board was opened outside a project, so it shows every one".into();
            return;
        };
        let name = crate::list::project_name(Some(here)).to_string();
        if !self.here_has {
            self.message = format!("no tasks in {name} yet, so the board shows every project");
            return;
        }
        self.scoped = !self.scoped;
        self.stale = true;
        self.message = if self.scoped { format!("only {name}") } else { "every project".into() };
    }

    /// Open the view on its Task tab, or its Project tab; `v` again closes
    /// it, on either.
    fn show_detail(&mut self, project: bool) {
        self.detail = !(self.detail && (!project || self.project_detail));
        self.project_detail = project;
        self.scroll = 0;
        self.view.open();
    }

    /// Pasted text goes into the prompt being typed, and nowhere else.
    fn paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::New(draft) => draft.paste(text),
            Mode::Recall(query) => query.insert(&text.split_whitespace().collect::<Vec<_>>().join(" ")),
            Mode::Move(_, dir) | Mode::Where(_, dir) => dir.insert(text.trim()),
            Mode::Normal if self.detail && self.view.paste(text) => {}
            Mode::Normal => self.message = "pasted text goes into a prompt: n new task, / recall".into(),
            _ => {}
        }
    }

    /// Leave the screen to `away`, and take it back after.
    fn away(&mut self, away: Away, term: &mut DefaultTerminal) -> Result<()> {
        leave();
        let result = match away {
            Away::Attach { task, run, notices } => self.attach_here(&task, &run, notices),
            Away::Edit(id) => {
                crate::memory_cmd::edit(self.paths, &id, false, false).map(|s| saved("cairn", s))
            }
            Away::EditProject(project) => {
                crate::memory_cmd::edit_project(self.paths, Path::new(&project), false, false)
                    .map(|s| saved("project cairn", s))
            }
            Away::Diff(diff) => self.diff(&diff),
            Away::Draft(mut draft) => {
                let result = crate::memory_cmd::edit_in_editor("task", &draft.text()).map(|text| {
                    draft.set_text(&text);
                    String::new()
                });
                self.mode = Mode::New(draft);
                result
            }
        };
        *term = enter()?;
        self.redraw = true;
        self.report(result);
        Ok(())
    }

    /// Say how an action went. Any action may have changed the store.
    fn report(&mut self, result: Result<String>) {
        self.stale = true;
        self.view.reload();
        self.message = match result {
            Ok(m) => m,
            Err(e) => format!("error: {e:#}"),
        };
    }

    /// `E`: the whole cairn in `$EDITOR`, the project's on the view's
    /// Project tab, else the task's with its context above it.
    fn editor(&mut self) -> Option<Away> {
        let r = self.current()?;
        if !(self.detail && self.project_detail) {
            return Some(Away::Edit(r.task.clone()));
        }
        match r.project.clone() {
            Some(p) => Some(Away::EditProject(p)),
            None => {
                self.message = "this task has no project yet: its first start picks one".into();
                None
            }
        }
    }

    /// `e`: the task's title and context, in the new-task form. A task
    /// linked elsewhere, which then owns its text, goes to `$EDITOR`.
    fn edit(&mut self) -> Option<Away> {
        let row = self.current()?;
        let id = row.task.clone();
        if row.external.is_some() {
            return Some(Away::Edit(id));
        }
        if let Some(i) = self.kept.iter().position(|d| d.task.as_deref() == Some(id.as_str())) {
            self.mode = Mode::New(Draft { restored: true, ..self.kept.remove(i) });
            self.message = "your changes, as you left them".into();
            return None;
        }
        let result = (|| -> Result<Draft> {
            let store = self.paths.open(BUSY)?;
            let (title, body): (String, Option<String>) =
                store.conn.query_row("SELECT title, body FROM task WHERE id = ?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            Ok(Draft::of(&id, &title, &body.unwrap_or_default()))
        })();
        match result {
            Ok(draft) => {
                self.mode = Mode::New(draft);
                None
            }
            Err(e) => {
                self.report(Err(e));
                None
            }
        }
    }

    /// The file the drafts wait in while no board runs: every board's,
    /// merged.
    fn drafts_file(&self) -> PathBuf {
        self.paths.home.join("drafts.json")
    }

    /// Before the board ends: the new task and the edits being written,
    /// or put away, and the cairn lines, added to what other boards of
    /// the store left. Theirs give way to this board's where both have a
    /// draft of the same thing.
    fn put_draft(&mut self) {
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::New(d) | Mode::Where(d, _) => self.keep(d),
            _ => {}
        }
        let mut ours: Vec<serde_json::Value> =
            self.kept.iter().filter(|d| !d.is_empty()).map(Draft::json).collect();
        ours.extend(self.view.drafts());
        if ours.is_empty() {
            return;
        }
        let file = self.drafts_file();
        let slot = |d: &serde_json::Value| (d["task"].clone(), d["project"].clone(), d["target"].clone());
        let theirs = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok());
        if let Some(serde_json::Value::Array(theirs)) = theirs {
            let mine: Vec<_> = ours.iter().map(slot).collect();
            ours.extend(theirs.into_iter().filter(|d| !mine.contains(&slot(d))));
        }
        let _ = std::fs::write(file, serde_json::Value::Array(ours).to_string());
    }

    /// When the board starts: the drafts the boards before it left, if
    /// any, and those of a board that started again as a newer varde.
    fn take_draft(&mut self) {
        let home = &self.paths.home;
        let mut all = Vec::new();
        for name in ["drafts.json", "cairn-drafts.json"] {
            if let Ok(text) = std::fs::read_to_string(home.join(name)) {
                let _ = std::fs::remove_file(home.join(name));
                if let Ok(serde_json::Value::Array(d)) = serde_json::from_str(&text) {
                    all.extend(d);
                }
            }
        }
        // As varde 1.1 left a new task's.
        if let Ok(text) = std::fs::read_to_string(home.join("draft.txt")) {
            let _ = std::fs::remove_file(home.join("draft.txt"));
            let mut d = Draft::default();
            d.set_text(&text);
            all.push(d.json());
        }
        let mut said = Vec::new();
        for d in &all {
            if let Some(draft) = Draft::of_json(d) {
                if draft.task.is_none() && !self.kept.iter().any(|k| k.task.is_none()) {
                    said.push(format!("your new task is kept: {} brings it back", self.keys.label(Act::New)));
                } else if draft.task.is_some() {
                    said.push(format!(
                        "changes to {} are kept: e on it brings them back",
                        self.named_task(&draft)
                    ));
                }
                self.kept.push(draft);
            }
        }
        // Each named by its task's title, or its project, and where in
        // the cairn it goes: `"Fix login" (Findings, Status)`.
        let took = self.view.take_drafts(&all);
        let mut whose: Vec<(String, Vec<String>)> = Vec::new();
        for (key, project, what) in took.iter().cloned() {
            let name = match project {
                true => format!("{}'s project cairn", crate::list::project_name(Some(&key))),
                false => self.named(&key),
            };
            match whose.iter_mut().find(|(n, _)| *n == name) {
                Some((_, w)) => w.push(what),
                None => whose.push((name, vec![what])),
            }
        }
        let whose: Vec<String> = whose.into_iter().map(|(n, w)| format!("{n} ({})", w.join(", "))).collect();
        match took.len() {
            0 => {}
            1 => said.push(format!("a cairn line you typed for {} is kept: v shows it", whose[0])),
            n => said
                .push(format!("{n} cairn lines you typed are kept, for {}: v shows them", whose.join(", "))),
        }
        if !said.is_empty() {
            self.message = said.join("; ");
        }
    }

    /// The task a draft edits, as a message names it.
    fn named_task(&self, draft: &Draft) -> String {
        draft.task.as_deref().map_or_else(|| "the new task".into(), |id| self.named(id))
    }

    /// Put a draft away, to be taken up again: nothing typed is lost to
    /// one key. One with nothing in it, or nothing changed, is dropped:
    /// to throw a draft away, clear it (C-u) and Esc.
    fn keep(&mut self, draft: Draft) {
        let unchanged = draft.is_empty()
            || draft.task.as_ref().is_some_and(|id| {
                self.paths.open(BUSY).ok().is_some_and(|s| {
                    s.conn
                        .query_row("SELECT title, coalesce(body, '') FROM task WHERE id = ?1", [id], |r| {
                            Ok(r.get::<_, String>(0)? == draft.title.text()
                                && r.get::<_, String>(1)? == draft.body.text())
                        })
                        .unwrap_or(false)
                })
            });
        if unchanged {
            if draft.restored {
                self.message = "draft dropped".into();
            }
            return;
        }
        self.message = match &draft.task {
            None => format!("new task kept: {} brings it back", self.keys.label(Act::New)),
            Some(_) => format!(
                "changes to {} kept: {} on it brings them back",
                self.named_task(&draft),
                self.keys.label(Act::Edit)
            ),
        };
        self.kept.retain(|k| k.task != draft.task);
        self.kept.push(draft);
    }

    /// Save the title and context of the task `draft` edits. A refusal
    /// leaves the form open, as it was.
    fn save(&mut self, draft: Draft) {
        let Some(id) = draft.task.clone() else { return };
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            task::set(&store, &id, Some(draft.title.text().trim()), Some(draft.body.text()))?;
            crate::naming::start(&store, &id);
            Ok(format!("saved {}", task::short(&id)))
        })();
        if result.is_err() {
            self.mode = Mode::New(draft);
        }
        self.report(result);
    }

    /// Where a new task goes without asking: the project the board keeps
    /// to. Showing every project, or opened outside one, it asks.
    fn new_task_project(&self) -> Option<String> {
        self.here.clone().filter(|_| self.scoped)
    }

    /// Create the task in the project `dir` is in. A failure leaves the
    /// form open, as it was.
    fn create(&mut self, draft: Draft, dir: &str) {
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            let project = crate::start::project_of(&store, Path::new(&home_in(dir)))?.display().to_string();
            let body = Some(draft.body.text().trim()).filter(|b| !b.is_empty());
            let id = task::create(&store, draft.title.text().trim(), body, Some(&project), None, now_ms())?;
            crate::naming::start(&store, &id);
            self.selected = Some(id.clone());
            self.reveal = Some(id.clone());
            let name = crate::list::project_name(Some(&project));
            Ok(format!("created {} in {name}; RET starts it", task::short(&id)))
        })();
        if result.is_err() {
            self.mode = Mode::Where(draft, Field::new(dir));
        }
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
        self.reveal = Some(task.to_string());
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

    /// The marked tasks, or else the one at point. Taking them clears the
    /// marks. In the view, the task it shows, and from the recall pane the
    /// hit: marks are the board's.
    fn targets(&mut self) -> Vec<String> {
        self.asked_marks = !self.marked.is_empty() && !self.detail && !self.on_hit;
        if self.asked_marks {
            std::mem::take(&mut self.marked)
        } else {
            self.current_id().into_iter().collect()
        }
    }

    /// A question about the marked tasks was not answered: they stay marked.
    fn unasked(&mut self, ids: Vec<String>) {
        if std::mem::take(&mut self.asked_marks) {
            self.marked = ids;
        }
        self.message = "cancelled".into();
    }

    /// Do `act` to each of `ids`, reporting its own message for one task
    /// and a count for several: `2 tasks done`.
    fn each(&mut self, ids: &[String], verb: &str, act: impl Fn(&Store, &str) -> Result<String>) {
        if ids.is_empty() {
            return;
        }
        self.asked_marks = false;
        self.keep_place(ids);
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            let mut last = String::new();
            for id in ids {
                last = act(&store, id)?;
            }
            Ok(if ids.len() == 1 { last } else { format!("{} tasks {verb}", ids.len()) })
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
        self.each(&inbox, "accepted", |s, id| {
            task::accept(s, id).map(|_| format!("accepted {}", task::short(id)))
        });
        // The selection follows the accepted task into Open.
        self.keep = inbox;
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
                .each(&ids, "done", |s, id| done::close(s, id, closing(!stop)).map(|r| done::describe(&r))),
            Close::Park => {
                self.each(&ids, "parked", |s, id| done::shelve(s, id, !stop).map(|r| done::describe_park(&r)))
            }
        }
        self.closed_now(ids);
    }

    fn drop_tasks(&mut self, ids: &[String]) {
        self.each(ids, "dropped", |s, id| done::reject(s, id).map(|r| done::describe_drop(&r)));
        self.closed_now(ids.to_vec());
    }

    /// `ids` were closed: `u` reopens them, as it says. In the view, the
    /// close is the last thing done, so `u` undoes it before a line.
    fn closed_now(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        if !self.message.starts_with("error:") {
            self.message.push_str(&format!("; {} reopens", self.keys.label(Act::Reopen)));
        }
        if self.detail {
            self.view.forget_undo();
        }
        self.closed_in_view = self.detail;
        self.closed = ids;
    }

    /// `u` on the board: reopen the tasks last closed on it, the
    /// selection on them.
    fn reopen_last(&mut self) {
        let ids = std::mem::take(&mut self.closed);
        if ids.is_empty() {
            self.message = "nothing closed here to reopen".into();
            return;
        }
        let result = (|| {
            let store = self.paths.open(BUSY)?;
            for id in &ids {
                crate::start::reopen(&store.conn, id)?;
            }
            Ok(match ids.as_slice() {
                [id] => format!("reopened {}", self.named(id)),
                _ => format!("{} tasks reopened", ids.len()),
            })
        })();
        self.report(result);
        // Shown, in whichever project, and selected.
        self.reveal = ids.first().cloned();
        self.keep = ids;
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

/// The newest event, run, note and hand-off, the latest change to a
/// cairn or a task's state, the selected task's own words and project,
/// and the minute: what the view shows changes only with these, its ages
/// with the minute. Reading them costs far less than reading the view.
#[derive(Clone, PartialEq)]
struct Seen([i64; 7], Option<String>, i64);

impl Seen {
    fn of(store: &Store, task: Option<&str>) -> Result<Seen> {
        let mut seen = [0; 7];
        let own = store.conn.query_row(
            "SELECT (SELECT max(rowid) FROM event), (SELECT max(rowid) FROM run), (SELECT max(rowid) FROM note),
                    (SELECT max(rowid) FROM shown), (SELECT max(updated) FROM cairn),
                    (SELECT max(updated) FROM project_cairn), (SELECT max(changed) FROM task),
                    (SELECT title || char(0) || coalesce(body, '') || char(0) || coalesce(project, '')
                     FROM task WHERE id = ?1)",
            [task],
            |r| {
                for (i, s) in seen.iter_mut().enumerate() {
                    *s = r.get::<_, Option<i64>>(i)?.unwrap_or(0);
                }
                r.get(7)
            },
        )?;
        Ok(Seen(seen, own, now_ms() / 60_000))
    }
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

/// The directories in `parent` Tab could complete a directory prompt to,
/// and the one it last put in the line.
struct Menu {
    parent: String,
    names: Vec<String>,
    at: Option<usize>,
}

/// Tab, or Shift-Tab when `back`, in a directory prompt: the next (or
/// previous) directory of the `menu` the last Tab listed put in the
/// line, or else the line completed.
fn tab(line: &mut Field, menu: Option<Menu>, back: bool) -> Option<Menu> {
    let Some(mut menu) = menu else {
        let mut text = line.text().to_string();
        let menu = complete_dir(&mut text);
        line.set(&text);
        return menu;
    };
    let n = menu.names.len();
    let at = match (menu.at, back) {
        (None, false) => 0,
        (None, true) => n - 1,
        (Some(i), false) => (i + 1) % n,
        (Some(i), true) => (i + n - 1) % n,
    };
    line.set(&format!("{}{}/", menu.parent, menu.names[at]));
    menu.at = Some(at);
    Some(menu)
}

/// `line` completed as a shell completes a directory, as far as the
/// directories it could name agree; those directories when that is no
/// further. Hidden ones only once a `.` is typed.
fn complete_dir(line: &mut String) -> Option<Menu> {
    if line.trim() == "~" {
        line.push('/');
    }
    let (parent, stem) = match line.rfind('/') {
        Some(i) => (line[..=i].to_string(), line[i + 1..].to_string()),
        None => (String::new(), line.clone()),
    };
    let dir = if parent.is_empty() { ".".to_string() } else { home_in(&parent) };
    let Ok(entries) = std::fs::read_dir(dir) else { return None };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(&stem) && (stem.starts_with('.') || !n.starts_with('.')))
        .collect();
    names.sort();
    let first = names.first()?;
    let common = names.iter().fold(first.as_str(), |a, b| {
        let end = a
            .char_indices()
            .zip(b.chars())
            .find(|((_, x), y)| x != y)
            .map_or(a.len().min(b.len()), |((i, _), _)| i);
        &a[..end]
    });
    if names.len() == 1 {
        *line = format!("{parent}{common}/");
    } else if common.len() > stem.len() {
        *line = format!("{parent}{common}");
    } else {
        return Some(Menu { parent, names, at: None });
    }
    None
}

/// Closing from the board: never merging or removing.
fn closing(keep_running: bool) -> done::Options {
    done::Options { remove: false, force: false, discard: false, keep_running }
}

#[cfg(test)]
mod tests {
    use super::draw::render;
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
    fn the_board_keeps_to_its_project_until_dot_shows_every_one() {
        let (_dir, paths) = temp_paths();
        let store = paths.open(BUSY).unwrap();
        task::create(&store, "Here", None, Some("/src/app"), None, 1).unwrap();
        task::create(&store, "There", None, Some("/src/lib"), None, 2).unwrap();
        let mut app = App::new(&paths, None);
        app.here = Some("/src/app".into());
        app.refresh().unwrap();
        let s = screen(&mut app, 80, 12);
        assert!(s.contains("varde · app · 1 task in other projects  . all") && !s.contains("There"), "{s}");
        key(&mut app, KeyCode::Char('.'));
        assert_eq!(app.message, "every project");
        let s = screen(&mut app, 80, 12);
        assert!(s.contains("every project  . only app") && s.contains("There"), "{s}");
        key(&mut app, KeyCode::Char('.'));
        assert!(!screen(&mut app, 80, 12).contains("There"));

        // A project with no tasks on the board keeps to nothing.
        app.here = Some("/src/new".into());
        app.refresh().unwrap();
        assert!(screen(&mut app, 80, 12).contains("every project (none in new yet)"));
        key(&mut app, KeyCode::Char('.'));
        assert_eq!(app.message, "no tasks in new yet, so the board shows every project");
        app.here = None;
        key(&mut app, KeyCode::Char('.'));
        assert!(app.message.contains("outside a project") && screen(&mut app, 80, 12).contains("There"));
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
        assert!(screen(&mut app, 80, 24).contains("fresh start / fork"));
        key(&mut app, KeyCode::Char('q'));
        assert!(!app.quit, "a key closes the help, and only that");
        key(&mut app, KeyCode::Char('v'));
        key(&mut app, KeyCode::Char('q'));
        assert!(!app.quit && !app.detail, "q closes the detail pane, and only that");
        key(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn esc_bound_to_quit_unmarks_and_closes_the_view_first() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        app.keys = Keys::of(&[("quit".to_string(), "esc".to_string())].into());
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Esc);
        assert!(app.marked.is_empty() && !app.quit, "unmarked first");
        key(&mut app, KeyCode::Char('v'));
        key(&mut app, KeyCode::Esc);
        assert!(!app.detail && !app.quit, "the view closed first");
        key(&mut app, KeyCode::Esc);
        assert!(app.quit);
    }

    #[test]
    fn accept_is_for_the_inbox_and_the_selection_follows_the_task() {
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
        assert_eq!(app.message, format!("accepted {}", task::short(&first)));
        let now = app.current().unwrap();
        assert!(now.task == first && !now.inbox, "on the accepted task, in Open");
    }

    /// Every task in a project of its own, not a repository: a start
    /// asks nothing.
    fn in_a_project(paths: &Paths) {
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET project = ?1", [paths.home.to_string_lossy()]).unwrap();
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
        assert!(s.contains("Park \"Busy\"? It is working.  y stop · n leave running · Esc cancel"), "{s}");
        key(&mut app, KeyCode::Esc);
        assert_eq!((app.message.as_str(), state(&paths, &id).as_str()), ("cancelled", "open"));
        app.mode = Mode::Stop(Close::Done, vec![id.clone()]);
        assert!(screen(&mut app, 120, 10).contains("Done \"Busy\"? It is working.  y stop"));
        // Worded by what it is doing: a question is not work.
        app.groups.iter_mut().flat_map(|g| g.rows.iter_mut()).for_each(|r| r.status = "blocked");
        assert!(screen(&mut app, 120, 10).contains("\"Busy\"? It is waiting for your answer.  y stop"));
        // A turn over, with what it left running.
        app.groups.iter_mut().flat_map(|g| g.rows.iter_mut()).for_each(|r| {
            (r.status, r.agents, r.shells) = ("waiting", 3, 1);
        });
        assert!(screen(&mut app, 120, 10).contains("\"Busy\"? It has 3 agents and a shell running.  y stop"));
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(state(&paths, &id), "done");
    }

    #[test]
    fn closing_a_task_selects_the_open_task_nearest_it() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Three", "Two", "One"]);
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "Two");
        key(&mut app, KeyCode::Char('d'));
        assert_eq!(app.current().unwrap().title, "Three", "the one after it");
        key(&mut app, KeyCode::Char('P'));
        assert_eq!(app.current().unwrap().title, "One", "the last: the one before it");
        key(&mut app, KeyCode::Char('d'));
        assert_ne!(app.current().map(|r| r.title.as_str()), Some("One"), "never the task just closed");
    }

    #[test]
    fn a_recall_hit_says_how_many_more_lines_of_its_task_matched() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Cache"]);
        let id = app.current_id().unwrap();
        let text = "Status: s\nFindings:\n- The locale matters.\n- The locale is cached.\n- The locale is read twice.\n";
        let cairn = varde_core::cairn::Cairn::from_edit(text, &Default::default());
        let store = paths.open(BUSY).unwrap();
        varde_core::memory::put_cairn(&store.conn, &id, &cairn, 0, "user", None, now_ms()).unwrap();
        app.recall("locale", true);
        assert!(
            screen(&mut app, 100, 24).contains("(+2 more in this task)"),
            "{}",
            screen(&mut app, 100, 24)
        );
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
        key(&mut app, KeyCode::Char('P'));
        assert!(matches!(app.mode, Mode::Stop(Close::Park, _)), "P asks first");
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
        assert!(
            s.contains("Park the 2 marked tasks? Some are working.")
                && s.contains("\ny stop · n leave running · Esc cancel"),
            "{s}"
        );
        app.mode = Mode::Drop(vec![app.current_id().unwrap()]);
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("? Starting it again brings it back.  y/N"), "no worktree, nothing kept:\n{s}");
        app.groups.iter_mut().flat_map(|g| g.rows.iter_mut()).for_each(|r| r.worktree = true);
        let s = screen(&mut app, 80, 24);
        assert!(
            s.contains("refs/varde/discarded/") && s.contains("brings it back.") && s.contains("\ny/N"),
            "{s}"
        );
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
        in_a_project(&paths);
        let id = app.current_id().unwrap();
        done::close(&paths.open(BUSY).unwrap(), &id, closing(true)).unwrap();
        app.refresh().unwrap();
        assert_eq!(app.current_id().as_deref(), Some(id.as_str()));
        assert!(key(&mut app, KeyCode::Enter).is_none());
        assert!(screen(&mut app, 100, 10).contains("is done: reopen and start it?  y/N"));
        key(&mut app, KeyCode::Char('n'));
        assert_eq!((app.message.as_str(), state(&paths, &id).as_str()), ("cancelled", "done"));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(key(&mut app, KeyCode::Char('y')), Some(Away::Attach { .. })), "y starts it");
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
        assert!(s.lines().any(|l| l.contains("Recall: CacheKey region")), "{s}");
        assert!(s.lines().any(|l| l.contains("RET view  s start  d done") && l.contains("Esc close")), "{s}");
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current_id(), before, "j picks a hit, not a board row");
        assert!(key(&mut app, KeyCode::Enter).is_none(), "RET on a hit starts nothing");
        assert!(app.found.is_none() && app.detail && !app.project_detail, "the view, on the hit");
        assert_eq!(app.current_id(), Some(second));
    }

    #[test]
    fn keys_in_the_recall_pane_act_on_the_hit_never_the_row_it_hides() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Cache by locale", "Something else"]);
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET project = '/p'", []).unwrap();
        let id = |title: &str| -> String {
            store.conn.query_row("SELECT id FROM task WHERE title = ?1", [title], |r| r.get(0)).unwrap()
        };
        let (hit, other) = (id("Cache by locale"), id("Something else"));
        let cairn = varde_core::cairn::Cairn::from_edit(
            "Status: s\nFindings:\n- The locale matters.\n",
            &Default::default(),
        );
        varde_core::memory::put_cairn(&store.conn, &hit, &cairn, 0, "user", None, now_ms()).unwrap();
        let gotcha = varde_core::memory::Change::Add(
            varde_core::cairn::Section::Gotchas,
            "The locale cache is per host.",
        );
        let owner = varde_core::memory::Owner::Project("/p");
        varde_core::memory::change(&mut paths.open(BUSY).unwrap(), owner, gotcha, now_ms()).unwrap();
        app.refresh().unwrap();
        // The other task selected and marked, under the pane.
        while app.current_id().as_deref() != Some(other.as_str()) {
            app.step(true);
        }
        key(&mut app, KeyCode::Char(' '));
        app.recall("locale", true);
        let hits = &app.found.as_ref().unwrap().hits;
        let at = hits.iter().position(|h| h.task == hit).unwrap();
        let project = hits.iter().position(|h| h.task.is_empty()).unwrap();

        // A project's cairn has no task: its footer has no `s` or `d`,
        // `d` does nothing, RET views it.
        app.found.as_mut().unwrap().at = project;
        let s = screen(&mut app, 100, 24);
        assert!(s.lines().any(|l| l.contains("RET view  . ") && !l.contains("s start")), "{s}");
        key(&mut app, KeyCode::Char('d'));
        assert!(
            app.message.starts_with("a project cairn has no task") && app.found.is_some(),
            "{}",
            app.message
        );
        key(&mut app, KeyCode::Char('v'));
        assert!(app.found.is_none() && app.detail && app.project_detail, "v is RET");
        assert!(screen(&mut app, 80, 24).contains("╭ p ─"), "the project's name in the border");
        key(&mut app, KeyCode::Char('q'));

        app.recall("locale", true);
        app.found.as_mut().unwrap().at = at;
        key(&mut app, KeyCode::Char('d'));
        assert_eq!((state(&paths, &hit), state(&paths, &other)), ("done".into(), "open".into()));
        assert_eq!(app.marked, [other], "marks are the board's");
    }

    #[test]
    fn esc_quits_a_popup_once_nothing_is_open() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        app.popup = true;
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char('v'));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert!(app.marked.is_empty() && !app.detail && !app.quit, "unmarked and closed first");
        key(&mut app, KeyCode::Esc);
        assert!(app.quit);
    }

    #[test]
    fn a_recall_hit_in_another_project_is_shown_on_the_board() {
        let (_dir, paths) = temp_paths();
        let store = paths.open(BUSY).unwrap();
        task::create(&store, "Here", None, Some("/src/app"), None, 1).unwrap();
        let there = task::create(&store, "Locale over there", None, Some("/src/lib"), None, 2).unwrap();
        let cairn = varde_core::cairn::Cairn::from_edit(
            "Status: s\nFindings:\n- The locale matters.\n",
            &Default::default(),
        );
        varde_core::memory::put_cairn(&store.conn, &there, &cairn, 0, "user", None, now_ms()).unwrap();
        let mut app = App::new(&paths, None);
        app.here = Some("/src/app".into());
        app.refresh().unwrap();
        assert!(app.row_of(&there).is_none(), "kept to its project");
        app.recall("locale", true);
        assert!(key(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.current_id().as_deref(), Some(there.as_str()), "{}", app.message);
        assert!(app.message.contains("every project"), "{}", app.message);
    }

    #[test]
    fn a_project_cairn_hit_opens_on_a_task_of_its_own_project() {
        let (_dir, paths) = temp_paths();
        let store = paths.open(BUSY).unwrap();
        let here = task::create(&store, "Here", None, Some("/src/app"), None, 2).unwrap();
        let there = task::create(&store, "Over there", None, Some("/src/lib"), None, 1).unwrap();
        let gotcha = varde_core::memory::Change::Add(
            varde_core::cairn::Section::Gotchas,
            "The locale cache is per host.",
        );
        let owner = varde_core::memory::Owner::Project("/src/lib");
        varde_core::memory::change(&mut paths.open(BUSY).unwrap(), owner, gotcha, now_ms()).unwrap();
        let mut app = App::new(&paths, None);
        app.refresh().unwrap();
        assert_eq!(app.current_id(), Some(here));
        app.recall("locale", true);
        assert!(app.found.as_ref().unwrap().hits[0].task.is_empty(), "the project's cairn");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.current_id(), Some(there), "{}", app.message);
        assert!(app.detail && app.project_detail);
        // A project with no task on the board says so.
        store.conn.execute("UPDATE task SET project = '/src/app'", []).unwrap();
        app.refresh().unwrap();
        app.recall("locale", true);
        key(&mut app, KeyCode::Enter);
        assert!(app.message.starts_with("no task of lib on the board"), "{}", app.message);
    }

    #[test]
    fn s_on_a_recall_hit_starts_its_task_even_one_gone_from_the_board() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Cache by locale", "Something else"]);
        in_a_project(&paths);
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
        assert!(screen(&mut app, 100, 10).contains("reopen and start it?  y/N"));
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(state(&paths, &old), "done");
        assert!(key(&mut app, KeyCode::Char('s')).is_none());
        assert!(matches!(key(&mut app, KeyCode::Char('y')), Some(Away::Attach { .. })), "y starts it");
        assert_eq!(state(&paths, &old), "open");
        assert!(app.found.is_none());
        assert_eq!(app.current_id().as_deref(), Some(old.as_str()), "the start is of the hit");

        // An open one starts straight away.
        let other = ids.into_iter().find(|id| *id != old).unwrap();
        app.recall("locale", true);
        let at = app.found.as_ref().unwrap().hits.iter().position(|h| h.task == other).unwrap();
        app.found.as_mut().unwrap().at = at;
        assert!(matches!(key(&mut app, KeyCode::Char('s')), Some(Away::Attach { .. })));
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
    fn tab_completes_a_directory_as_a_shell_does() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["projects/varde", "projects/vardo", "projects/web", "projects/.hidden"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("projects/varfile"), "").unwrap();
        let root = dir.path().display().to_string();
        let complete_dir = |line: &mut String| complete_dir(line).map(|m| m.names).unwrap_or_default();
        let mut line = format!("{root}/pro");
        assert!(complete_dir(&mut line).is_empty());
        assert_eq!(line, format!("{root}/projects/"), "one match, closed with a slash");
        assert_eq!(complete_dir(&mut line), ["varde", "vardo", "web"], "no prefix: listed, hidden left out");
        line.push('v');
        assert!(complete_dir(&mut line).is_empty());
        assert_eq!(line, format!("{root}/projects/vard"), "as far as they agree, files left out");
        assert_eq!(complete_dir(&mut line), ["varde", "vardo"]);
        assert_eq!(line, format!("{root}/projects/vard"));
        line.push('e');
        complete_dir(&mut line);
        assert_eq!(line, format!("{root}/projects/varde/"));
        let mut hidden = format!("{root}/projects/.");
        complete_dir(&mut hidden);
        assert_eq!(hidden, format!("{root}/projects/.hidden/"));
        let mut nothing = format!("{root}/nowhere/x");
        assert!(complete_dir(&mut nothing).is_empty());
        assert_eq!(nothing, format!("{root}/nowhere/x"));
    }

    #[test]
    fn tab_in_the_new_tasks_project_line_completes_lists_and_picks() {
        let (dir, paths) = temp_paths();
        for d in ["alpha", "alps", "alto"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        let mut app = board(&paths, &[]);
        key(&mut app, KeyCode::Char('n'));
        key(&mut app, KeyCode::Char('T'));
        key(&mut app, KeyCode::Enter);
        let Mode::Where(_, line) = &mut app.mode else { panic!("not asking the project") };
        line.set(&format!("{}/a", dir.path().display()));
        let line = |app: &App| match &app.mode {
            Mode::Where(_, l) => l.text().rsplit_terminator('/').next().unwrap().to_string(),
            _ => panic!("left the prompt"),
        };
        key(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "al", "common prefix");
        key(&mut app, KeyCode::Tab);
        assert!(screen(&mut app, 120, 10).contains("alpha  alps  alto"));
        key(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "alpha", "the next Tab picks the first");
        key(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "alps");
        key(&mut app, KeyCode::BackTab);
        key(&mut app, KeyCode::BackTab);
        assert_eq!(line(&app), "alto", "Shift-Tab goes back, round the end");
        assert!(screen(&mut app, 120, 10).contains("alpha  alps  alto"), "listed while picking");
        key(&mut app, KeyCode::Char('x'));
        assert!(!screen(&mut app, 120, 10).contains("alpha  alps"), "gone at another key");
        assert!(matches!(&app.mode, Mode::Where(_, l) if l.text().ends_with("/alto/x")), "the pick kept");
    }

    #[test]
    fn the_picked_directory_stays_in_view() {
        let (dir, paths) = temp_paths();
        let names: Vec<String> = (0..20).map(|i| format!("dir{i:02}")).collect();
        for n in &names {
            std::fs::create_dir(dir.path().join(n)).unwrap();
        }
        let mut app = board(&paths, &[]);
        app.mode = Mode::Where(Draft::default(), Field::new(&format!("{}/", dir.path().display())));
        key(&mut app, KeyCode::Tab);
        for _ in 0..16 {
            key(&mut app, KeyCode::Tab);
        }
        assert!(matches!(&app.mode, Mode::Where(_, l) if l.text().ends_with("/dir14/")));
        let s = screen(&mut app, 80, 10);
        assert!(s.contains("dir14") && !s.contains("dir00") && s.contains("…"), "{s}");
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
    fn gg_g_and_home_jump_to_the_ends() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two", "Three"]);
        let first = app.current_id();
        key(&mut app, KeyCode::Char('G'));
        assert_eq!(app.current().unwrap().title, "One", "the oldest is last");
        key(&mut app, KeyCode::Home);
        assert_eq!(app.current_id(), first);
        key(&mut app, KeyCode::Char('G'));
        key(&mut app, KeyCode::Char('g'));
        assert_eq!(app.current().unwrap().title, "One", "one g waits");
        key(&mut app, KeyCode::Char('g'));
        assert_eq!(app.current_id(), first);
        // A g then another key is that key.
        key(&mut app, KeyCode::Char('g'));
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "Two");
    }

    /// Put `title` in the group a board shows it in.
    fn put(paths: &Paths, title: &str, group: &str) {
        let store = paths.open(BUSY).unwrap();
        let id = task::create(&store, title, None, None, None, 1).unwrap();
        let sql = match group {
            "inbox" => "UPDATE task SET inbox = 1 WHERE id = ?1",
            "done" => "UPDATE task SET state = 'done', closed = 9e15 WHERE id = ?1",
            "parked" => "UPDATE task SET state = 'parked' WHERE id = ?1",
            _ => return,
        };
        store.conn.execute(sql, [&id]).unwrap();
    }

    #[test]
    fn every_group_is_always_there_and_z_folds_any() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Open one"]);
        put(&paths, "Later", "parked");
        app.refresh().unwrap();
        let s = screen(&mut app, 80, 12);
        for g in ["Needs you  0", "Working  0", "Open  1", "Inbox  0", "Done  0", "Parked  1 ▸"] {
            assert!(s.contains(g), "{g}:\n{s}");
        }
        assert!(!s.contains("Later") && !s.contains("Dropped"), "{s}");
        // The folded group is a stop on the way down; RET opens it.
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.group_at, Some("parked"));
        assert!(key(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.current().unwrap().title, "Later");
        // z folds the group at point, any group, and opens it again.
        key(&mut app, KeyCode::Char('k'));
        key(&mut app, KeyCode::Char('z'));
        let s = screen(&mut app, 80, 12);
        assert!(s.contains("Open  1 ▸") && !s.contains("Open one"), "{s}");
        key(&mut app, KeyCode::Char('z'));
        assert_eq!(app.current().unwrap().title, "Open one");
    }

    #[test]
    fn tab_cycles_what_needs_you_and_braces_move_by_group() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Open one", "Open two"]);
        put(&paths, "In", "inbox");
        app.refresh().unwrap();
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.message, "nothing needs you");
        // Needs you is what varde says: rows put there by hand stand in.
        let needs = app.groups.iter().position(|g| g.key == "needs_you").unwrap();
        let open = app.groups.iter().position(|g| g.key == "open").unwrap();
        let moved: Vec<Row> = app.groups[open].rows.drain(..).collect();
        app.groups[needs].rows = moved;
        app.layout();
        // Where the board opened, on the first that needs you, the first
        // Tab stays there.
        assert_eq!(app.current().unwrap().title, "Open two");
        key_only(&mut app, KeyCode::Tab);
        assert_eq!(
            (app.current().unwrap().title.as_str(), app.message.as_str()),
            ("Open two", "needs you: 1 of 2")
        );
        key_only(&mut app, KeyCode::Tab);
        assert_eq!(app.current().unwrap().title, "Open one");
        app.select(app.items.len() - 1);
        key_only(&mut app, KeyCode::Tab);
        assert_eq!(
            (app.current().unwrap().title.as_str(), app.message.as_str()),
            ("Open two", "needs you: 1 of 2")
        );
        key_only(&mut app, KeyCode::Tab);
        key_only(&mut app, KeyCode::Tab);
        assert_eq!(app.current().unwrap().title, "Open two", "round again");
        key_only(&mut app, KeyCode::BackTab);
        assert_eq!(app.current().unwrap().title, "Open one");
        key_only(&mut app, KeyCode::Char('}'));
        assert_eq!(app.current().unwrap().title, "In", "past the empty groups");
        key_only(&mut app, KeyCode::Char('{'));
        assert_eq!(app.current().unwrap().title, "Open two", "the top of the one before");
        key_only(&mut app, KeyCode::Char('j'));
        key_only(&mut app, KeyCode::Char('{'));
        assert_eq!(app.current().unwrap().title, "Open two", "the top of its own first");
    }

    #[test]
    fn half_a_page_moves_the_list_or_scrolls_the_pane() {
        let (_dir, paths) = temp_paths();
        let titles: Vec<String> = (0..30).map(|i| format!("Task {i}")).collect();
        let mut app = board(&paths, &titles.iter().map(String::as_str).collect::<Vec<_>>());
        screen(&mut app, 80, 24);
        let ctrl = |app: &mut App, c| app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        ctrl(&mut app, 'd');
        assert_eq!(app.current().unwrap().title, "Task 18", "11 rows down");
        ctrl(&mut app, 'u');
        assert_eq!(app.current().unwrap().title, "Task 29");
        let body: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET body = ?1", [body.join("\n")]).unwrap();
        key(&mut app, KeyCode::Char('v'));
        screen(&mut app, 80, 24);
        ctrl(&mut app, 'd');
        assert!(app.view.scroll > 0 && app.current().unwrap().title == "Task 29");
    }

    /// A key, without reading the board again.
    fn key_only(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn home_shows_the_first_group_header_again() {
        let (_dir, paths) = temp_paths();
        let titles: Vec<String> = (0..30).map(|i| format!("Task {i}")).collect();
        let mut app = board(&paths, &titles.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(matches!(app.items[0], Item::Header(_)), "a header first");
        let header = format!("{}  ", app.group(0).label);
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
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        // What follows the title ends at the same place on every row.
        let end = |y: u16| (0..60).rev().find(|&x| buf[(x, y)].symbol() != " ");
        let rows: Vec<u16> = (0..10).filter(|&y| (0..60).any(|x| buf[(x, y)].symbol() == "…")).collect();
        let plain = (0..10).find(|&y| (0..60).any(|x| buf[(x, y)].symbol() == "P")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(end(rows[0]), end(plain));
    }

    #[test]
    fn needs_you_rows_leave_out_your_turn_and_say_needs_input() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["Waiting", "Asking"]);
        let needs = app.groups.iter().position(|g| g.key == "needs_you").unwrap();
        let open = app.groups.iter().position(|g| g.key == "open").unwrap();
        let mut moved: Vec<Row> = app.groups[open].rows.drain(..).collect();
        moved[0].status = "waiting";
        moved[1].status = "blocked";
        app.groups[needs].rows = moved;
        app.layout();
        let s = screen(&mut app, 80, 12);
        assert!(!s.contains("your turn") && s.contains("needs input"), "{s}");
    }

    #[test]
    fn the_board_dims_under_an_open_pane() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two"]);
        let colour = |app: &mut App| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            term.draw(|f| app.draw(f)).unwrap();
            let buf = term.backend().buffer().clone();
            // The title of the row not selected: "One", under "Two".
            let y = (0..24).find(|&y| buf[(3, y)].symbol() == "O" && buf[(4, y)].symbol() == "n").unwrap();
            buf[(3, y)].fg
        };
        assert_ne!(colour(&mut app), ratatui::style::Color::DarkGray);
        key(&mut app, KeyCode::Char('v'));
        assert_eq!(colour(&mut app), ratatui::style::Color::DarkGray);
        assert!(screen(&mut app, 80, 24).contains("╭ Two  "), "the view's title in its border");
        // The form is a pane too, and so is the help, over the rows.
        key(&mut app, KeyCode::Char('q'));
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(colour(&mut app), ratatui::style::Color::DarkGray);
        assert!(screen(&mut app, 80, 24).contains("╭ New task "));
        app.mode = Mode::Help;
        assert!(screen(&mut app, 80, 24).contains("╭ varde "));
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
    #[cfg_attr(not(target_os = "linux"), ignore = "uses util-linux script")]
    fn in_a_popup_going_to_a_session_quits_the_board_and_starting_one_does_not() {
        let (dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two"]);
        in_a_project(&paths);
        app.refresh().unwrap();
        let Some(server) = Server::start(dir.path()) else { return eprintln!("no tmux; skipped") };
        app.tmux = Some(tmux::at(dir.path().join("tmux.sock")));
        app.popup = true;
        // switch-client needs a client attached, as a popup's is.
        let attach = format!("tmux -S {} attach -t board", server.0.display());
        let mut client = std::process::Command::new("script")
            .args(["-qfc", &attach, "/dev/null"])
            .env("TERM", "xterm-256color")
            .env_remove("TMUX")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let attached = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            let out =
                std::process::Command::new("tmux").arg("-S").arg(&server.0).arg("list-clients").output();
            out.is_ok_and(|o| !o.stdout.is_empty())
        });
        if attached {
            key(&mut app, KeyCode::Char('s'));
            assert!(app.message.contains("in a window of its own") && !app.quit, "{}", app.message);
            key(&mut app, KeyCode::Char('j'));
            key(&mut app, KeyCode::Enter);
            assert!(app.quit, "went to it: {}", app.message);
        }
        let _ = client.kill();
        let _ = client.wait();
        assert!(attached, "no tmux client attached");
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
        let keys = ['j', 'k', 'v', 'j', 'v', ' '].map(KeyCode::Char);
        for code in keys {
            app.key(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(!app.stale, "{code:?}");
        }
        app.key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
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
        key(&mut app, KeyCode::Char('q'));
        key(&mut app, KeyCode::Char('?'));
        let help = screen(&mut app, 80, 24);
        assert!(help.contains("diff of its work") && help.contains("redraw and refresh"), "{help}");
        assert!(help.contains("move to another project"), "{help}");
        // It opens with what each group holds.
        for group in ["Needs you", "Working", "Open", "Inbox  ", "Done", "Parked"] {
            assert!(help.contains(group), "{group}:\n{help}");
        }
        assert!(
            help.contains("a       accept an Inbox task") && help.contains("next / previous task"),
            "{help}"
        );
        assert!(
            help.contains("Groups   ● working or unread") && help.contains("closed today or not landed"),
            "{help}"
        );
        // What a cairn is, in the body under Groups, not the border.
        let cairn = help.lines().position(|l| l.contains("Cairn    what sessions learned")).unwrap();
        assert!(help.lines().nth(cairn - 1).unwrap().contains("Groups"), "{help}");
        assert!(help.lines().nth(cairn).unwrap().contains("› yours  ? stale: its code is gone"), "{help}");
        // Taller than 80x24, it scrolls to its end; any other key closes it.
        assert!(help.contains("↓ j more") && !help.contains("u undoes"), "{help}");
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char('j'));
        let end = screen(&mut app, 80, 24);
        assert!(
            end.contains("u undoes the last x, d, P or X in the view") && !end.contains("j more"),
            "{end}"
        );
        key(&mut app, KeyCode::Char('j'));
        assert!(matches!(app.mode, Mode::Help));
        key(&mut app, KeyCode::Char('x'));
        assert!(matches!(app.mode, Mode::Normal));
        // In a 90%x85% popup of an 80x24 terminal, too, nothing collides.
        key(&mut app, KeyCode::Char('?'));
        let popup = screen(&mut app, 72, 20);
        assert!(
            popup.contains("Tab/S-Tab cycle what needs you") && popup.contains("d/P/x   done"),
            "{popup}"
        );
        assert!(popup.contains("? stale: its code is gone"), "{popup}");
    }

    #[test]
    fn the_views_project_tab_shows_the_project_cairn_and_e_then_edits_it() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        key(&mut app, KeyCode::Char('v'));
        key(&mut app, KeyCode::Char(']'));
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
        assert!(s.contains(" Task   Project ") && s.contains("what p's tasks learned"), "{s}");
        assert!(s.contains("Gotchas") && s.contains("tests fail on macOS"), "{s}");
        key(&mut app, KeyCode::Char('['));
        assert!(!screen(&mut app, 80, 24).contains("tests fail on macOS"), "[ shows the task again");
        key(&mut app, KeyCode::Char(']'));
        key(&mut app, KeyCode::Char('v'));
        assert!(!app.detail, "v closes it, on either tab");
        assert!(matches!(key(&mut app, KeyCode::Char('E')), Some(Away::Edit(_))), "the task's cairn");
        key(&mut app, KeyCode::Char('v'));
        assert!(!app.project_detail, "v opens on the task");
        key(&mut app, KeyCode::Char(']'));
        assert!(matches!(key(&mut app, KeyCode::Char('E')), Some(Away::EditProject(p)) if p == "/p"));
        // `cairn` in [keys] opens the view on the Project tab.
        app.keys = Keys::of(&[("cairn".to_string(), "C".to_string())].into());
        key(&mut app, KeyCode::Char('q'));
        key(&mut app, KeyCode::Char('C'));
        assert!(app.detail && app.project_detail);
    }

    #[test]
    fn e_edits_title_and_context_in_the_form() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        let id = app.current_id().unwrap();
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET body = 'Old context.'", []).unwrap();
        let text = |col: &str| -> Option<String> {
            store.conn.query_row(&format!("SELECT {col} FROM task"), [], |r| r.get(0)).unwrap()
        };

        assert!(key(&mut app, KeyCode::Char('e')).is_none(), "no editor");
        assert!(
            matches!(&app.mode, Mode::New(d) if d.title.text() == "One" && d.body.text() == "Old context.")
        );
        let s = screen(&mut app, 100, 24);
        assert!(s.contains("Edit task") && s.contains("RET save"), "{s}");
        app.paste(" more");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Enter);
        app.paste("New line.");
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.message.starts_with("saved "), "{}", app.message);
        assert_eq!(text("title").as_deref(), Some("One more"));
        assert_eq!(text("body").as_deref(), Some("Old context.\nNew line."));
        assert_eq!(app.rows().count(), 1, "nothing created");

        // A cleared title is cut from the context, as a new task's is; with
        // no context either, the refusal shows over the form, which stays.
        key(&mut app, KeyCode::Char('e'));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.message.starts_with("saved "), "{}", app.message);
        assert_eq!(text("title").as_deref(), Some("Old context"));
        assert_eq!(text("prompt_title").as_deref(), Some("Old context"), "the model may name it");
        key(&mut app, KeyCode::Char('e'));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        key(&mut app, KeyCode::Tab);
        for _ in 0..3 {
            app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        }
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::New(_)), "the form stays");
        assert!(screen(&mut app, 100, 24).contains("a task needs a title or a context"));
        key(&mut app, KeyCode::Char('x'));
        assert!(!screen(&mut app, 100, 24).contains("needs a title"), "gone at the next key");
        app.mode = Mode::Normal;
        store.conn.execute("UPDATE task SET title = 'One more', prompt_title = NULL", []).unwrap();
        app.refresh().unwrap();

        // Esc changes nothing, and keeps what was typed for the next e,
        // as often as it is pressed; nothing changed, it keeps nothing.
        key(&mut app, KeyCode::Char('e'));
        app.paste("!");
        key(&mut app, KeyCode::Esc);
        assert_eq!(text("title").as_deref(), Some("One more"));
        assert_eq!(app.message, "changes to \"One more\" kept: e on it brings them back");
        for _ in 0..2 {
            key(&mut app, KeyCode::Char('e'));
            assert!(matches!(&app.mode, Mode::New(d) if d.title.text() == "One more!"));
            key(&mut app, KeyCode::Esc);
        }
        key(&mut app, KeyCode::Char('e'));
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.message, "draft dropped");
        key(&mut app, KeyCode::Char('e'));
        assert!(matches!(&app.mode, Mode::New(d) if d.title.text() == "One more"));
        key(&mut app, KeyCode::Esc);

        // A cairn changes nothing: `e` is the form, `E` the whole cairn in $EDITOR.
        let cairn =
            varde_core::cairn::Cairn::from_edit("Status: Looked.\nFindings:\n- A.\n", &Default::default());
        varde_core::memory::put_cairn(&store.conn, &id, &cairn, 0, "user", None, now_ms()).unwrap();
        assert!(key(&mut app, KeyCode::Char('e')).is_none() && matches!(app.mode, Mode::New(_)));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(key(&mut app, KeyCode::Char('E')), Some(Away::Edit(t)) if t == id));

        store.conn.execute("UPDATE task SET external = 'org'", []).unwrap();
        app.refresh().unwrap();
        assert!(matches!(key(&mut app, KeyCode::Char('e')), Some(Away::Edit(_))), "linked: $EDITOR");
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
        let refused = "a task needs a title or a context";
        assert!(matches!(app.mode, Mode::New(_)) && app.message == refused, "{}", app.message);
        key(&mut app, KeyCode::Esc);
        assert!(app.kept.is_empty(), "nothing typed, nothing kept");

        // Only a context: the title is cut from it, as from a first prompt.
        key(&mut app, KeyCode::Char('n'));
        key(&mut app, KeyCode::Tab);
        app.paste("why does the cache key ignore the region in the locale cache");
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        key(&mut app, KeyCode::Enter);
        let titled: (String, String) = store
            .conn
            .query_row("SELECT title, prompt_title FROM task WHERE body LIKE 'why%'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(titled.0, "why does the cache key ignore the region…");
        assert_eq!(titled.1, titled.0, "the model may name it");
        store.conn.execute("DELETE FROM task WHERE body LIKE 'why%'", []).unwrap();

        key(&mut app, KeyCode::Char('n'));
        app.paste("Two");
        key(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, 200, 24).contains(&format!("Project for this task: {}", old.display())));
        // Esc goes back to the form, the title kept.
        key(&mut app, KeyCode::Esc);
        assert!(matches!(&app.mode, Mode::New(d) if d.title.text() == "Two"));
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
        assert!(matches!(&app.mode, Mode::Where(d, _) if d.title.text() == "Three"), "the draft is kept");

        // Esc twice puts it away, and n takes it up again, as often as
        // that; cleared, Esc drops it.
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.message, "new task kept: n brings it back");
        key(&mut app, KeyCode::Char('n'));
        assert!(matches!(&app.mode, Mode::New(d) if d.title.text() == "Three"));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('n'));
        assert!(matches!(&app.mode, Mode::New(d) if d.title.text() == "Three"));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.message, "draft dropped");
        key(&mut app, KeyCode::Char('n'));
        assert!(matches!(&app.mode, Mode::New(d) if d.is_empty()));
    }

    #[test]
    fn a_draft_outlives_the_board_quitting_or_starting_again_as_a_newer_varde() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &[]);
        key(&mut app, KeyCode::Char('n'));
        app.paste("Half written\nwith context");
        app.put_draft();
        let mut again = board(&paths, &[]);
        again.take_draft();
        assert_eq!(again.message, "your new task is kept: n brings it back");
        key(&mut again, KeyCode::Char('n'));
        assert!(
            matches!(&again.mode, Mode::New(d) if d.title.text() == "Half written" && d.body.text() == "with context")
        );
        assert!(!again.drafts_file().exists());
    }

    #[test]
    fn a_board_kept_to_its_project_makes_new_tasks_there_and_shows_one_made_elsewhere() {
        let (dir, paths) = temp_paths();
        let dir = dir.path().canonicalize().unwrap();
        let (here, there) = (dir.join("here"), dir.join("there"));
        for d in [&here, &there] {
            std::fs::create_dir(d).unwrap();
        }
        let here_s = here.to_string_lossy().to_string();
        task::create(&paths.open(BUSY).unwrap(), "Old", None, Some(&here_s), None, 1).unwrap();
        let mut app = App::new(&paths, None);
        app.here = Some(here_s.clone());
        app.refresh().unwrap();
        key(&mut app, KeyCode::Char('n'));
        app.paste("No question asked");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal) && app.message.starts_with("created "), "{}", app.message);
        assert_eq!(app.current().unwrap().title, "No question asked");

        // Showing every project, it asks; a task made in another project
        // is shown, and selected, so RET starts it.
        key(&mut app, KeyCode::Char('.'));
        key(&mut app, KeyCode::Char('.'));
        app.scoped = false;
        key(&mut app, KeyCode::Char('n'));
        app.paste("Over there");
        key(&mut app, KeyCode::Enter);
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste(&there.to_string_lossy());
        app.scoped = true;
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.current().unwrap().title, "Over there", "{}", app.message);
        assert!(!app.scoped && app.message.contains("showing every project"), "{}", app.message);
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

    #[test]
    fn z_opens_a_group_on_the_task_it_was_folded_from() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two", "Three"]);
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "Two");
        key(&mut app, KeyCode::Char('z'));
        assert!(app.current().is_none());
        key(&mut app, KeyCode::Char('z'));
        assert_eq!(app.current().unwrap().title, "Two");
    }

    #[test]
    fn marked_tasks_are_counted_and_acted_on_together() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One", "Two", "Three"]);
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char(' '));
        let s = screen(&mut app, 80, 12);
        assert!(s.contains("2 marked  space mark") && s.contains("x drop"), "{s}");
        // x asks about them all, naming how many; Esc keeps them marked.
        key(&mut app, KeyCode::Char('x'));
        let s = screen(&mut app, 80, 12);
        assert!(s.contains("Drop the 2 marked tasks?") && s.matches('*').count() == 2, "{s}");
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.marked.len(), 2);
        key(&mut app, KeyCode::Char('d'));
        assert_eq!(app.message, "2 tasks done; u reopens");
        assert_eq!(app.rows().filter(|r| r.state == "done").count(), 2);
        // u reopens them, the selection on them; once.
        key(&mut app, KeyCode::Char('u'));
        assert_eq!(app.message, "2 tasks reopened");
        assert_eq!(app.rows().filter(|r| r.state == "done").count(), 0);
        assert_eq!(app.current().unwrap().title, "Three");
        key(&mut app, KeyCode::Char('u'));
        assert_eq!(app.message, "nothing closed here to reopen");
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char('d'));
        // One mark is a mark too.
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char('x'));
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.marked.len(), 1);
    }

    #[test]
    fn the_view_reads_the_store_again_only_when_it_changed() {
        let (_dir, paths) = temp_paths();
        let mut app = board(&paths, &["One"]);
        key(&mut app, KeyCode::Char('v'));
        screen(&mut app, 80, 24);
        app.refresh().unwrap();
        assert!(app.view.loaded(), "nothing changed");
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE cairn SET updated = 1", []).unwrap();
        task::set(&store, &app.current_id().unwrap(), Some("One more"), None).unwrap();
        app.refresh().unwrap();
        assert!(!app.view.loaded(), "the task changed");
    }

    #[test]
    fn a_first_start_in_this_terminal_asks_on_the_board_before_leaving_it() {
        let (dir, paths) = temp_paths();
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
        let mut app = board(&paths, &["Start me"]);
        let store = paths.open(BUSY).unwrap();
        store.conn.execute("UPDATE task SET project = ?1", [repo.to_string_lossy()]).unwrap();
        app.refresh().unwrap();
        assert!(key(&mut app, KeyCode::Enter).is_none(), "the screen is the board's still");
        if !matches!(app.mode, Mode::Worktree(..)) {
            // A config of the user's that settles it asks nothing.
            return eprintln!("worktree settled by the config; skipped");
        }
        let s = screen(&mut app, 80, 10);
        assert!(s.contains("Start \"Start me\" in a worktree of its own (repo)?  Y/n · Esc cancel"), "{s}");
        assert!(matches!(key(&mut app, KeyCode::Char('n')), Some(Away::Attach { .. })), "n starts it");
    }
}
