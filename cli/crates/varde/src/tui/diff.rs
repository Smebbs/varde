//! `=`: the task's changes, in git's pager.

use anyhow::{Context, Result};
use varde_core::task;

use super::draw::tilde;
use super::term::any_key;
use super::{App, Away, BUSY};
use crate::process;

/// The changes `=` shows: `git diff` in `dir`, from `from`.
pub(super) struct Diff {
    pub(super) dir: String,
    pub(super) from: Option<String>,
    /// Without a worktree, the project's checkout, which other tasks share.
    pub(super) shared: bool,
    /// An index of its own, in which the untracked files are marked to be
    /// added, so the diff shows them as new files; the real index is never
    /// touched. `None` diffs against the real one.
    pub(super) index: Option<tempfile::TempDir>,
}

impl Diff {
    pub(super) fn git(&self) -> std::process::Command {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(&self.dir);
        if let Some(i) = &self.index {
            c.env("GIT_INDEX_FILE", i.path().join("index"));
        }
        c
    }

    /// Take a copy of the index, and mark the untracked files in it.
    fn with_untracked(&mut self) -> Result<()> {
        let out = self.git().args(["rev-parse", "--path-format=absolute", "--git-path", "index"]).output()?;
        let real = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let scratch = tempfile::tempdir()?;
        if std::path::Path::new(&real).is_file() {
            std::fs::copy(&real, scratch.path().join("index"))?;
        }
        self.index = Some(scratch);
        let out = self.git().args(["add", "--intent-to-add", "--", "."]).output()?;
        if !out.status.success() {
            anyhow::bail!("git add --intent-to-add: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    /// What is printed under the diff, which stays until a key.
    pub(super) fn footer(&self) -> String {
        let back = "Any key returns to the board.";
        if self.shared {
            format!("That is uncommitted in {}, which the project's tasks share.\n{back}", tilde(&self.dir))
        } else {
            back.to_string()
        }
    }
}

impl App<'_> {
    /// What `=` shows of the task's changes: its worktree against where it
    /// branched off, or else what is uncommitted in its project's
    /// checkout, which other tasks share. `None`, and a message, when
    /// there is nothing to show.
    pub(super) fn changes(&mut self, id: &str) -> Option<Away> {
        let result = (|| -> Result<(bool, Diff)> {
            let d = task::detail(&self.paths.open(BUSY)?, id, &process::alive)?;
            let (dir, base) = match d.worktree {
                Some(w) if w.state == "active" => (w.path, Some(w.base)),
                _ => (d.project.context("the task has no project to diff")?, None),
            };
            let mut diff = Diff { from: None, shared: base.is_none(), dir, index: None };
            diff.with_untracked()?;
            if let Some(b) = base {
                let out = diff.git().args(["merge-base", &b, "HEAD"]).output()?;
                diff.from =
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty());
            }
            let quiet = diff
                .git()
                .args(["diff", "--quiet"])
                .args(&diff.from)
                .output()
                .context("running git diff")?
                .status;
            Ok((!quiet.success(), diff))
        })();
        match result {
            Ok((true, diff)) => Some(Away::Diff(diff)),
            Ok((false, diff)) => {
                self.message = if diff.shared {
                    format!("nothing uncommitted in {}, shared by the project's tasks", tilde(&diff.dir))
                } else {
                    format!("no changes in {}", tilde(&diff.dir))
                };
                None
            }
            Err(e) => {
                self.message = format!("error: {e:#}");
                None
            }
        }
    }

    /// The task's changes, in git's pager, left on screen until a key.
    /// git's pager quits at once on a diff that fits the screen (`LESS=F`),
    /// so without the wait the board would be drawn straight over it.
    pub(super) fn diff(&self, diff: &Diff) -> Result<String> {
        diff.git().arg("diff").args(&diff.from).status().context("running git diff")?;
        println!("\n{}", diff.footer());
        any_key();
        Ok(if diff.shared {
            format!("{} is shared by the project's tasks", tilde(&diff.dir))
        } else {
            String::new()
        })
    }
}
