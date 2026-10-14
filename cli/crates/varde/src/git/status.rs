//! Where a checkout's work stands against where it has to end up: the
//! board's git column and `varde show`, with a PR's state from `gh`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};

use super::git;

/// Where a checkout's work stands against where it has to end up. Worked
/// out when asked, never stored.
pub struct Facts {
    pub branch: String,
    /// The base branch for a worktree; the upstream for a shared checkout.
    pub target: Option<String>,
    /// A shared checkout: the project directory, not the task's own tree.
    pub shared: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Paths `git status` lists: changes and untracked files.
    pub changes: u32,
    pub upstream: Option<String>,
    /// Commits the upstream does not have; `None` when there is none.
    pub unpushed: Option<u32>,
    /// The branch's own commits all reached the target.
    pub landed: bool,
    pub pr: Option<Pr>,
}

pub struct Pr {
    pub number: u64,
    /// `open`, `merged` or `closed`.
    pub state: String,
    pub draft: bool,
    /// `passing`, `failing` or `pending`; `None` when there are no checks.
    pub checks: Option<&'static str>,
    pub url: String,
    /// The commit the PR last had.
    pub head: String,
}

impl Facts {
    /// The one word for where the work is, earliest unfinished step first.
    pub fn stage(&self) -> &'static str {
        if self.changes > 0 {
            "edited"
        } else if self.landed {
            "landed"
        } else if self.pr.as_ref().is_some_and(|p| p.state == "open") {
            "pr"
        } else if self.ahead > 0 && self.unpushed == Some(0) && !self.shared {
            "pushed"
        } else if self.ahead > 0 {
            "committed"
        } else {
            "clean"
        }
    }

    /// What the board shows: the stage, or the PR and its checks. Nothing
    /// when there is nothing to do.
    pub fn label(&self) -> String {
        let pr = |p: &Pr, what: &str| format!("PR #{}{what}", p.number);
        match (self.stage(), &self.pr) {
            ("clean", _) => String::new(),
            ("pr", Some(p)) => pr(
                p,
                match p.checks {
                    Some("passing") => " ✓",
                    Some("failing") => " ✗",
                    Some(_) => " …",
                    None => "",
                },
            ),
            ("committed" | "pushed", Some(p)) if p.state == "closed" => pr(p, " closed"),
            (stage, _) => stage.to_string(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "branch": self.branch, "target": self.target, "shared": self.shared,
            "stage": self.stage(), "ahead": self.ahead, "behind": self.behind,
            "changes": self.changes, "upstream": self.upstream, "unpushed": self.unpushed,
            "landed": self.landed,
            "pr": self.pr.as_ref().map(|p| json!({"number": p.number, "state": p.state, "draft": p.draft,
                "checks": p.checks, "url": p.url, "head": p.head})),
        })
    }
}

