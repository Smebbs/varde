//! The attach decision: given what is known about a task, what should
//! starting it do? One pure function, so every case is a table test and
//! no client re-implements any of it.

use crate::status::Status;

/// What the caller knows about one of the task's runs.
#[derive(Debug, Clone)]
pub struct RunFacts {
    pub id: String,
    pub cwd: Option<String>,
    pub status: Option<Status>,
    /// `Some` for a run `varde run` supervises: whether its process is
    /// alive. `None` for a session varde only hears about through hooks.
    pub process_alive: Option<bool>,
    /// Whether varde knows the run's process, launched or not: then
    /// `status` already says `Exited` once it is gone.
    pub process_known: bool,
    pub transcript_exists: bool,
    pub cwd_exists: bool,
    /// The terminal the session runs on, when its hooks could tell.
    pub tty: Option<String>,
}

impl RunFacts {
    /// A process varde launched is live while it runs, even before its
    /// first event arrives; a session varde only hears about is live by
    /// its events.
    pub fn live(&self) -> bool {
        match self.process_alive {
            Some(alive) => alive,
            None => {
                matches!(
                    self.status,
                    Some(Status::Working | Status::Delegated | Status::Waiting | Status::Blocked)
                )
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeFacts {
    pub path: String,
    pub branch: String,
    pub active: bool,
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Resume or attach where possible.
    Continue,
    /// Always a new conversation.
    Fresh,
    /// Branch off the latest conversation, leaving it as it is.
    Fork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// Already running in a terminal varde launched, or in one whose
    /// name the session's hooks gave: go there.
    Focus {
        run: String,
    },
    /// Continue the conversation under the same id.
    Resume {
        run: String,
        cwd: String,
    },
    /// A new conversation carrying the old one's history.
    Fork {
        run: String,
        cwd: String,
    },
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tree {
    /// Run in this existing directory.
    Use(String),
    /// Recreate the task's worktree at `path` from `branch`, then run there.
    Restore { path: String, branch: String },
    /// Make a new worktree for the task.
    Create,
    /// Run in the project root.
    Root,
    /// Policy is `ask` and the task has no worktree: the caller decides.
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub session: Session,
    pub tree: Tree,
}

/// `runs` are the task's runs, newest first. `policy` is
/// whether a fresh run gets a worktree: `Some(true)`, `Some(false)`, or
/// `None` to ask.
pub fn plan(runs: &[RunFacts], wt: Option<&WorktreeFacts>, want: Want, policy: Option<bool>) -> Plan {
    if want == Want::Continue {
        // The newest session still running where someone can go to it,
        // even behind a newer run that never got going. Behind one, only
        // a known process counts: a session known only by its events may
        // have died long ago without saying so.
        let reachable = |(i, r): &(usize, &RunFacts)| {
            r.process_alive == Some(true) || (r.live() && r.tty.is_some() && (*i == 0 || r.process_known))
        };
        if let Some((_, run)) = runs.iter().enumerate().find(reachable) {
            return Plan { session: Session::Focus { run: run.id.clone() }, tree: Tree::Root };
        }
    }
    if want != Want::Fresh {
        // The newest conversation that can still be picked up. A run that
        // never started leaves no transcript; the one before it may.
        if let Some(run) = runs.iter().find(|r| r.transcript_exists && r.cwd.is_some()) {
            let cwd = run.cwd.clone().unwrap_or_default();
            // Something else holds a live session, and a second front end
            // cannot attach to it: branching off is the only safe move.
            let fork = want == Want::Fork || run.live();
            let session = if fork {
                Session::Fork { run: run.id.clone(), cwd: cwd.clone() }
            } else {
                Session::Resume { run: run.id.clone(), cwd: cwd.clone() }
            };
            // The agent files a transcript under the directory it ran in,
            // so a conversation only continues from that same directory.
            if run.cwd_exists {
                return Plan { session, tree: Tree::Use(cwd) };
            }
            if let Some(w) = wt.filter(|w| std::path::Path::new(&cwd).starts_with(&w.path)) {
                return Plan {
                    session,
                    tree: Tree::Restore { path: w.path.clone(), branch: w.branch.clone() },
                };
            }
        }
    }

    let tree = match wt {
        Some(w) if w.active && w.exists => Tree::Use(w.path.clone()),
        Some(w) if w.active => Tree::Restore { path: w.path.clone(), branch: w.branch.clone() },
        _ => match policy {
            Some(true) => Tree::Create,
            Some(false) => Tree::Root,
            // The question is asked once per task: one that already ran
            // without a worktree was answered.
            None if wt.is_none() && !runs.is_empty() => Tree::Root,
            None => Tree::Ask,
        },
    };
    Plan { session: Session::Fresh, tree }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, status: Option<Status>, alive: Option<bool>) -> RunFacts {
        RunFacts {
            id: id.into(),
            cwd: Some("/p/.worktrees/x".into()),
            status,
            process_alive: alive,
            process_known: alive.is_some(),
            transcript_exists: true,
            cwd_exists: true,
            tty: None,
        }
    }

    fn wt(active: bool, exists: bool) -> WorktreeFacts {
        WorktreeFacts { path: "/p/.worktrees/x".into(), branch: "varde/x".into(), active, exists }
    }

    const W: Option<Status> = Some(Status::Waiting);
    const X: Option<Status> = Some(Status::Exited);

    #[test]
    fn a_new_task_asks_about_a_worktree_unless_told() {
        assert_eq!(plan(&[], None, Want::Continue, None).tree, Tree::Ask);
        assert_eq!(plan(&[], None, Want::Continue, Some(true)).tree, Tree::Create);
        assert_eq!(plan(&[], None, Want::Continue, Some(false)).tree, Tree::Root);
    }

    #[test]
    fn a_session_live_in_our_terminal_is_focused() {
        let p = plan(&[run("r1", W, Some(true))], None, Want::Continue, None);
        assert_eq!(p.session, Session::Focus { run: "r1".into() });
    }

    #[test]
    fn a_live_session_we_did_not_start_is_forked_not_resumed() {
        // `claude --resume` on a running session forks or refuses; ask for
        // the one outcome that holds whatever the old session is doing.
        let p = plan(&[run("r1", W, None)], None, Want::Continue, None);
        assert!(matches!(p.session, Session::Fork { .. }));
    }

    #[test]
    fn a_live_session_we_did_not_start_is_focused_when_its_terminal_is_known() {
        let mut r = run("r1", W, None);
        r.tty = Some("/dev/pts/3".into());
        assert_eq!(
            plan(&[r.clone()], None, Want::Continue, None).session,
            Session::Focus { run: "r1".into() }
        );
        assert!(matches!(plan(&[r.clone()], None, Want::Fork, None).session, Session::Fork { .. }));
        // Once it has ended, its terminal says nothing about where it is.
        r.status = X;
        assert!(matches!(plan(&[r], None, Want::Continue, None).session, Session::Resume { .. }));
    }

    #[test]
    fn a_live_session_behind_a_launch_that_died_is_focused() {
        let mut died = run("r2", None, Some(false));
        died.transcript_exists = false;
        let mut live = run("r1", W, None);
        live.tty = Some("/dev/pts/3".into());
        live.process_known = true;
        let p = plan(&[died, live], None, Want::Continue, None);
        assert_eq!(p.session, Session::Focus { run: "r1".into() });
    }

    #[test]
    fn an_older_session_known_only_by_its_events_is_not_focused() {
        // From before varde recorded processes: its terminal closed with
        // no end, and a newer session has run and ended since.
        let mut old = run("r1", W, None);
        old.tty = Some("/dev/pts/3".into());
        let p = plan(&[run("r2", X, Some(false)), old], None, Want::Continue, None);
        assert_eq!(p.session, Session::Resume { run: "r2".into(), cwd: "/p/.worktrees/x".into() });
    }

    #[test]
    fn a_finished_session_is_resumed_where_it_ran() {
        let p = plan(&[run("r1", X, Some(false))], None, Want::Continue, None);
        assert_eq!(p.session, Session::Resume { run: "r1".into(), cwd: "/p/.worktrees/x".into() });
        assert_eq!(p.tree, Tree::Use("/p/.worktrees/x".into()));
    }

    #[test]
    fn a_dead_process_is_not_live_whatever_its_last_event_said() {
        // The terminal closed without SessionEnd.
        let p = plan(&[run("r1", W, Some(false))], None, Want::Continue, None);
        assert!(matches!(p.session, Session::Resume { .. }));
    }

    #[test]
    fn a_run_that_never_started_falls_back_to_the_one_before() {
        let mut failed = run("r2", None, Some(false));
        failed.transcript_exists = false;
        let p = plan(&[failed, run("r1", X, Some(false))], None, Want::Continue, None);
        assert!(matches!(p.session, Session::Resume { ref run, .. } if run == "r1"));
    }

    #[test]
    fn a_removed_worktree_is_restored_to_resume_in_it() {
        let mut r = run("r1", X, Some(false));
        r.cwd_exists = false;
        let p = plan(&[r], Some(&wt(false, false)), Want::Continue, None);
        assert!(matches!(p.session, Session::Resume { .. }));
        assert_eq!(p.tree, Tree::Restore { path: "/p/.worktrees/x".into(), branch: "varde/x".into() });
    }

    #[test]
    fn a_removed_worktree_is_restored_for_a_run_in_its_subdirectory() {
        let mut r = run("r1", X, Some(false));
        r.cwd_exists = false;
        r.cwd = Some("/p/.worktrees/x/src".into());
        let p = plan(&[r], Some(&wt(false, false)), Want::Continue, None);
        assert!(matches!(p.session, Session::Resume { .. }));
        assert_eq!(p.tree, Tree::Restore { path: "/p/.worktrees/x".into(), branch: "varde/x".into() });
        // A sibling whose name only starts the same is not inside it.
        let mut r = run("r1", X, Some(false));
        r.cwd_exists = false;
        r.cwd = Some("/p/.worktrees/xy".into());
        assert_eq!(plan(&[r], Some(&wt(false, false)), Want::Continue, Some(false)).tree, Tree::Root);
    }

    #[test]
    fn a_vanished_directory_that_was_not_a_worktree_means_fresh() {
        let mut r = run("r1", X, Some(false));
        r.cwd_exists = false;
        r.cwd = Some("/gone".into());
        let p = plan(&[r], None, Want::Continue, Some(false));
        assert_eq!(p, Plan { session: Session::Fresh, tree: Tree::Root });
    }

    #[test]
    fn fresh_reuses_the_tasks_worktree() {
        let p = plan(&[run("r1", X, Some(false))], Some(&wt(true, true)), Want::Fresh, None);
        assert_eq!(p, Plan { session: Session::Fresh, tree: Tree::Use("/p/.worktrees/x".into()) });
        let p = plan(&[], Some(&wt(true, false)), Want::Fresh, None);
        assert!(matches!(p.tree, Tree::Restore { .. }));
    }

    #[test]
    fn the_worktree_question_is_asked_once_per_task() {
        let p = plan(&[run("r1", X, Some(false))], None, Want::Fresh, None);
        assert_eq!(p.tree, Tree::Root);
        // Merged: the task's worktree is done with, so ask again.
        assert_eq!(
            plan(&[run("r1", X, Some(false))], Some(&wt(false, false)), Want::Fresh, None).tree,
            Tree::Ask
        );
    }

    #[test]
    fn a_merged_worktree_is_not_reused() {
        let p = plan(&[], Some(&wt(false, false)), Want::Fresh, Some(false));
        assert_eq!(p.tree, Tree::Root);
    }

    #[test]
    fn fork_branches_even_from_our_own_live_session() {
        let p = plan(&[run("r1", W, Some(true))], None, Want::Fork, None);
        assert!(matches!(p.session, Session::Fork { .. }));
    }

    #[test]
    fn a_launch_not_yet_heard_from_is_focused_not_resumed_again() {
        let mut starting = run("r2", None, Some(true));
        starting.transcript_exists = false;
        let p = plan(&[starting, run("r1", X, Some(false))], None, Want::Continue, None);
        assert_eq!(p.session, Session::Focus { run: "r2".into() });
    }

    #[test]
    fn no_transcript_anywhere_means_fresh() {
        let mut r = run("r1", X, Some(false));
        r.transcript_exists = false;
        assert_eq!(plan(&[r], None, Want::Continue, Some(false)).session, Session::Fresh);
    }
}
