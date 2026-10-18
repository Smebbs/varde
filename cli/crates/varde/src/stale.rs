//! Whether what a cairn says about code still names code that exists:
//! each bullet's anchor, a path or an identifier, checked against the
//! checkout it is about. Runs detached, after an update (that cairn's
//! anchors only) and from `varde git-refresh` (the least recently checked,
//! a bounded number at a time); a bullet whose anchor no longer resolves
//! is marked, never deleted. Every check has a deadline: an anchor not
//! reached keeps what it was.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Result;
use varde_core::cairn;
use varde_core::memory::{self, Anchored, Owner};
use varde_core::store::Store;

/// An anchor is checked by `varde git-refresh` at most this often.
pub const EVERY_MS: i64 = 10 * 60 * 1000;
/// At most this many anchors are checked by one `git-refresh`, the least
/// recently checked first.
const PER_REFRESH: usize = 50;
/// How long one check may take in all.
const DEADLINE: Duration = Duration::from_secs(2);

/// Check the anchors not checked for `EVERY_MS`, a bounded number of them.
pub fn check_due(store: &Store, now: i64) -> Result<usize> {
    let due = memory::anchored_due(&store.conn, now - EVERY_MS, PER_REFRESH)?;
    check(store, &due, now)
}

/// Check the anchors of one cairn, or one project's cairn: what an
/// update just wrote.
pub fn check_doc(store: &Store, owner: Owner, now: i64) -> Result<usize> {
    check(store, &memory::anchored(&store.conn, owner)?, now)
}

/// Check `items`, each in its checkout, within `DEADLINE`. Returns how
/// many were found stale.
fn check(store: &Store, items: &[Anchored], now: i64) -> Result<usize> {
    let until = Instant::now() + DEADLINE;
    let mut by_dir: HashMap<&str, Vec<&Anchored>> = HashMap::new();
    for a in items {
        by_dir.entry(a.dir.as_str()).or_default().push(a);
    }
    let mut results = Vec::new();
    for (dir, items) in by_dir {
        results.extend(resolve(&PathBuf::from(dir), &items, until));
    }
    memory::set_stale(&store.conn, &results, now)?;
    Ok(results.iter().filter(|(_, stale)| *stale).count())
}

/// Whether each of `items` is stale in checkout `dir`, for those that
/// could be told by `until`. A checkout that cannot be read tells none.
fn resolve(dir: &Path, items: &[&Anchored], until: Instant) -> Vec<(i64, bool)> {
    let Ok(files) = crate::git::git(dir, &["ls-files", "-z"]) else { return Vec::new() };
    let files: Vec<&str> = files.split('\0').filter(|f| !f.is_empty()).collect();
    let mut found: HashMap<&str, bool> = HashMap::new();
    let mut out = Vec::new();
    for a in items {
        if cairn::is_path(&a.anchor) || outside(dir, &a.anchor) {
            // A path outside the checkout (`~/.config/…`, `../shared/…`)
            // is the user's machine, not the code: never stale.
            out.push((a.id, !outside(dir, &a.anchor) && !path_exists(dir, &files, &a.anchor)));
            continue;
        }
        // A name that is a tracked file's, `Makefile` or `Dockerfile`, is
        // that file: its contents need not say it.
        if path_exists(dir, &files, &a.anchor) {
            out.push((a.id, false));
            continue;
        }
        // `CacheKey::new` resolves when `CacheKey` and `new` are both
        // found: `new` alone is in almost any code.
        let mut stale = Some(false);
        for term in terms(&a.anchor) {
            if !found.contains_key(term) {
                match grep(dir, term, until) {
                    Some(seen) => found.insert(term, seen),
                    None => {
                        stale = None;
                        break;
                    }
                };
            }
            if found.get(term) == Some(&false) {
                stale = Some(true);
                break;
            }
        }
        if let Some(stale) = stale {
            out.push((a.id, stale));
        }
    }
    out
}

/// What of an identifier anchor must be found in the code: each part of a
/// `::` path, and of a dotted name, which code writes apart:
/// `config.memory.enabled` is `[memory]` and then `enabled = …`.
fn terms(anchor: &str) -> Vec<&str> {
    let mut parts: Vec<&str> =
        anchor.split("::").flat_map(|p| p.split('.')).filter(|p| !p.is_empty()).collect();
    parts.dedup();
    parts
}

/// Whether `anchor` is an absolute or `~/` path that is not in checkout
/// `dir`.
fn outside(dir: &Path, anchor: &str) -> bool {
    let full = match anchor.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => Path::new(&home).join(rest),
            None => return true,
        },
        None if anchor.starts_with('/') => PathBuf::from(anchor),
        // `../shared/env.toml` is beside the checkout, not in it.
        None => return anchor.starts_with("../"),
    };
    !full.starts_with(dir)
}

