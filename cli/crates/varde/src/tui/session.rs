//! Where a started task runs: in this terminal, which the board leaves,
//! or in a tmux window of its own, which the board also watches.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use varde_core::board::{Board, Row};
use varde_core::store::Store;
use varde_core::task;

use super::{is_closed, App, Asked, Away, Mode, Pick, Start, BUSY, OPEN_GROUPS};
use crate::{now_ms, run, start, tmux};

impl App<'_> {
    /// Inside tmux: a task that comes to need you rings its window's
    /// bell, a task whose window is on screen has been seen, and the
    /// windows a board of this store opened for tasks closed elsewhere go
    /// once their shell is idle. Another store's windows are not this
    /// board's to judge, nor are tasks it has not seen yet.
    pub(super) fn watch_tmux(&mut self, store: &Store, board: &mut Board, now: i64) {
        let Some(t) = &self.tmux else { return };
        let Ok(panes) = t.panes() else { return };
        let open: HashSet<&str> = board
            .groups
            .iter()
            .filter(|g| OPEN_GROUPS.contains(&g.key))
            .flat_map(|g| g.rows.iter().map(|r| r.task.as_str()))
            .collect();
        let home = self.home();
        for p in
            panes.iter().filter(|p| p.owned && p.home == home && !open.contains(p.task.as_str()) && p.idle())
        {
            if state_of(store, &p.task).is_some_and(|s| is_closed(&s)) {
                let _ = t.kill(&p.id);
            }
        }
        let first = self.rung.is_none();
        let rung = self.rung.get_or_insert_with(HashSet::new);
        let unread: Vec<&mut Row> =
            board.groups.iter_mut().flat_map(|g| g.rows.iter_mut()).filter(|r| r.unread).collect();
        rung.retain(|t| unread.iter().any(|r| r.task == *t));
        for r in unread {
            let tty = r.run.as_ref().and_then(|run| start::tty_of(store, &run.id).ok().flatten());
            let Some(p) = tmux::find(&panes, &r.task, tty.as_deref()) else { continue };
            if p.shown {
                if varde_core::store::mark_seen(&store.conn, &r.task, now).is_ok() {
                    r.unread = false;
                }
            } else if rung.insert(r.task.clone()) && !first {
                p.bell();
            }
        }
    }

    /// Start the selected task. In tmux a session starts in a window of
    /// its own, and the board keeps the screen; else it gets this
    /// terminal, once the board has asked what a first start needs.
    pub(super) fn begin(&mut self, start: Start) -> Option<Away> {
        let id = self.current_id()?;
        self.launch(&id, start, None, None)
    }

    pub(super) fn launch(
        &mut self,
        id: &str,
        start: Start,
        worktree: Option<bool>,
        project: Option<PathBuf>,
    ) -> Option<Away> {
        if self.tmux.is_some() {
            self.attach_tmux(id, start, worktree, project);
            None
        } else if !start.go {
            self.message = "starting in the background needs tmux; RET starts it here".into();
            None
        } else {
            match self.prepare_here(id, start, worktree, project) {
                Ok(away) => away,
                Err(e) => {
                    self.report(Err(e));
                    None
                }
            }
        }
    }

    /// Answer a question asked about starting a task: start that task,
    /// whatever is selected now, unless it was closed or reopened since.
    pub(super) fn answer(
        &mut self,
        asked: Asked,
        start: Start,
        worktree: Option<bool>,
        project: Option<PathBuf>,
    ) -> Option<Away> {
        let now = self.paths.open(BUSY).ok().and_then(|s| state_of(&s, &asked.task));
        if now.as_ref() != Some(&asked.state) {
            let now = now.as_deref().unwrap_or("gone");
            self.message = format!("{} is {now} now; not started", task::short(&asked.task));
            return None;
        }
        self.launch(&asked.task, start, worktree, project)
    }

    /// Prepare starting `id`, or ask on the board what a first start
    /// needs to know, whose answer starts it again: `None` then.
    fn prepare(
        &mut self,
        store: &mut Store,
        id: &str,
        start: Start,
        worktree: Option<bool>,
        project: Option<PathBuf>,
    ) -> Result<Option<start::Outcome>> {
        let asked = Asked { task: id.to_string(), state: state_of(store, id).unwrap_or_default() };
        let opts = start::Options {
            want: start.want,
            worktree,
            context: None,
            agent: None,
            project: project.clone(),
            take_over: start.take_over,
            run: start.run.clone(),
        };
        match start::prepare(store, id, opts, false)? {
            start::Outcome::Ask(start::Ask::TakeOver { .. }) => {
                self.mode = Mode::TakeOver(start, asked);
                Ok(None)
            }
            start::Outcome::Ask(start::Ask::Project { choices }) => {
                self.mode = Mode::Project(start, asked, Pick::new("Project for this task", choices, None));
                Ok(None)
            }
            start::Outcome::Ask(start::Ask::Worktree) => {
                self.mode = Mode::Worktree(start, asked, project);
                Ok(None)
            }
            outcome => Ok(Some(outcome)),
        }
    }

    /// Outside tmux: a run ready leaves the board for it; a question, or
    /// a session already running elsewhere, keeps the board.
    fn prepare_here(
        &mut self,
        id: &str,
        start: Start,
        worktree: Option<bool>,
        project: Option<PathBuf>,
    ) -> Result<Option<Away>> {
        let mut store = self.paths.open(BUSY)?;
        Ok(match self.prepare(&mut store, id, start, worktree, project)? {
            None | Some(start::Outcome::Ask(_)) => None,
            Some(start::Outcome::Focus { run, .. }) => {
                let tty = start::tty_of(&store, &run)?;
                self.message = self.running_elsewhere(id, tty.as_deref());
                None
            }
            Some(start::Outcome::Ready { run, notices, .. }) => {
                Some(Away::Attach { task: id.to_string(), run, notices })
            }
        })
    }

    /// Run the session in a tmux window of its own and go to it, or start
    /// it there in the background; or go to where it already runs.
    fn attach_tmux(&mut self, id: &str, start: Start, worktree: Option<bool>, project: Option<PathBuf>) {
        let Some(tmux) = self.tmux.take() else { return };
        let result = self.in_tmux(&tmux, id, start, worktree, project);
        self.tmux = Some(tmux);
        self.report(result);
    }

    fn in_tmux(
        &mut self,
        tmux: &tmux::Tmux,
        id: &str,
        start: Start,
        worktree: Option<bool>,
        project: Option<PathBuf>,
    ) -> Result<String> {
        let mut store = self.paths.open(BUSY)?;
        let title: String =
            store.conn.query_row("SELECT title FROM task WHERE id = ?1", [id], |r| r.get(0))?;
        let go = start.go;
        match self.prepare(&mut store, id, start, worktree, project)? {
            None | Some(start::Outcome::Ask(_)) => Ok(String::new()),
            Some(start::Outcome::Focus { run, .. }) => {
                let tty = start::tty_of(&store, &run)?;
                Ok(if !go {
                    format!("{} is already running; RET goes to it", task::short(id))
                } else if tmux.focus(id, tty.as_deref())? {
                    varde_core::store::mark_seen(&store.conn, id, now_ms())?;
                    self.quit |= self.popup;
                    String::new()
                } else {
                    self.running_elsewhere(id, tty.as_deref())
                })
            }
            Some(start::Outcome::Ready { run, cwd, mut notices }) => {
                let title = varde_core::title::clean(&title);
                let run = crate::memory_cmd::short_run(&store.conn, &run);
                tmux.open(id, &title, &cwd, &self.home(), &format!("{} run {run}", typed_exe()), go)?;
                if !go {
                    notices.push(format!("started {} in a window of its own", task::short(id)));
                }
                self.quit |= self.popup && go;
                Ok(notices.join("; "))
            }
        }
    }

    /// That `id` already runs where the board cannot go to it, and how to
    /// have another session of it.
    fn running_elsewhere(&self, id: &str, tty: Option<&str>) -> String {
        let fork = self.keys.label(super::keys::Act::Fork);
        format!("{} is already running {}; {fork} forks it", task::short(id), where_runs(id, tty))
    }

    /// The store, as the windows the board opens are tagged with it.
    pub(super) fn home(&self) -> String {
        self.paths.tag()
    }

    /// Run the session prepared as `run` in this terminal, which the
    /// board has left: a banner says which task it is and how to come
    /// back, and the first time, that tmux runs sessions side by side. A
    /// session that fails leaves its last words on screen until a key.
    pub(super) fn attach_here(&mut self, id: &str, run: &str, mut notices: Vec<String>) -> Result<String> {
        let title = self.paths.open(BUSY).ok().and_then(|s| {
            s.conn.query_row("SELECT title FROM task WHERE id = ?1", [id], |r| r.get::<_, String>(0)).ok()
        });
        let title = varde_core::title::clean(&title.unwrap_or_default());
        let short = task::short(id);
        let width = ratatui::crossterm::terminal::size().map_or(80, |(w, _)| w as usize);
        let head = crate::list::clip(&format!("── {short}  {title} "), width);
        let rule = "─".repeat(width.saturating_sub(unicode_width::UnicodeWidthStr::width(head.as_str())));
        println!("\n\x1b[1m{head}{rule}\x1b[0m");
        println!("\x1b[2mThe session has this terminal now. When it ends, you are back on the board.\x1b[0m");
        let tip = self.paths.home.join("tmux-tip");
        if !tip.exists() {
            println!(
                "\x1b[2mTip: in tmux, sessions run side by side and the board stays: tmux new varde\x1b[0m"
            );
            let _ = std::fs::write(&tip, "");
        }
        // Said before the session takes the terminal, as attach says them.
        for n in &notices {
            println!("\x1b[2mvarde: {n}\x1b[0m");
        }
        println!();
        let code = run::run(self.paths, run)?;
        if code != 0 {
            println!(
                "\n\x1b[1mvarde:\x1b[0m the session exited with code {code}. Any key returns to the board."
            );
            super::term::any_key();
        }
        let ended = if code == 0 { "ended".into() } else { format!("exited with code {code}") };
        notices.insert(0, format!("back from {short}: its session {ended}"));
        Ok(notices.join("; "))
    }
}

