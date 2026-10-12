//! Worktrees, by shelling out to `git`: it matches whatever the user has
//! installed, and `git worktree` is the reference implementation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

mod status;

pub use status::{facts, merged_pr, Facts, Pr};

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_env(dir, args, &[])
}

/// `git -C dir args`, which never prompts.
pub fn command(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0");
    c
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<String> {
    let out = command(dir, args).envs(env.iter().copied()).output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn ok(dir: &Path, args: &[&str]) -> bool {
    git(dir, args).is_ok()
}

/// The branch work is merged back into: the remote's default, else a
/// conventional name, else whatever is checked out. Always a local
/// branch: given a name that only exists on the remote, `worktree add`
/// would create and check out that branch instead of the task's.
pub fn base(repo: &Path) -> Result<String> {
    if !has_commits(repo) {
        bail!(
            "{} has no commits yet, and a worktree starts from one: commit first, or pass --worktree never",
            repo.display()
        );
    }
    if let Ok(r) = git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = r.strip_prefix("origin/").filter(|b| has_branch(repo, b)) {
            return Ok(b.to_string());
        }
    }
    for b in ["main", "master", "trunk"] {
        if has_branch(repo, b) {
            return Ok(b.to_string());
        }
    }
    let head = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if head == "HEAD" {
        bail!("{} has no branch checked out to base a worktree on", repo.display());
    }
    Ok(head)
}

/// The git directory of the checkout at `dir`, and the one its
/// repository shares; they differ in a linked worktree.
fn git_dirs(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = git(dir, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"]).ok()?;
    let (own, common) = out.split_once('\n')?;
    Some((PathBuf::from(own), PathBuf::from(common)))
}

/// The main checkout of the repository `dir` is the top of: `dir` itself,
/// unless it is a linked worktree (`git worktree add`), so that a task's
/// worktree goes beside the others rather than nested inside yours. A
/// bare repository has no main checkout, and keeps `dir`.
pub fn main_checkout(dir: &Path) -> PathBuf {
    git_dirs(dir)
        .filter(|(own, common)| own != common && common.file_name().is_some_and(|n| n == ".git"))
        .and_then(|(_, common)| common.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| dir.to_path_buf())
}

/// The branch checked out in `dir` when it is a linked worktree, which
/// its tasks' worktrees start from and merge back into.
fn linked_branch(dir: &Path) -> Option<String> {
    git_dirs(dir).filter(|(own, common)| own != common)?;
    git(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).ok().filter(|b| !b.is_empty())
}

/// A branch- and path-safe name: the title's first five words, then the
/// task's short id, which makes it unique without any collision logic.
pub fn slug(title: &str, short: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .map(str::to_ascii_lowercase)
        .collect();
    let mut s = words.join("-");
    s.truncate(32);
    let s = s.trim_end_matches('-');
    if s.is_empty() {
        short.to_string()
    } else {
        format!("{s}-{short}")
    }
}

/// The commit `rev` names, if it names one.
pub fn rev(dir: &Path, rev: &str) -> Option<String> {
    git(dir, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).ok()
}

pub fn has_branch(repo: &Path, branch: &str) -> bool {
    rev(repo, &format!("refs/heads/{branch}")).is_some()
}

/// Whether the checkout at `dir` has a commit checked out.
pub fn has_commits(dir: &Path) -> bool {
    rev(dir, "HEAD").is_some()
}

pub struct New {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
    /// Whether it was made now, rather than found already there.
    pub made: bool,
    /// What the user should hear about, from `restore`.
    pub notices: Vec<String>,
}

/// Where a task's worktree goes and what its branch is called: the
/// user's settings, else `<repo>/.worktrees/<slug>` on `varde/<slug>`.
pub struct Layout {
    pub dir: PathBuf,
    pub prefix: String,
    /// The repository's own choice of base, from `git config varde.base`.
    pub base: Option<String>,
}

pub fn create(repo: &Path, slug: &str, layout: &Layout) -> Result<New> {
    // A worktree of your own is where you work on its branch, so its
    // tasks start from that work and merge back into it.
    let base = match (linked_branch(repo), layout.base.as_deref()) {
        (Some(b), _) => b,
        (None, Some(b)) if has_branch(repo, b) => b.to_string(),
        (None, Some(b)) => bail!("git config varde.base names {b}, which is not a local branch"),
        (None, None) => base(repo)?,
    };
    let path = layout.dir.join(slug);
    let branch = format!("{}{slug}", layout.prefix);
    exclude_worktrees(&main_checkout(repo), &layout.dir)?;
    // The branch outlives a removed worktree when it holds unmerged work;
    // a fresh start picks that work back up rather than failing.
    let made = restore(repo, &path, &branch, &base)?;
    Ok(New { path, branch, base, made: made.is_some(), notices: made.unwrap_or_default() })
}

/// Where the work of `branch` is kept when its task is dropped: under its
/// slug, whatever prefix the branch was given.
pub fn discarded_ref(branch: &str) -> String {
    format!("refs/varde/discarded/{}", branch.rsplit('/').next().unwrap_or(branch))
}

/// The subject of a commit `save` makes.
const KEPT: &str = "varde: discarded work";

/// Bring the task's worktree back at `path`, or make it. A worktree still
/// there is reused, so a start that failed half-way can simply be run
/// again. Otherwise the branch is checked out; failing that, the work a
/// drop kept is brought back; failing that, the branch is cut from
/// `base`. Returns what the user should hear about, or `None` when the
/// worktree was there already.
pub fn restore(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<Option<Vec<String>>> {
    let _ = git(repo, &["worktree", "prune"]);
    if path.is_dir() && registered(repo, path)? {
        return Ok(None);
    }
    let p = path.to_string_lossy();
    let kept = discarded_ref(branch);
    let mut notices = vec![];
    if has_branch(repo, branch) {
        git(repo, &["worktree", "add", &p, branch])?;
    } else if let Some(k) = rev(repo, &kept) {
        // A commit `save` made holds the worktree's files on top of the
        // branch: the branch comes back as it was, and the files as they were.
        let made = git(repo, &["log", "-1", "--format=%s", &k])?.starts_with(KEPT);
        let start = if made { format!("{k}^") } else { k.clone() };
        git(repo, &["worktree", "add", "-b", branch, &p, &start])?;
        if made {
            git(path, &["restore", "--source", &k, "--worktree", "--", "."])?;
        }
        // Back where it was, so a later start does not bring it back again;
        // kept when it also holds an earlier drop or another branch.
        if git(repo, &["rev-list", "--parents", "-n", "1", &k])?.split(' ').count() <= 2 {
            git(repo, &["update-ref", "-d", &kept, &k])?;
        }
        notices.push(format!("brought back the work dropped from {branch}"));
    } else {
        git(repo, &["worktree", "add", "-b", branch, &p, base])?;
    }
    notices.extend(setup(repo, path));
    Ok(Some(notices))
}

/// The file in a worktree's own git directory that holds the setup
/// command its next `varde run` runs first. Kept there, it is never
/// committed, and goes with the worktree.
const SETUP_FILE: &str = "varde-setup";

/// Leave `command` for the next `varde run` in the new worktree.
pub fn mark_setup(worktree: &Path, command: &str) -> Result<()> {
    let gitdir = git(worktree, &["rev-parse", "--absolute-git-dir"])?;
    Ok(std::fs::write(Path::new(&gitdir).join(SETUP_FILE), command)?)
}

/// The setup command left for the worktree `dir` is in, taken so it runs
/// once, and the worktree's root, where it runs: `dir` may be below it.
pub fn take_setup(dir: &Path) -> Option<(String, PathBuf)> {
    let out = git(dir, &["rev-parse", "--absolute-git-dir", "--show-toplevel"]).ok()?;
    let (gitdir, root) = out.split_once('\n')?;
    let file = Path::new(gitdir).join(SETUP_FILE);
    let command = std::fs::read_to_string(&file).ok()?;
    std::fs::remove_file(&file).ok()?;
    Some((command, PathBuf::from(root))).filter(|(c, _)| !c.trim().is_empty())
}

/// Whether `path` is one of `repo`'s worktrees.
fn registered(repo: &Path, path: &Path) -> Result<bool> {
    let want = path.canonicalize()?;
    let list = git(repo, &["worktree", "list", "--porcelain"])?;
    Ok(list.lines().filter_map(|l| l.strip_prefix("worktree ")).any(|p| Path::new(p) == want))
}

/// Make a new worktree ready to work in: its submodules checked out, and
/// the ignored files `.worktreeinclude` names copied from the main
/// checkout. A failure is reported, not fatal.
fn setup(repo: &Path, path: &Path) -> Vec<String> {
    let mut notices = vec![];
    if path.join(".gitmodules").is_file() {
        if let Err(e) = git(path, &["submodule", "update", "--init", "--recursive"]) {
            notices.push(format!("submodules not checked out: {e:#}"));
        }
    }
    let copy = || -> Result<()> {
        let root = path.canonicalize()?;
        for rel in included(repo, repo, path)? {
            let to = path.join(&rel);
            // Nothing there yet, not even a link: a committed symlink must
            // not carry your files out of the worktree.
            if to.symlink_metadata().is_err() {
                let dir = to.parent().unwrap_or(path);
                std::fs::create_dir_all(dir)?;
                if !dir.canonicalize()?.starts_with(&root) {
                    bail!("{rel} would be copied outside the worktree");
                }
                std::fs::copy(repo.join(&rel), &to).with_context(|| format!("copying {rel}"))?;
            }
        }
        Ok(())
    };
    if let Err(e) = copy() {
        notices.push(format!(".worktreeinclude files not all copied: {e:#}"));
    }
    notices
}

/// The files in checkout `dir` that `repo`'s `.worktreeinclude` names and
/// git ignores, as Claude Code reads that file: a file git would track
/// comes with the checkout, or not at all. Paths are relative to `dir`;
/// the worktrees beside `worktree`, when kept inside the repository, are
/// never among them.
fn included(repo: &Path, dir: &Path, worktree: &Path) -> Result<Vec<String>> {
    let list = repo.join(".worktreeinclude");
    if !list.is_file() {
        return Ok(vec![]);
    }
    let from = format!("--exclude-from={}", list.display());
    let named = git(dir, &["ls-files", "-z", "--others", "--ignored", &from])?;
    let trees =
        worktree.parent().and_then(|d| d.strip_prefix(repo).ok()).map(|d| format!("{}/", d.display()));
    let named: Vec<&str> = named
        .split('\0')
        .filter(|p| !p.is_empty() && !trees.as_deref().is_some_and(|t| t != "/" && p.starts_with(t)))
        .collect();
    if named.is_empty() {
        return Ok(vec![]);
    }
    let out = git_input(dir, &["check-ignore", "-z", "--stdin"], &named.join("\0"))?;
    // 1 is "nothing ignored".
    if !out.status.success() && out.status.code() != Some(1) {
        bail!("git check-ignore: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
}

/// Files `.worktreeinclude` names that the worktree at `path` has changed
/// from the main checkout's copy, or has on its own. Removing the
/// worktree would lose them.
pub fn included_changes(repo: &Path, path: &Path) -> Result<Vec<String>> {
    let read = |p: PathBuf| std::fs::read(p).ok();
    Ok(included(repo, path, path)?
        .into_iter()
        .filter(|rel| read(path.join(rel)) != read(repo.join(rel)))
        .collect())
}

/// `git` with `input` on its stdin; the caller judges the exit status.
fn git_input(dir: &Path, args: &[&str], input: &str) -> Result<std::process::Output> {
    let mut child = command(dir, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git")?;
    let mut stdin = child.stdin.take().context("git's stdin")?;
    let input = input.to_string();
    // Written from a thread, so a large answer cannot stall the writing.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok(out)
}

/// Anything `git worktree remove --force` would lose: changes, untracked
/// files, and edits inside submodules. Ignored files are not counted; git
/// treats them as disposable too. The user's `status.showUntrackedFiles`
/// and `diff.ignoreSubmodules` are overridden: either would hide work.
pub fn dirty(path: &Path) -> Result<bool> {
    Ok(!git(path, &["status", "--porcelain", STATUS_ALL[0], STATUS_ALL[1]])?.is_empty())
}

/// The checkout to run a task's worktree commands from: `repo`, where the
/// task was made, unless that was a linked worktree of the user's that is
/// gone (Q24); then the main checkout of the repository `worktree` is in.
pub fn home(repo: &Path, worktree: &Path) -> PathBuf {
    if repo.join(".git").exists() {
        return repo.to_path_buf();
    }
    Some(main_checkout(worktree))
        .filter(|main| main != worktree && main.join(".git").exists())
        .unwrap_or_else(|| repo.to_path_buf())
}

/// What `git status` must list whatever the user's config says.
pub const STATUS_ALL: [&str; 2] = ["--untracked-files=all", "--ignore-submodules=none"];

/// Work in the worktree at `path` that removing it would destroy and no
/// ref keeps, so neither a branch nor `save` can: commits on a detached
/// `HEAD` no branch has, a submodule's commit nothing outside the worktree
/// has (its repository lives inside the worktree's git dir), and an
/// untracked repository inside the tree, which a snapshot records only as
/// a link.
pub fn stranded(path: &Path) -> Result<Vec<String>> {
    let mut lost = vec![];
    if rev(path, "HEAD").is_some()
        && git(
            path,
            &["for-each-ref", "--count=1", "--contains", "HEAD", "refs/heads", "refs/remotes", "refs/tags"],
        )?
        .is_empty()
    {
        lost.push("commits on a detached HEAD that no branch has".to_string());
    }
    if path.join(".gitmodules").exists() {
        // Every checked-out submodule, nested ones too, relative to `path`.
        let subs = git(
            path,
            &["submodule", "foreach", "--quiet", "--recursive", r#"printf '%s\0' "$displaypath""#],
        )?;
        for sub in subs.split('\0').filter(|s| !s.is_empty()) {
            if !submodule_kept(path, sub)? {
                lost.push(format!("commits in submodule {sub} that its remote does not have"));
            }
        }
    }
    for line in git(path, &["status", "--porcelain", STATUS_ALL[0], STATUS_ALL[1]])?.lines() {
        // With every untracked file listed, only a repository shows as a directory.
        if let Some(dir) = line.strip_prefix("?? ").filter(|d| d.ends_with('/')) {
            lost.push(format!("a git repository at {dir} that is not a submodule"));
        }
    }
    Ok(lost)
}

/// Whether the commit checked out in the worktree `path`'s submodule `sub`
/// is kept once the worktree goes: a remote-tracking ref or tag of the
/// submodule has it, or it was fetched rather than made here, or the
/// main checkout's own repository for that submodule reaches it.
fn submodule_kept(path: &Path, sub: &str) -> Result<bool> {
    let dir = path.join(sub);
    let Some(head) = rev(&dir, "HEAD") else { return Ok(true) };
    if !git(&dir, &["for-each-ref", "--count=1", "--contains", &head, "refs/remotes", "refs/tags"])?
        .is_empty()
    {
        return Ok(true);
    }
    // A shallow clone fetches what the superproject pins, which no ref may
    // reach: it lists it among its shallow commits, or, fetched by its id,
    // in FETCH_HEAD. Either way it came from the remote. A commit made here
    // is in neither, whatever pins it (a superproject pushed without its
    // submodule pins work nothing else has). A later fetch forgets it, and
    // the worktree is then kept: the safe way to be wrong.
    let fetched = |name: &str| {
        git(&dir, &["rev-parse", "--path-format=absolute", "--git-path", name])
            .ok()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .is_some_and(|text| text.lines().any(|l| l.split_whitespace().next() == Some(head.as_str())))
    };
    if fetched("shallow") || fetched("FETCH_HEAD") {
        return Ok(true);
    }
    let Ok(module) = git(&dir, &["rev-parse", "--absolute-git-dir"]) else { return Ok(false) };
    // In a linked worktree, a submodule's repository is under the
    // worktree's git dir; the main checkout's is at the same place under
    // the common one, and outlives the worktree.
    let Some((own, common)) = git_dirs(path) else { return Ok(false) };
    let Ok(rel) = Path::new(&module).strip_prefix(&own) else { return Ok(false) };
    if own == common || rel.as_os_str().is_empty() {
        return Ok(false);
    }
    let main = common.join(rel);
    let main = main.to_string_lossy();
    // Reached, not merely present: an object only fetched is pruned in time.
    Ok(Path::new(main.as_ref()).is_dir()
        && (ok(Path::new("/"), &["--git-dir", &main, "merge-base", "--is-ancestor", &head, "HEAD"])
            || !git(Path::new("/"), &["--git-dir", &main, "for-each-ref", "--count=1", "--contains", &head])
                .unwrap_or_default()
                .is_empty()))
}

/// Keep all the work of `branch` and its worktree at `dir` under
/// `refname`, losing nothing already there: the branch's commits, what the
/// worktree has checked out if that is something else, its changes and
/// untracked files, and the work an earlier drop kept under `refname`.
/// One commit holds them: its first parent is the branch and its tree the
/// worktree's files, so `restore` can put both back. Nothing is written
/// when one of them already holds it all. Neither the worktree nor its
/// index is touched. Returns the commit kept, `None` when there is none.
pub fn save(repo: &Path, dir: &Path, branch: &str, refname: &str, task: &str) -> Result<Option<String>> {
    let tip = rev(repo, &format!("refs/heads/{branch}"));
    let head = if dir.is_dir() { rev(dir, "HEAD") } else { None };
    let old = rev(repo, refname);
    let mut parents: Vec<String> = vec![];
    for c in [&tip, &head, &old].into_iter().flatten() {
        if !parents.iter().any(|p| ok(repo, &["merge-base", "--is-ancestor", c, p])) {
            parents.push(c.clone());
        }
    }
    let Some(first) = parents.first().cloned() else { return Ok(None) };
    let tree = match &head {
        Some(_) if dirty(dir)? => Some(snapshot(dir)?),
        _ => None,
    };
    let kept = if tree.is_none() && parents.len() == 1 {
        first
    } else {
        let tree = match tree {
            Some(t) => t,
            None => git(repo, &["rev-parse", &format!("{}^{{tree}}", head.as_ref().unwrap_or(&first))])?,
        };
        let message = format!("{KEPT} of task {task}");
        let mut args = vec!["commit-tree", &tree, "-m", &message];
        for p in &parents {
            args.extend(["-p", p]);
        }
        let who = Path::new("varde");
        let at = Path::new("varde@localhost");
        git_env(
            repo,
            &args,
            &[
                ("GIT_AUTHOR_NAME", who),
                ("GIT_AUTHOR_EMAIL", at),
                ("GIT_COMMITTER_NAME", who),
                ("GIT_COMMITTER_EMAIL", at),
            ],
        )?
    };
    if old.as_ref() != Some(&kept) {
        // Moved only from what was read, so nothing is overwritten unseen.
        git(repo, &["update-ref", refname, &kept, old.as_deref().unwrap_or("")])?;
    }
    Ok(Some(kept))
}

/// The tree of everything in the worktree at `dir`, changes and untracked
/// files included, written through a scratch index.
fn snapshot(dir: &Path) -> Result<String> {
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    let with_index = |args: &[&str]| git_env(dir, args, &[("GIT_INDEX_FILE", &index)]);
    with_index(&["read-tree", "HEAD"])?;
    with_index(&["add", "-A"])?;
    with_index(&["write-tree"])
}

/// Commits on `branch` that `base` does not have.
pub fn ahead(repo: &Path, base: &str, branch: &str) -> Result<u32> {
    Ok(git(repo, &["rev-list", "--count", &format!("{base}..{branch}")])?.parse()?)
}

/// Remove a worktree. `--force` is always passed, because git refuses any
/// tree that lists submodules without it; callers check `dirty` first.
pub fn remove(repo: &Path, path: &Path) -> Result<()> {
    if path.exists() {
        git(repo, &["worktree", "remove", "--force", &path.to_string_lossy()])?;
    } else {
        git(repo, &["worktree", "prune"])?;
    }
    Ok(())
}

/// Delete `branch` only if it is merged into `base`; unmerged work is kept.
pub fn delete_if_merged(repo: &Path, base: &str, branch: &str) -> Result<bool> {
    if ahead(repo, base, branch)? > 0 {
        return Ok(false);
    }
    git(repo, &["branch", "-D", branch])?;
    Ok(true)
}

/// Keep the worktrees' directory out of `git status` when it is inside
/// the repository, unless something already does.
fn exclude_worktrees(repo: &Path, dir: &Path) -> Result<()> {
    let Ok(inside) = dir.strip_prefix(repo) else { return Ok(()) };
    let inside = inside.to_string_lossy();
    if inside.is_empty()
        || inside.starts_with("..")
        || ok(repo, &["check-ignore", "-q", &format!("{inside}/x")])
    {
        return Ok(());
    }
    let common = git(repo, &["rev-parse", "--git-common-dir"])?;
    let exclude = repo.join(common).join("info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap_or(repo))?;
    let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("/{inside}/\n"));
    std::fs::write(&exclude, text)?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn layout(repo: &Path) -> Layout {
        Layout { dir: repo.join(".worktrees"), prefix: "varde/".into(), base: None }
    }

    /// A repository with one commit on `main`, and an identity to commit as
    /// that signs nothing, whatever the user's own git config says.
    pub fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "t"],
            &["config", "user.email", "t@t"],
            &["config", "commit.gpgsign", "false"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            git(d.path(), args).unwrap();
        }
        d
    }

    #[test]
    fn slugs_are_short_safe_and_unique() {
        assert_eq!(slug("Fix the flaky login test in CI!", "k3m9qz"), "fix-the-flaky-login-test-k3m9qz");
        assert_eq!(slug("¿¿??", "k3m9qz"), "k3m9qz");
    }

    #[test]
    fn a_worktree_is_created_ignored_and_restored() {
        let r = repo();
        let new = create(r.path(), "x-abc123", &layout(r.path())).unwrap();
        assert!(new.path.is_dir());
        assert_eq!(new.base, "main");
        assert_eq!(git(r.path(), &["status", "--porcelain"]).unwrap(), "");

        git(r.path(), &["worktree", "remove", &new.path.to_string_lossy()]).unwrap();
        restore(r.path(), &new.path, &new.branch, "main").unwrap();
        assert!(new.path.is_dir());

        // With the branch gone as well, it is cut again from base.
        git(r.path(), &["worktree", "remove", &new.path.to_string_lossy()]).unwrap();
        git(r.path(), &["branch", "-D", &new.branch]).unwrap();
        restore(r.path(), &new.path, &new.branch, "main").unwrap();
        assert!(new.path.is_dir());
    }

    pub fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]).unwrap();
        git(dir, &["commit", "-q", "-m", file]).unwrap();
    }

    #[test]
    fn the_base_is_always_a_local_branch() {
        let r = repo();
        // As in a clone whose default branch was renamed locally.
        git(r.path(), &["branch", "-m", "main", "dev"]).unwrap();
        git(r.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]).unwrap();
        git(r.path(), &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]).unwrap();
        assert_eq!(base(r.path()).unwrap(), "dev");
        let new = create(r.path(), "b-abc123", &layout(r.path())).unwrap();
        assert_eq!(git(&new.path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(), new.branch);
    }

    #[test]
    fn a_kept_branch_is_picked_up_by_a_new_worktree() {
        let r = repo();
        let first = create(r.path(), "k-abc123", &layout(r.path())).unwrap();
        commit(&first.path, "a.txt", "a");
        remove(r.path(), &first.path).unwrap();
        let again = create(r.path(), "k-abc123", &layout(r.path())).unwrap();
        assert!(again.path.join("a.txt").exists());
    }

    #[test]
    fn unmerged_branches_are_kept() {
        let r = repo();
        let new = create(r.path(), "k-abc123", &layout(r.path())).unwrap();
        commit(&new.path, "a.txt", "a");
        remove(r.path(), &new.path).unwrap();
        assert!(!delete_if_merged(r.path(), "main", &new.branch).unwrap());
        assert!(git(r.path(), &["rev-parse", "--verify", &new.branch]).is_ok());
    }

    /// Whether some ref other than a branch reaches `commit`.
    fn kept(repo: &Path, commit: &str) -> bool {
        !git(repo, &["for-each-ref", "--contains", commit, "refs/varde/"]).unwrap().is_empty()
    }

    /// What `varde drop` does to a worktree.
    fn drop_it(repo: &Path, new: &New) -> Option<String> {
        let saved = save(repo, &new.path, &new.branch, &discarded_ref(&new.branch), "abc123").unwrap();
        remove(repo, &new.path).unwrap();
        git(repo, &["branch", "-D", &new.branch]).unwrap();
        saved
    }

    #[test]
    fn a_drop_keeps_the_branch_when_the_worktree_has_another_checked_out() {
        let r = repo();
        let new = create(r.path(), "o-abc123", &layout(r.path())).unwrap();
        commit(&new.path, "a.txt", "real work");
        let work = git(&new.path, &["rev-parse", "HEAD"]).unwrap();
        git(&new.path, &["checkout", "-q", "-b", "elsewhere", "main"]).unwrap();
        commit(&new.path, "b.txt", "other");
        let other = git(&new.path, &["rev-parse", "HEAD"]).unwrap();
        drop_it(r.path(), &new);
        assert!(kept(r.path(), &work) && kept(r.path(), &other));
        // Starting again brings the branch back, and the files as they were.
        restore(r.path(), &new.path, &new.branch, "main").unwrap();
        assert_eq!(git(&new.path, &["rev-parse", "HEAD"]).unwrap(), work);
        assert!(new.path.join("b.txt").exists());
    }

    #[test]
    fn a_second_drop_keeps_the_first() {
        let r = repo();
        let new = create(r.path(), "s-abc123", &layout(r.path())).unwrap();
        commit(&new.path, "a.txt", "first");
        let first = git(&new.path, &["rev-parse", "HEAD"]).unwrap();
        drop_it(r.path(), &new);
        // Started again from scratch, as an older varde did.
        git(r.path(), &["worktree", "add", "-q", "-b", &new.branch, &new.path.to_string_lossy(), "main"])
            .unwrap();
        commit(&new.path, "b.txt", "second");
        let second = git(&new.path, &["rev-parse", "HEAD"]).unwrap();
        drop_it(r.path(), &new);
        assert!(kept(r.path(), &first) && kept(r.path(), &second));
    }

    #[test]
    fn a_dropped_worktree_comes_back_as_it_was() {
        let r = repo();
        let new = create(r.path(), "d-abc123", &layout(r.path())).unwrap();
        commit(&new.path, "a.txt", "committed");
        let tip = git(&new.path, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(new.path.join("a.txt"), "edited").unwrap();
        std::fs::write(new.path.join("new.txt"), "untracked").unwrap();
        drop_it(r.path(), &new);
        assert!(!new.path.exists());

        let notices = restore(r.path(), &new.path, &new.branch, "main").unwrap().unwrap();
        assert!(notices[0].contains("brought back"), "{notices:?}");
        assert_eq!(git(&new.path, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(std::fs::read_to_string(new.path.join("a.txt")).unwrap(), "edited");
        assert_eq!(std::fs::read_to_string(new.path.join("new.txt")).unwrap(), "untracked");
        // Brought back once: the ref goes, and the work is where it was.
        assert!(rev(r.path(), &discarded_ref(&new.branch)).is_none());
    }

    #[test]
    fn a_drop_with_nothing_left_keeps_nothing() {
        let r = repo();
        let new = create(r.path(), "n-abc123", &layout(r.path())).unwrap();
        remove(r.path(), &new.path).unwrap();
        git(r.path(), &["branch", "-D", &new.branch]).unwrap();
        assert_eq!(save(r.path(), &new.path, &new.branch, &discarded_ref(&new.branch), "x").unwrap(), None);
    }

    #[test]
    fn restoring_twice_reuses_the_worktree() {
        let r = repo();
        let new = create(r.path(), "t-abc123", &layout(r.path())).unwrap();
        std::fs::write(new.path.join("wip.txt"), "wip").unwrap();
        // A start that failed after git made the worktree runs again.
        assert!(create(r.path(), "t-abc123", &layout(r.path())).unwrap().notices.is_empty());
        assert!(new.path.join("wip.txt").exists());
    }

    #[test]
    fn a_new_worktree_gets_its_submodules_and_included_files() {
        let r = repo();
        let sub = repo();
        git(
            r.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &sub.path().to_string_lossy(),
                "sub",
            ],
        )
        .unwrap();
        std::fs::write(r.path().join(".gitignore"), ".env\n*.log\n").unwrap();
        std::fs::write(r.path().join(".worktreeinclude"), ".env\nnotes.txt\n").unwrap();
        git(r.path(), &["add", "."]).unwrap();
        git(r.path(), &["commit", "-q", "-m", "setup"]).unwrap();
        std::fs::write(r.path().join(".env"), "SECRET=1").unwrap();
        std::fs::write(r.path().join("debug.log"), "noise").unwrap();
        // Named, but not ignored: git would track it, so it is not copied.
        std::fs::write(r.path().join("notes.txt"), "mine").unwrap();

        let made = create(r.path(), "w-abc123", &layout(r.path())).unwrap();
        // Git clones a submodule from a local path only when the user's own
        // config allows it, which a test cannot set without touching it: the
        // update is tried, and its failure reported rather than fatal.
        assert_eq!(made.notices.len(), 1, "{:?}", made.notices);
        assert!(made.notices[0].starts_with("submodules not checked out: git submodule update"));
        assert_eq!(std::fs::read_to_string(made.path.join(".env")).unwrap(), "SECRET=1");
        assert!(!made.path.join("debug.log").exists() && !made.path.join("notes.txt").exists());

        assert!(included_changes(r.path(), &made.path).unwrap().is_empty());
        std::fs::write(made.path.join(".env"), "SECRET=2").unwrap();
        assert_eq!(included_changes(r.path(), &made.path).unwrap(), [".env"]);
    }

    #[test]
    fn a_repo_with_no_commits_says_so() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]).unwrap();
        let e = create(d.path(), "e-abc123", &layout(d.path())).err().unwrap().to_string();
        assert!(e.contains("no commits yet"), "{e}");
    }
}
