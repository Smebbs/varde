//! Where a started task runs: in this terminal, which the board leaves,
//! or in a tmux window of its own, which the board also watches.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use varde_core::board::{Board, Row};
use varde_core::plan::Want;
use varde_core::store::Store;
use varde_core::task;

use super::{App, Asked, Away, Mode, Start, BUSY, OPEN_GROUPS};
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
            if state_of(store, &p.task).is_some_and(|s| matches!(s.as_str(), "done" | "parked" | "dropped")) {
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
    /// its own, and the board keeps the screen.
    pub(super) fn begin(&mut self, start: Start) -> Option<Away> {
        if self.tmux.is_some() {
            if let Some(id) = self.current_id() {
                self.attach_tmux(&id, start, None, None);
            }
            None
        } else if !start.go {
            self.message = "starting in the background needs tmux; RET starts it here".into();
            None
        } else {
            Some(Away::Attach(start))
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
    ) {
        let now = self.paths.open(BUSY).ok().and_then(|s| state_of(&s, &asked.task));
        if now.as_ref() != Some(&asked.state) {
            let now = now.as_deref().unwrap_or("gone");
            self.message = format!("{} is {now} now; not started", task::short(&asked.task));
            return;
        }
        self.attach_tmux(&asked.task, start, worktree, project);
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
        let (title, state): (String, String) =
            store.conn.query_row("SELECT title, state FROM task WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let asked = Asked { task: id.to_string(), state };
        let opts = start::Options {
            want: start.want,
            worktree,
            context: None,
            agent: None,
            project: project.clone(),
        };
        match start::prepare(&mut store, id, opts, false)? {
            start::Outcome::Ask(start::Ask::Project { choices }) => {
                if choices.is_empty() {
                    anyhow::bail!("no project to offer: varde attach {} --project <dir>", task::short(id));
                }
                self.mode = Mode::Project(start, asked, choices, 0);
                Ok(String::new())
            }
            start::Outcome::Ask(start::Ask::Worktree) => {
                self.mode = Mode::Worktree(start, asked, project);
                Ok(String::new())
            }
            start::Outcome::Focus { run, .. } => {
                let tty = start::tty_of(&store, &run)?;
                Ok(if !start.go {
                    format!("{} is already running; RET goes to it", task::short(id))
                } else if tmux.focus(id, tty.as_deref())? {
                    varde_core::store::mark_seen(&store.conn, id, now_ms())?;
                    String::new()
                } else {
                    format!("already running outside tmux (run {}); F forks it", task::short(&run))
                })
            }
            start::Outcome::Ready { run, cwd, mut notices } => {
                let title = varde_core::title::clean(&title);
                tmux.open(id, &title, &cwd, &self.home(), &format!("{} run {run}", typed_exe()), start.go)?;
                if !start.go {
                    notices.push(format!("started {} in a window of its own", task::short(id)));
                }
                Ok(notices.join("; "))
            }
        }
    }

    /// The store, as the windows the board opens are tagged with it.
    pub(super) fn home(&self) -> String {
        let home = &self.paths.home;
        home.canonicalize().unwrap_or_else(|_| home.clone()).to_string_lossy().into_owned()
    }

    /// Run the session in this terminal, which the board has left.
    pub(super) fn attach_here(&mut self, want: Want) -> Result<String> {
        let Some((id, title)) = self.current().map(|r| (r.task.clone(), varde_core::title::clean(&r.title)))
        else {
            return Ok(String::new());
        };
        println!("── varde: {} {title}", task::short(&id));
        let mut store = self.paths.open(BUSY)?;
        let opts = start::Options { want, worktree: None, context: None, agent: None, project: None };
        match start::prepare(&mut store, &id, opts, true)? {
            start::Outcome::Focus { run, .. } => {
                Ok(format!("already running in another terminal (run {}); F forks it", task::short(&run)))
            }
            start::Outcome::Ask(_) => unreachable!("an interactive start asks on the terminal"),
            start::Outcome::Ready { run, mut notices, .. } => {
                drop(store);
                let code = run::run(self.paths, &run)?;
                notices.push(format!("session ended (exit {code})"));
                Ok(notices.join("; "))
            }
        }
    }
}

fn state_of(store: &Store, task: &str) -> Option<String> {
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