/// The facts for `checkout`: a task's worktree against `base`, or, with no
/// base, a shared checkout against its upstream. With `with_pr`, a pushed
/// branch's PR is looked up through `gh`, which goes over the network.
pub fn facts(checkout: &Path, base: Option<&str>, with_pr: bool) -> Result<Facts> {
    // No optional locks: a plain status refreshes the index as it reads.
    let status = git(
        checkout,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            super::STATUS_ALL[0],
            super::STATUS_ALL[1],
        ],
    )?;
    let (mut branch, mut upstream, mut unpushed, mut changes) = (String::new(), None, None, 0);
    for line in status.lines() {
        if let Some(b) = line.strip_prefix("# branch.head ") {
            branch = b.to_string();
        } else if let Some(u) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(u.to_string());
        } else if let Some(ab) = line.strip_prefix("# branch.ab +") {
            unpushed = ab.split(' ').next().and_then(|a| a.parse().ok());
        } else if !line.starts_with('#') {
            changes += 1;
        }
    }
    // A task's branch tracks its local base (`git::restore`): that says
    // nothing about where the work is pushed.
    if upstream.is_some() && local_upstream(checkout, &branch) {
        (upstream, unpushed) = (None, None);
    }
    let target = base.map(str::to_string).or_else(|| upstream.clone());
    let (behind, ahead) = match &target {
        Some(t) => {
            let counts = git(checkout, &["rev-list", "--left-right", "--count", &format!("{t}...HEAD")])?;
            let mut n = counts.split_whitespace().map(|n| n.parse::<u32>().unwrap_or(0));
            (n.next().unwrap_or(0), n.next().unwrap_or(0))
        }
        None => (0, 0),
    };
    let default = || base.map(str::to_string).or_else(|| super::base(checkout).ok());
    let pr =
        if with_pr && upstream.is_some() && branch != "(detached)" && default().as_deref() != Some(&branch) {
            pr(checkout, &branch)
        } else {
            None
        };
    let tip = git(checkout, &["rev-parse", "HEAD"]).unwrap_or_default();
    let landed = base.is_some()
        && if pr.as_ref().is_some_and(|p| p.state == "merged" && p.head == tip) {
            true
        } else {
            ahead == 0 && has_own_commits(checkout, &branch, &tip)
        };
    Ok(Facts {
        branch,
        target,
        shared: base.is_none(),
        ahead,
        behind,
        changes,
        upstream,
        unpushed,
        landed,
        pr,
    })
}

/// Whether `branch` tracks a branch of its own repository, not a remote's.
fn local_upstream(dir: &Path, branch: &str) -> bool {
    git(dir, &["config", "--get", &format!("branch.{branch}.remote")]).is_ok_and(|r| r == ".")
}

/// Whether `branch` moved on from where it was cut. Its reflog's oldest
/// entry is its creation; without a reflog nothing can be claimed.
fn has_own_commits(dir: &Path, branch: &str, tip: &str) -> bool {
    let log =
        git(dir, &["reflog", "show", "--format=%H", &format!("refs/heads/{branch}")]).unwrap_or_default();
    log.lines().last().is_some_and(|created| created != tip)
}

/// `branch`'s PR, if it has an upstream and `gh` can say. Any failure (no
/// `gh`, not GitHub, no PR, no network) is simply no PR.
fn pr(dir: &Path, branch: &str) -> Option<Pr> {
    let out =
        gh(dir, &["pr", "view", branch, "--json", "number,state,isDraft,headRefOid,url,statusCheckRollup"])?;
    let v: Value = serde_json::from_str(&out).ok()?;
    Some(Pr {
        number: v["number"].as_u64()?,
        state: v["state"].as_str()?.to_ascii_lowercase(),
        draft: v["isDraft"] == true,
        checks: checks(v["statusCheckRollup"].as_array().map_or(&[][..], Vec::as_slice)),
        url: v["url"].as_str().unwrap_or_default().to_string(),
        head: v["headRefOid"].as_str().unwrap_or_default().to_string(),
    })
}

/// A PR merged with exactly what `branch` holds: its work is in the base
/// even when the merge squashed or rebased it, which git alone cannot see.
pub fn merged_pr(repo: &Path, branch: &str) -> Option<Pr> {
    // Its configured upstream, not the ref: GitHub deletes a merged PR's
    // branch, and a prune then takes the ref while the PR stays merged.
    git(repo, &["config", "--get", &format!("branch.{branch}.merge")]).ok()?;
    if local_upstream(repo, branch) {
        return None;
    }
    let tip = git(repo, &["rev-parse", branch]).ok()?;
    pr(repo, branch).filter(|p| p.state == "merged" && p.head == tip)
}

