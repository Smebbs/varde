//! Which project a directory belongs to, found without spawning git: a
//! hook must stay in the low milliseconds.

use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

/// The checkout containing `dir`: the top of its repository, or of the
/// linked worktree it is in. A directory outside any repository is its
/// own project. A worktree varde made for a task still belongs to its
/// repository, which only the store knows: see [`of`].
pub fn root(dir: &Path) -> PathBuf {
    dir.ancestors().find(|d| d.join(".git").exists()).unwrap_or(dir).to_path_buf()
}

/// The project `dir` is in: the checkout you are in, so a worktree of
/// your own is a project of its own, apart from a task's worktree varde
/// made, which belongs to the repository it was made in. One lookup by
/// key, cheap enough for a hook.
pub fn of(conn: &Connection, dir: &Path) -> PathBuf {
    let root = root(dir);
    if !root.join(".git").is_file() {
        return root;
    }
    conn.prepare_cached("SELECT repo FROM worktree WHERE path = ?1")
        .and_then(|mut s| s.query_row([root.to_string_lossy()], |r| r.get::<_, String>(0)).optional())
        .ok()
        .flatten()
        .map(PathBuf::from)
        .unwrap_or(root)
}

/// `path` with its `.` and `..` resolved by name, with no system call.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn subdirectories_belong_to_the_repo() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("repo/.git")).unwrap();
        fs::create_dir_all(d.path().join("repo/src/deep")).unwrap();
        assert_eq!(root(&d.path().join("repo/src/deep")), d.path().join("repo"));
    }

    fn store() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE worktree (path TEXT PRIMARY KEY, repo TEXT NOT NULL)").unwrap();
        conn
    }

    /// A linked worktree of `main` at `wt`, as `git worktree add` leaves it.
    fn linked(main: &Path, wt: &Path) {
        fs::create_dir_all(main.join(".git/worktrees/x")).unwrap();
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/x").display())).unwrap();
    }

    #[test]
    fn a_worktree_of_your_own_is_its_own_project() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("repo");
        let wt = d.path().join("repo-feature");
        linked(&main, &wt);
        assert_eq!(of(&store(), &wt.join("src")), wt);
        assert_eq!(of(&store(), &main), main);
    }

    #[test]
    fn a_task_worktree_varde_made_belongs_to_its_repo() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("repo");
        let wt = main.join(".worktrees/x");
        linked(&main, &wt);
        let conn = store();
        conn.execute("INSERT INTO worktree VALUES (?1, ?2)", [wt.to_string_lossy(), main.to_string_lossy()])
            .unwrap();
        assert_eq!(of(&conn, &wt.join("src")), main);
    }

    #[test]
    fn a_submodule_is_its_own_project() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("repo/sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
        assert_eq!(of(&store(), &sub), sub);
    }

    #[test]
    fn outside_a_repo_the_directory_is_the_project() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(of(&store(), d.path()), d.path());
    }
}