/// Where `task`'s live session runs, as a person would say it: `in tmux
/// window main:2`, else `in another terminal (pts/7)`.
fn where_runs(task: &str, tty: Option<&str>) -> String {
    let t = tmux::here().unwrap_or_else(tmux::server);
    let panes = t.panes().unwrap_or_default();
    match (tmux::find(&panes, task, tty), tty) {
        (Some(p), _) => format!("in tmux window {}", p.window.split_whitespace().next().unwrap_or_default()),
        (None, Some(tty)) => format!("in another terminal ({})", tty.trim_start_matches("/dev/")),
        (None, None) => "in another terminal".into(),
    }
}

pub(super) fn state_of(store: &Store, task: &str) -> Option<String> {
    store.conn.query_row("SELECT state FROM task WHERE id = ?1", [task], |r| r.get(0)).ok()
}

/// This binary as typed into a shell: `varde` when that is it on PATH.
fn typed_exe() -> String {
    let exe = crate::exe();
    let on_path = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("varde")).find(|c| c.is_file()));
    let same = |c: PathBuf| c.canonicalize().ok().is_some_and(|c| exe.canonicalize().ok() == Some(c));
    if exe.as_os_str() == "varde" || on_path.is_some_and(same) {
        "varde".into()
    } else {
        shell_word(&exe.to_string_lossy())
    }
}

/// `s` as one word to a shell: as it is if nothing in it is special.
fn shell_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@,%=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_typed_as_one_word() {
        assert_eq!(shell_word("/usr/local/bin/varde"), "/usr/local/bin/varde");
        assert_eq!(shell_word("/My Tools/it's/varde"), r"'/My Tools/it'\''s/varde'");
    }
}