/// One word for a PR's checks: any failure fails them, then anything
/// unfinished leaves them pending.
fn checks(rollup: &[Value]) -> Option<&'static str> {
    if rollup.is_empty() {
        return None;
    }
    let mut pending = false;
    for c in rollup {
        // A check run has a conclusion once complete; a status has a state.
        let result = c["conclusion"].as_str().filter(|s| !s.is_empty()).or(c["state"].as_str());
        match result.unwrap_or("") {
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                return Some("failing")
            }
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            _ => pending = true,
        }
    }
    Some(if pending { "pending" } else { "passing" })
}

const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `gh` (or `$VARDE_GH`), giving up after `GH_TIMEOUT`.
fn gh(dir: &Path, args: &[&str]) -> Option<String> {
    let bin = std::env::var_os("VARDE_GH").unwrap_or_else(|| "gh".into());
    let mut child = Command::new(bin)
        .current_dir(dir)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read as it comes, so a large reply cannot fill the pipe and stall it.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        stdout.read_to_string(&mut s).map(|_| s)
    });
    let deadline = Instant::now() + GH_TIMEOUT;
    loop {
        match child.try_wait().ok()? {
            Some(status) => return status.success().then(|| reader.join().ok()?.ok()).flatten(),
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::create;
    use super::super::tests::{commit, layout, repo};
    use super::*;

    #[test]
    fn facts_follow_the_work_from_edit_to_landed() {
        let r = repo();
        let new = create(r.path(), "f-abc123", &layout(r.path()), None).unwrap();
        let stage = || facts(&new.path, Some("main"), false).unwrap();
        // A fresh branch is not landed: it has nothing of its own.
        assert_eq!(stage().stage(), "clean");
        std::fs::write(new.path.join("a.txt"), "a").unwrap();
        assert_eq!(stage().changes, 1);
        assert_eq!(stage().stage(), "edited");
        commit(&new.path, "a.txt", "a");
        let f = stage();
        assert_eq!((f.stage(), f.ahead, f.behind, f.unpushed), ("committed", 1, 0, None));
        commit(r.path(), "m.txt", "m");
        assert_eq!(stage().behind, 1);
        git(r.path(), &["merge", "-q", "--no-edit", &new.branch]).unwrap();
        let f = stage();
        assert!(f.landed);
        assert_eq!((f.stage(), f.ahead), ("landed", 0));
        // Reading the status leaves the index as it was.
        assert_eq!(git(&new.path, &["status", "--porcelain"]).unwrap(), "");
    }

    #[test]
    fn a_shared_checkout_is_measured_against_its_upstream() {
        let r = repo();
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q", "--bare"]).unwrap();
        git(r.path(), &["remote", "add", "origin", &origin.path().to_string_lossy()]).unwrap();
        git(r.path(), &["push", "-q", "-u", "origin", "main"]).unwrap();
        commit(r.path(), "a.txt", "a");
        let f = facts(r.path(), None, true).unwrap();
        assert!(f.shared && !f.landed && f.pr.is_none());
        assert_eq!(f.target.as_deref(), Some("origin/main"));
        assert_eq!((f.stage(), f.ahead, f.unpushed), ("committed", 1, Some(1)));
        git(r.path(), &["push", "-q"]).unwrap();
        assert_eq!(facts(r.path(), None, false).unwrap().stage(), "clean");
    }

    #[test]
    fn checks_fail_first_then_wait() {
        let run = |conclusion: &str, status: &str| json!({"conclusion": conclusion, "status": status});
        let ctx = |state: &str| json!({"state": state});
        assert_eq!(checks(&[]), None);
        assert_eq!(checks(&[run("SUCCESS", "COMPLETED"), ctx("SUCCESS")]), Some("passing"));
        assert_eq!(checks(&[run("", "IN_PROGRESS"), ctx("SUCCESS")]), Some("pending"));
        assert_eq!(checks(&[ctx("PENDING"), run("FAILURE", "COMPLETED")]), Some("failing"));
        assert_eq!(checks(&[run("SKIPPED", "COMPLETED"), run("NEUTRAL", "COMPLETED")]), Some("passing"));
    }
}