/// A path resolves when it is there, or a tracked file or directory ends
/// with it: a bullet may name `src/job.rs` of `cli/crates/varde/src/job.rs`.
fn path_exists(dir: &Path, files: &[&str], anchor: &str) -> bool {
    let p = anchor.trim_start_matches("./");
    let on_disk = match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").is_some_and(|h| Path::new(&h).join(rest).exists()),
        None if p.starts_with('/') => Path::new(p).exists(),
        None => dir.join(p).exists(),
    };
    if on_disk {
        return true;
    }
    let p = p.trim_end_matches('/');
    files.iter().any(|f| {
        f.strip_suffix(p).is_some_and(|head| head.is_empty() || head.ends_with('/'))
            || f.find(&format!("{p}/")).is_some_and(|i| i == 0 || f[..i].ends_with('/'))
    })
}

/// Whether `term` occurs in the checkout's tracked files: one `git grep
/// -q`, which stops at the first match. `None` when it could not be told
/// by `until` (it is stopped then), or git could not search.
fn grep(dir: &Path, term: &str, until: Instant) -> Option<bool> {
    if Instant::now() >= until {
        return None;
    }
    let mut child = crate::git::command(dir, &["grep", "-q", "-I", "-F", "-e", term])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match child.try_wait() {
            // 1 is no match; anything above, git could not search.
            Ok(Some(status)) => {
                return match status.code() {
                    Some(0) => Some(true),
                    Some(1) => Some(false),
                    _ => None,
                }
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_resolves_by_its_tail() {
        let files = ["cli/crates/varde/src/job.rs", "docs/clients.md"];
        let dir = Path::new("/nonexistent");
        assert!(path_exists(dir, &files, "cli/crates/varde/src/job.rs"));
        assert!(path_exists(dir, &files, "src/job.rs"));
        assert!(path_exists(dir, &files, "job.rs"));
        assert!(path_exists(dir, &files, "docs/"));
        assert!(path_exists(dir, &files, "crates/varde"));
        assert!(!path_exists(dir, &files, "ob.rs"));
        assert!(!path_exists(dir, &files, "src/run.rs"));
    }

    #[test]
    fn a_path_outside_the_checkout_is_never_stale() {
        let dir = tempfile::tempdir().unwrap();
        let ok = |args: &[&str]| assert!(crate::git::git(dir.path(), args).is_ok());
        ok(&["init", "-q"]);
        std::fs::write(dir.path().join("run.rs"), "fn main() {}\n").unwrap();
        ok(&["add", "run.rs"]);
        let inside = format!("{}/gone.rs", dir.path().display());
        let anchors = ["~/.config/nowhere/config.toml", "/etc/nowhere.conf", &inside];
        assert_eq!(stale_in(dir.path(), &anchors), [(1, false), (2, false), (3, true)]);
    }

    /// `resolve` of `anchors`, numbered from 1, in checkout `dir`.
    fn stale_in(dir: &Path, anchors: &[&str]) -> Vec<(i64, bool)> {
        let items: Vec<Anchored> = (1..)
            .zip(anchors)
            .map(|(id, a)| Anchored { id, anchor: a.to_string(), dir: String::new() })
            .collect();
        let refs: Vec<&Anchored> = items.iter().collect();
        resolve(dir, &refs, Instant::now() + Duration::from_secs(5))
    }

    #[test]
    fn a_qualified_name_is_found_by_each_of_its_parts() {
        assert_eq!(terms("CacheKey::new"), ["CacheKey", "new"]);
        assert_eq!(terms("OAUTH_CLIENT_ID"), ["OAUTH_CLIENT_ID"]);
        assert_eq!(terms("config.memory.enabled"), ["config", "memory", "enabled"]);
    }

    /// The review's false marks: a path beside the checkout, files with
    /// no extension, and a dotted name the code writes apart.
    #[test]
    fn what_is_there_in_another_form_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let ok = |args: &[&str]| assert!(crate::git::git(dir.path(), args).is_ok());
        ok(&["init", "-q"]);
        std::fs::create_dir(dir.path().join("deploy")).unwrap();
        std::fs::write(dir.path().join("Makefile"), "all:\n\ttrue\n").unwrap();
        std::fs::write(dir.path().join("deploy/Dockerfile"), "FROM scratch\n").unwrap();
        std::fs::write(dir.path().join("config.toml"), "# the config\n[memory]\nenabled = true\n").unwrap();
        ok(&["add", "."]);
        let anchors = [
            "../shared/env.toml",
            "Makefile",
            "Dockerfile",
            "config.memory.enabled",
            "Justfile",
            "config.memory.disabled_by",
        ];
        assert_eq!(
            stale_in(dir.path(), &anchors),
            [(1, false), (2, false), (3, false), (4, false), (5, true), (6, true)]
        );
    }

    #[test]
    fn nothing_is_told_past_the_deadline() {
        let gone = Instant::now() - Duration::from_millis(1);
        assert_eq!(grep(Path::new("."), "fn", gone), None);
    }
}
