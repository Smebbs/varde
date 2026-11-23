//! The sandbox (design §6.8): what a session with nobody at it, and the
//! check of its work, can reach. The system's own: bubblewrap on Linux,
//! sandbox-exec on macOS, around whatever agent runs, so no agent is
//! trusted to keep to its own rules.
//!
//! Inside one a session may write its checkout, its agent's own files,
//! temporary files and build caches, and nothing else; the logins varde
//! knows of (ssh, gpg, cloud, gh, browsers) it cannot read. A sandbox of
//! your own shows one (a read-only cloud login, say), lets more be written,
//! sets variables and denies commands. The network stays open: the agent
//! needs its model.
//!
//! varde's store it cannot reach: its varde hands each command to the
//! varde outside (`broker.rs`). It bounds a session's mistakes and what
//! a prompt injected into it can reach, not a program set on getting out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension;
use varde_core::store::Store;

use crate::config;

/// The sandboxes varde has: `default`; `push`, which also shows git's
/// logins, for a step that opens a pull request; and `off`, none.
pub const BUILT_IN: &[&str] = &["default", "push", "off"];

/// Set in every sandbox: varde run inside one starts nothing outside it.
pub const FENCED: &str = "VARDE_FENCED";

/// The logins every sandbox hides.
const HIDDEN: &[&str] = &[
    "~/.ssh",
    "~/.gnupg",
    "~/.aws",
    "~/.azure",
    "~/.config/gcloud",
    "~/.kube",
    "~/.docker/config.json",
    "~/.config/gh",
    "~/.git-credentials",
    "~/.config/git/credentials",
    "~/.netrc",
    "~/.password-store",
    "~/.local/share/keyrings",
    "~/.vault-token",
    "~/.terraform.d/credentials.tfrc.json",
    "~/.oci",
    "~/.mozilla",
    "~/.config/google-chrome",
    "~/.config/chromium",
    "~/.config/BraveSoftware",
    "~/.config/microsoft-edge",
    "~/Library/Keychains",
    "~/Library/Cookies",
    "~/Library/Application Support/Google/Chrome",
    "~/Library/Application Support/Firefox",
    "~/Library/Application Support/Microsoft Edge",
    "~/Library/Application Support/BraveSoftware",
    "~/.cargo/credentials.toml",
    "~/.cargo/credentials",
    "~/.gem/credentials",
    "~/.authinfo",
    "~/.authinfo.gpg",
    "~/.config/containers/auth.json",
    "~/.config/hub",
    "~/.config/glab-cli",
    "~/.Xauthority",
    "~/.bash_history",
    "~/.zsh_history",
    "~/.local/share/fish/fish_history",
    "~/.python_history",
];

/// What `push` shows of them: to push a branch and open a pull request.
const PUSH: &[&str] = &[
    "~/.ssh",
    "~/.config/gh",
    "~/.git-credentials",
    "~/.config/git/credentials",
    "~/.netrc",
    "~/Library/Keychains",
];

/// The caches builds write, left writable in every sandbox: only the
/// caches, never a tool's config or programs beside them, which run as
/// you. What a build downloads there (a crate's source) you then build
/// from, unchecked: the sandbox bounds mistakes, not a program set on
/// getting out.
const CACHES: &[&str] = &[
    "~/.cache/go-build",
    "~/.cache/pip",
    "~/.cache/uv",
    "~/.cache/yarn",
    "~/.cache/pnpm",
    "~/.cache/node-gyp",
    "~/.cache/sccache",
    "~/.cache/zig",
    "~/.cache/deno",
    "~/.cache/huggingface",
    "~/Library/Caches/go-build",
    "~/Library/Caches/pip",
    "~/Library/Caches/Yarn",
    "~/Library/Caches/pnpm",
    "~/Library/Caches/deno",
    "~/Library/Caches/node/corepack",
    "~/Library/Caches/Mozilla.sccache",
    // Cargo's downloads and their locks, not its config or programs.
    "~/.cargo/registry",
    "~/.cargo/git",
    "~/.cargo/.package-cache",
    "~/.cargo/.package-cache-mutate",
    "~/.cargo/.global-cache",
    "~/.npm/_cacache",
    "~/.pnpm-store",
    "~/.yarn/berry/cache",
    "~/.bun/install/cache",
    "~/go/pkg",
    "~/.m2/repository",
    "~/.gradle/caches",
    "~/.gradle/daemon",
    "~/.gradle/native",
    "~/Library/pnpm/store",
];

/// Sockets that run what they are sent as you, or as root, outside it.
const SOCKETS: &[&str] = &["/var/run/docker.sock", "/run/docker.sock", "/run/podman/podman.sock"];

/// What reaches past it, dropped from its environment: agents holding
/// keys, the desktop's bus and the terminal's server.
const UNSET: &[&str] = &[
    "SSH_AUTH_SOCK",
    "GPG_AGENT_INFO",
    "DBUS_SESSION_BUS_ADDRESS",
    "TMUX",
    "TMUX_PANE",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
];

/// A sandbox, its paths made absolute.
#[derive(Debug, Clone)]
pub struct Policy {
    pub name: String,
    /// What it shows of what is hidden.
    pub show: Vec<PathBuf>,
    /// What it lets write beyond the checkout, the agent's and the caches.
    pub write: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub deny: Vec<String>,
    /// Variables it keeps that carry a login, by name or prefix.
    pub pass: Vec<String>,
    /// Whether it reaches your ssh agent, to push.
    pub ssh_agent: bool,
    /// The task's branch, as varde made it: the only refs it may move are
    /// beside it.
    pub branch: Option<String>,
    /// The task's check's guard: what a session in it may read but not
    /// change (its tests, say), which agents otherwise edit to pass.
    pub guard: Vec<String>,
}

/// `~/x` as `$HOME/x`.
pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if p == "~" => home(),
        None => PathBuf::from(p),
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// The sandbox `name` (`None`: the default). `Ok(None)` for `off`.
pub fn policy(name: Option<&str>) -> Result<Option<Policy>> {
    let name = name.unwrap_or("default");
    let all = |v: &[&str]| v.iter().map(|p| expand(p)).collect::<Vec<_>>();
    let mut p = Policy {
        name: name.to_string(),
        show: vec![],
        write: vec![],
        env: vec![],
        deny: vec![],
        pass: vec![],
        ssh_agent: false,
        branch: None,
        guard: vec![],
    };
    match name {
        "off" => return Ok(None),
        "default" => {}
        "push" => {
            p.show = all(PUSH);
            // The keychain writes its lock files beside it.
            p.write = all(&["~/Library/Keychains"]);
            p.pass = ["GH_", "GITHUB_"].map(String::from).to_vec();
            p.ssh_agent = true;
        }
        _ => {
            let Some(f) = config::get().sandboxes.get(name) else {
                let mut names: Vec<String> = BUILT_IN.iter().map(|s| s.to_string()).collect();
                names.extend(config::get().sandboxes.keys().cloned());
                bail!("no sandbox {name}: there are {}", names.join(", "));
            };
            p.show = f.show.iter().map(|s| expand(s)).collect();
            p.write = f.write.iter().map(|s| expand(s)).collect();
            p.env = f.env.iter().map(|(k, v)| (k.clone(), expand(v).display().to_string())).collect();
            p.deny = f.deny.clone();
            p.pass = f.pass.clone();
        }
    }
    Ok(Some(p))
}

/// Where a sandbox, any of them, can write outside a checkout (`dir`'s):
/// caches, temporary files, an agent's session files, a sandbox's own
/// `write`. A link out of a checkout to one leads to what a session left.
pub fn writable(dir: &Path) -> Vec<PathBuf> {
    let s = config::get();
    let mut out: Vec<PathBuf> = CACHES.iter().map(|c| expand(c)).collect();
    out.extend(s.sandbox.write.iter().map(|w| expand(w)));
    out.extend(s.sandboxes.values().flat_map(|f| f.write.iter().map(|w| expand(w))));
    out.push(expand("~/Library/Keychains"));
    for a in varde_agent::all() {
        out.extend(a.writes(dir));
        out.extend(a.private().into_iter().map(|(d, _)| d));
        out.extend(a.reads().into_iter().filter(|(_, w)| *w).map(|(d, _)| d));
    }
    let temp = std::env::temp_dir();
    // macOS: beside a user's temporary files, its caches (`…/C`).
    out.extend(temp.parent().filter(|_| temp.ends_with("T")).map(|p| p.join("C")));
    out.extend([temp, "/tmp".into(), "/var/tmp".into(), "/dev/shm".into()]);
    let real: Vec<PathBuf> = out.iter().filter_map(|p| p.canonicalize().ok()).collect();
    out.extend(real);
    out
}

/// The sandbox a new task gets: `asked`, checked, as kept (`None` for
/// the default). A task made from inside a sandbox gets that sandbox,
/// whatever was asked: a session cannot make itself a way out.
pub fn for_new(asked: Option<&str>) -> Result<Option<String>> {
    if let Some(inside) = std::env::var(FENCED).ok().filter(|v| !v.is_empty()) {
        return Ok((inside != "default").then_some(inside));
    }
    if let Some((held, _)) = HELD.with(|h| h.borrow().clone()) {
        return Ok(held);
    }
    let Some(name) = asked.map(str::trim).filter(|n| !n.is_empty()) else { return Ok(None) };
    policy(Some(name))?;
    Ok((name != "default").then(|| name.to_string()))
}

thread_local! {
    static HELD: std::cell::RefCell<Option<(Option<String>, PathBuf)>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with every task it makes in the sandbox `name` (`None`: the
/// default), whatever they ask, and every file it reads for them in the
/// checkout `root` and no other: what a session wrote in one, made into
/// tasks outside it, stays as bounded as it was.
pub fn holding<T>(name: Option<String>, root: PathBuf, f: impl FnOnce() -> T) -> T {
    HELD.with(|h| *h.borrow_mut() = Some((name, root)));
    let out = f();
    HELD.with(|h| *h.borrow_mut() = None);
    out
}

/// `varde set TASK --sandbox NAME`.
pub fn set(paths: &crate::Paths, reference: &str, name: &str) -> Result<()> {
    not_inside("Changing a task's sandbox")?;
    let store = paths.open(std::time::Duration::from_secs(2))?;
    let id = varde_core::task::resolve(&store, reference)?;
    let kept = for_new(Some(name))?;
    store.conn.execute("UPDATE task SET sandbox = ?2 WHERE id = ?1", rusqlite::params![id, kept])?;
    let short = varde_core::task::short(&id);
    println!(
        "{short}'s background sessions and checks run in the sandbox {} from the next one",
        kept.as_deref().unwrap_or("default")
    );
    Ok(())
}

/// How the system sandboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Bwrap,
    Seatbelt,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Bwrap => "bwrap",
            Backend::Seatbelt => "sandbox-exec",
        }
    }
}

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The system's sandbox, when it has one that works: bubblewrap must be
/// able to make its namespace, which some systems forbid.
pub fn backend() -> Option<Backend> {
    static FOUND: OnceLock<Option<Backend>> = OnceLock::new();
    *FOUND.get_or_init(|| {
        let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
            (SANDBOX_EXEC, &["-p", "(version 1)(allow default)", "/usr/bin/true"])
        } else {
            ("bwrap", &["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "true"])
        };
        let backend = if cfg!(target_os = "macos") { Backend::Seatbelt } else { Backend::Bwrap };
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
            .then_some(backend)
    })
}

/// Whether the sandbox is off because you said so.
pub fn turned_off() -> bool {
    std::env::var("VARDE_SANDBOX").is_ok_and(|v| v == "off") || config::get().sandbox.on == Some(false)
}

/// Whether background work runs in a sandbox: as `sandbox.on` says (or
/// `$VARDE_SANDBOX`, `on` or `off`), else where there is one. An error
/// when one is asked for and there is none.
pub fn enabled() -> Result<bool> {
    let asked = match std::env::var("VARDE_SANDBOX").ok().as_deref() {
        Some("off") => Some(false),
        Some("on") => Some(true),
        _ => config::get().sandbox.on,
    };
    match (asked, backend()) {
        (Some(false), _) => Ok(false),
        (_, Some(_)) => Ok(true),
        // Installed, yet it would not start: no running without it on a
        // passing failure.
        (None, None) if installed() => bail!(
            "the sandbox is installed but would not start: varde doctor says why; sandbox.on = false runs without one"
        ),
        (Some(true), None) => bail!("sandbox.on is set, and this system has no sandbox: {}", install_hint()),
        (None, None) => Ok(false),
    }
}

/// Whether the system's sandbox program is there at all.
fn installed() -> bool {
    if cfg!(target_os = "macos") {
        return Path::new(SANDBOX_EXEC).exists();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("bwrap").is_file()))
}

pub fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS should have /usr/bin/sandbox-exec"
    } else {
        "install bubblewrap (bwrap), and let it make user namespaces"
    }
}

/// How a task's background sessions and checks run: in which sandbox, if
/// any, and whether in one at all.
pub struct Plan {
    /// `None`: the task's sandbox is `off`.
    pub policy: Option<Policy>,
    pub fenced: bool,
}

/// The plan for `task`'s background sessions and checks.
pub fn plan(store: &Store, task: &str) -> Result<Plan> {
    let name: Option<String> = store
        .conn
        .query_row("SELECT sandbox FROM task WHERE id = ?1", [task], |r| r.get(0))
        .optional()?
        .flatten();
    let mut policy = policy(name.as_deref())?;
    if let Some(p) = policy.as_mut() {
        p.branch = crate::sweep::active_worktree(store, task)?.map(|(_, _, branch, _)| branch);
        p.guard = crate::flow::def(store, task)?.map(|d| d.guard).unwrap_or_default();
    }
    let fenced = policy.is_some() && enabled()?;
    // A checkout a sandboxed session worked in is not run in without one,
    // unless you turned it off.
    if policy.is_some() && !fenced && !turned_off() {
        if let Some(dir) = crate::flow::checkout(store, task)?.filter(|d| crate::git::untrusted(d)) {
            bail!(
                "{} was worked in by a sandboxed session, and no sandbox runs here now: varde doctor says why",
                dir.display()
            );
        }
    }
    Ok(Plan { policy, fenced })
}

/// What runs a command in a sandbox: the program and arguments that go
/// first, and its whole environment, which replaces the caller's.
pub type Wrap = (Vec<String>, Vec<(String, String)>);

/// What wraps a command to run it in the sandbox `p` from `dir`.
pub fn wrap(
    p: &Policy,
    dir: &Path,
    agent: Option<&dyn varde_agent::Agent>,
    varde_home: &Path,
    broker: Option<&Path>,
) -> Result<Wrap> {
    // Asked from inside one, through the broker: what it would wrap would
    // run outside.
    not_inside("Running a command in a sandbox")?;
    let backend = backend().with_context(|| format!("no sandbox here: {}", install_hint()))?;
    let s = config::get();
    let mut write: Vec<PathBuf> = vec![];
    let mut kept: Vec<PathBuf> = vec![];
    // The checkout, and of the git directory a worktree commits into only
    // what a commit writes: never what git reads for what to run as you.
    let (root, write_git, kept_git, objects) = checkout_dirs(dir, p.branch.as_deref())?;
    if root.join(".git").exists() {
        crate::git::distrust(&root);
    }
    write.push(root.clone());
    write.extend(write_git);
    kept.extend(kept_git);
    // The repository's objects, read through its quarantine: read-only.
    kept.extend(objects.clone());
    // A session's sandbox keeps the guard read-only: a test it may not
    // change it cannot (the check's own, which may write beside its
    // files, only flags it).
    if agent.is_some() {
        kept.extend(guarded(&root, &p.guard));
    }
    // What the agent's sessions share: its own, empty, under bwrap; under
    // sandbox-exec, which has no such thing, shared, but what it runs
    // read-only.
    let mut private: Vec<PathBuf> = vec![];
    // A file of it (every prompt typed, in every project): hidden.
    let mut theirs: Vec<PathBuf> = vec![];
    if let Some(a) = agent {
        for (d, runs) in a.private() {
            if d.is_file() {
                theirs.push(d);
                continue;
            }
            match (backend, runs) {
                (Backend::Bwrap, _) => private.push(d),
                (_, true) => kept.push(d),
                (_, false) => write.push(d),
            }
        }
        // Its directories, made if missing, to be bound.
        for w in a.writes(dir) {
            if w.extension().is_none() && !w.exists() {
                let _ = std::fs::create_dir_all(&w);
            }
            write.push(w);
        }
    }
    // A cache a tool has not made yet, made now: its parent is not writable.
    for c in CACHES.iter().map(|c| expand(c)) {
        let file = c.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if !file && !c.exists() && c.parent().is_some_and(Path::is_dir) {
            let _ = std::fs::create_dir(&c);
        }
    }
    write.extend(CACHES.iter().map(|c| expand(c)));
    write.extend(s.sandbox.write.iter().map(|w| expand(w)));
    // The agent's cloud login, if it reaches its model with one, and its
    // program, wherever it is (a private /tmp would hide one there).
    let mut show: Vec<(PathBuf, bool)> = agent.map(|a| a.reads()).unwrap_or_default();
    if let Some(program) = agent.and_then(|a| on_path(&crate::start::agent_command(a)[0])) {
        show.push((program, false));
    }
    show.extend(p.show.iter().map(|s| (s.clone(), p.write.contains(s))));
    show.extend(p.write.iter().filter(|w| !p.show.contains(w)).map(|w| (w.clone(), true)));
    // varde's store: its varde reaches it through the broker, and only so.
    let hidden: Vec<PathBuf> = HIDDEN
        .iter()
        .map(|h| expand(h))
        .chain(s.sandbox.hide.iter().map(|h| expand(h)))
        .filter(|h| !p.show.contains(h))
        .chain([varde_home.to_path_buf()])
        .chain(theirs)
        .collect();

    // The environment, whole, for the caller to clear the rest: a login
    // in a variable (a cloud's, a git host's, a key) is dropped but for
    // what the agent reaches its model with and what the sandbox passes.
    let mut claims: Vec<String> = agent.map(|a| a.env()).unwrap_or_default();
    claims.extend(s.sandbox.pass.iter().cloned());
    claims.extend(p.pass.iter().cloned());
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| kept_var(k, &claims)).collect();
    let mut sockets = vec![];
    if let Some(b) = broker {
        env.push((crate::broker::BROKER.into(), b.display().to_string()));
        sockets.push(b.to_path_buf());
    }
    if p.ssh_agent {
        if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK").filter(|s| !s.is_empty()) {
            env.push(("SSH_AUTH_SOCK".into(), sock.to_string_lossy().into_owned()));
            sockets.push(PathBuf::from(sock));
        }
    }
    env.extend(p.env.iter().cloned());
    // varde itself stays on a PATH a sandbox sets: the agent's hooks call it.
    if let (Some(path), Some(dir)) = (
        env.iter().rev().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()),
        crate::exe().parent().map(Path::to_path_buf),
    ) {
        if !std::env::split_paths(&path).any(|d| d == dir) {
            env.push(("PATH".into(), format!("{path}:{}", dir.display())));
        }
    }
    env.push((FENCED.into(), p.name.clone()));
    // Its git objects in a quarantine of its own, the repository's read
    // through it: `import` brings them in, checked, when it is done.
    if let Some(objects) = &objects {
        let q = varde_home.join("quarantine").join(ulid::Ulid::new().to_string());
        std::fs::create_dir_all(&q)?;
        show.push((q.clone(), true));
        env.push((QUARANTINE.into(), q.display().to_string()));
        env.push(("GIT_ALTERNATE_OBJECT_DIRECTORIES".into(), objects.display().to_string()));
    }
    // A signing key is out of reach: commits here go unsigned, as yours
    // after them are not.
    // No upkeep after a commit: it would write where it may not.
    let mut git_config = vec![("commit.gpgsign", "false"), ("maintenance.auto", "false"), ("gc.auto", "0")];
    // Git's own logins, which a helper hands out from the keychain,
    // only where the sandbox shows git's logins.
    if !p.ssh_agent {
        git_config.push(("credential.helper", ""));
    }
    for (i, (k, v)) in git_config.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k.to_string()));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), v.to_string()));
    }
    env.push(("GIT_CONFIG_COUNT".into(), git_config.len().to_string()));
    let real =
        |v: Vec<PathBuf>| -> Vec<PathBuf> { v.into_iter().filter_map(|p| p.canonicalize().ok()).collect() };
    let write = real(write);
    let kept = real(kept);
    let private = real(private);
    let hidden = real(hidden);
    let show: Vec<(PathBuf, bool)> =
        show.into_iter().filter_map(|(p, w)| p.canonicalize().ok().map(|p| (p, w))).collect();
    let sockets = real(sockets);
    let prefix = match backend {
        Backend::Bwrap => {
            env.push(("TMPDIR".into(), "/tmp".into()));
            // The user's runtime directory holds the desktop's bus and the
            // service manager's, which start what they are asked outside:
            // hidden, but for the programs on PATH a version manager keeps there.
            let runtime: Vec<PathBuf> = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .into_iter()
                .chain([PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))])
                .collect();
            let on_path: Vec<PathBuf> =
                std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
            let runtime = real(runtime);
            let on_path: Vec<(PathBuf, bool)> = real(on_path)
                .into_iter()
                .filter(|p| runtime.iter().any(|r| p.starts_with(r)))
                .map(|p| (p, false))
                .collect();
            let mut hidden = hidden;
            hidden.extend(runtime);
            hidden.extend(real(SOCKETS.iter().map(PathBuf::from).collect()));
            let mut show = show;
            show.extend(on_path);
            show.extend(sockets.into_iter().map(|s| (s, false)));
            bwrap(&write, &private, &kept, &hidden, &show)
        }
        Backend::Seatbelt => {
            // A temporary directory of its own, where its sockets go: those
            // it makes there and in its checkout it may reach, and no other
            // (tmux's, an ssh agent's, an editor's). It may write the rest
            // of the temporary directories, which bwrap keeps private.
            clear_temp();
            let temp = std::env::temp_dir().join(format!("varde-{}", ulid::Ulid::new()));
            std::fs::create_dir_all(&temp)?;
            let temp = temp.canonicalize()?;
            env.push(("TMPDIR".into(), format!("{}/", temp.display())));
            let mut write = write;
            write.push(temp.clone());
            let mut sockets = sockets;
            sockets.extend([temp.clone(), root.canonicalize().unwrap_or(root.clone())]);
            let profile = seatbelt(&write, &kept, &hidden, &show, &sockets, p.ssh_agent);
            vec![SANDBOX_EXEC.into(), "-p".into(), profile]
        }
    };
    Ok((prefix, env))
}

/// `s` with what a regex reads as more than itself escaped.
fn regex_escape(s: &str) -> String {
    s.chars().fold(String::new(), |mut out, c| {
        if "\\.^$|?*+()[]{}\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

/// The temporary directories of runs a day gone (macOS), whose sandbox
/// nothing removes as it ends.
fn clear_temp() {
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > day));
        if old && e.file_name().to_string_lossy().starts_with("varde-") && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The paths in the checkout `root` the globs `guard` name: a directory
/// for `dir/*` or `dir/**`, so nothing new goes in it either; else each
/// file git knows that matches.
fn guarded(root: &Path, guard: &[String]) -> Vec<PathBuf> {
    let mut out = vec![];
    for g in guard {
        let dir = g.strip_suffix("/**").or_else(|| g.strip_suffix("/*"));
        match dir.filter(|d| !d.contains('*')) {
            Some(d) => out.push(root.join(d)),
            None => out.extend(
                crate::git::git(root, &["ls-files", "--", &format!(":(glob){g}")])
                    .unwrap_or_default()
                    .lines()
                    .map(|f| root.join(f)),
            ),
        }
    }
    out
}

/// The text of the file `path` under `root`, read through the file
/// opened, never a name checked and then read: a link swapped in between
/// would read anything. `Err` for a file not under `root`, or not one.
pub fn read_beneath(root: &Path, path: &Path) -> Result<String> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let root = root.canonicalize()?;
    // Not waited on: a pipe in its place would hold varde for ever.
    let mut f =
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(root.join(path))?;
    if !f.metadata()?.is_file() {
        bail!("{} is not a file", path.display());
    }
    let real = opened_path(f.as_raw_fd())?;
    if !real.starts_with(&root) {
        bail!("{} is not in {}", path.display(), root.display());
    }
    let mut text = String::new();
    f.read_to_string(&mut text)?;
    Ok(text)
}

/// Where the open file `fd` is.
#[cfg(target_os = "macos")]
fn opened_path(fd: i32) -> Result<PathBuf> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } == -1 {
        bail!("where the file is");
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(String::from_utf8_lossy(&buf[..end]).into_owned()))
}

/// Where the open file `fd` is.
#[cfg(not(target_os = "macos"))]
fn opened_path(fd: i32) -> Result<PathBuf> {
    Ok(std::fs::read_link(format!("/proc/self/fd/{fd}"))?)
}

/// Where `program` is: a path, or the first on PATH.
fn on_path(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        return Some(PathBuf::from(program));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(program)).find(|p| p.is_file())
}

/// Variables that carry a login, by prefix: dropped in a sandbox unless
/// it passes them.
const LOGINS: &[&str] = &[
    "AZURE_",
    "ARM_",
    "AWS_",
    "GOOGLE_",
    "GCP_",
    "GCLOUD_",
    "CLOUDSDK_",
    "KUBECONFIG",
    "DOCKER_",
    "GITHUB_",
    "GH_",
    "GITLAB_",
    "VAULT_",
    "OP_",
    "TF_TOKEN_",
    "DIGITALOCEAN_",
    "HEROKU_",
    "CLOUDFLARE_",
    "OCI_",
];

/// Whether a sandbox keeps the variable `k`: one `claims` names, or one
/// that carries no login and reaches nothing past it.
fn kept_var(k: &str, claims: &[String]) -> bool {
    if k == FENCED || k == "SSH_AUTH_SOCK" {
        return false;
    }
    if claims.iter().any(|c| k.starts_with(c.as_str())) {
        return true;
    }
    !secret_name(k) && !UNSET.contains(&k) && !LOGINS.iter().any(|l| k.starts_with(l))
}

/// Whether a variable's name says it holds a secret, by its words:
/// `GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, `NPM_AUTH`; not `GIT_AUTHOR_NAME`
/// or `…_MAX_OUTPUT_TOKENS`.
pub fn secret_name(k: &str) -> bool {
    let up = k.to_ascii_uppercase();
    if up == "DATABASE_URL" || up.ends_with("_DATABASE_URL") {
        return true;
    }
    // One word of it may be the secret (`PGPASSWORD`, `MYSQL_PWD`).
    let words: Vec<&str> = up.split('_').collect();
    let glued = ["PASSWORD", "PASSWD", "TOKEN", "SECRET", "AUTHKEY", "SECRETKEY", "APIKEY"];
    if words.iter().any(|w| glued.iter().any(|g| w.ends_with(g)))
        || (words.len() > 1 && words.last() == Some(&"PWD"))
    {
        return true;
    }
    up.split('_').any(|w| {
        matches!(
            w,
            "KEY"
                | "APIKEY"
                | "SECRET"
                | "TOKEN"
                | "PASSWORD"
                | "PASSWD"
                | "CREDENTIAL"
                | "CREDENTIALS"
                | "AUTH"
                | "PAT"
        )
    })
}

/// The checkout `dir` is in, what of its git directories a commit there
/// writes, and what in them stays as it is.
///
/// A linked worktree commits into its own git directory, the
/// repository's `objects`, and the refs beside its own branch's. The rest of the
/// repository's git directory (its config, hooks, `commondir`, other
/// worktrees, submodules) and the files that say where a git directory is
/// (`.git`, `commondir`, `gitdir`) are git's reading for what to run, as
/// you: read-only. A main checkout's git directory is all of that at
/// once, so a session in one changes its files but cannot commit.
#[allow(clippy::type_complexity)]
fn checkout_dirs(
    dir: &Path,
    branch: Option<&str>,
) -> Result<(PathBuf, Vec<PathBuf>, Vec<PathBuf>, Option<PathBuf>)> {
    let out =
        crate::git::git(dir, &["rev-parse", "--show-toplevel", "--absolute-git-dir", "--git-common-dir"])
            .ok();
    let parts: Vec<&str> = out.as_deref().map(|o| o.lines().collect()).unwrap_or_default();
    let (root, write, kept, objects) = match parts.as_slice() {
        [root, gitdir, common] => {
            let root = PathBuf::from(root);
            let gitdir = PathBuf::from(gitdir);
            let common = Path::new(common);
            let common = if common.is_absolute() { common.to_path_buf() } else { dir.join(common) };
            let common = common.canonicalize().unwrap_or(common);
            // What varde checks after a run is the files' refs: a reftable
            // repository keeps them where a session writes, out of sight.
            if common.join("reftable").exists() {
                bail!(
                    "{} keeps its refs in a reftable: a session in a sandbox is not run in one",
                    common.display()
                );
            }
            let gitdir = gitdir.canonicalize().unwrap_or(gitdir);
            let mut kept = vec![];
            // Hooks kept in the checkout (`core.hooksPath`, as husky sets it).
            if let Ok(hooks) = crate::git::git(&root, &["config", "--type=path", "core.hooksPath"]) {
                kept.push(hooks_kept(&root, Path::new(hooks.trim()))?);
            }
            if gitdir == common {
                kept.push(common);
                (root, vec![], kept, None)
            } else {
                // Made now if missing, so a session cannot make it.
                let worktree_config = gitdir.join("config.worktree");
                if !worktree_config.exists() {
                    let _ = std::fs::write(&worktree_config, "");
                }
                // The repository's own hooks and config too, though a
                // `write` of yours covers them.
                kept.extend([
                    root.join(".git"),
                    gitdir.join("commondir"),
                    gitdir.join("gitdir"),
                    worktree_config,
                    common.join("hooks"),
                    common.join("config"),
                ]);
                // Its submodules, kept here, not in the repository's
                // `modules/`: git runs what their config names, as you.
                let modules = gitdir.join("modules");
                let _ = std::fs::create_dir_all(&modules);
                kept.push(modules);
                // Of the refs, only where its own branch is (`varde/…`), so
                // it moves no other: not main, not a tag, not a remote's.
                // The branch as varde made it, not as HEAD says, which a
                // session can write; and only one under varde's prefix.
                let prefix = config::get().branch_prefix().trim_end_matches('/').to_string();
                let place = branch
                    .map(Path::new)
                    .and_then(Path::parent)
                    .filter(|p| !prefix.is_empty() && *p == Path::new(&prefix));
                let refs = match place {
                    Some(p) => {
                        vec![common.join("refs/heads").join(p), common.join("logs/refs/heads").join(p)]
                    }
                    None => vec![],
                };
                for r in &refs {
                    let _ = std::fs::create_dir_all(r);
                }
                // Its objects go to a quarantine of its own (`import`):
                // git trusts an object's name without checking it, so one
                // written into the repository's own could change main.
                let mut write = vec![gitdir];
                write.extend(refs);
                (root, write, kept, Some(common.join("objects")))
            }
        }
        _ => (dir.to_path_buf(), vec![], vec![], None),
    };
    // A directory that holds your home holds what runs as you.
    let home = home().canonicalize().unwrap_or_else(|_| home());
    let real = root.canonicalize().unwrap_or(root.clone());
    if home.starts_with(&real) {
        bail!("{} holds your home directory: a session in a sandbox may not write it", real.display());
    }
    Ok((root, write, kept, objects))
}

/// What keeps the hooks at `hooks` (`core.hooksPath`) as they are. In the
/// checkout `root`, its first directory there, made if missing: a
/// directory between could be renamed, and hooks put where git looks.
fn hooks_kept(root: &Path, hooks: &Path) -> Result<PathBuf> {
    let full =
        crate::flow::lexical(&if hooks.is_absolute() { hooks.to_path_buf() } else { root.join(hooks) });
    let Ok(inside) = full.strip_prefix(root) else { return Ok(full) };
    match inside.components().next() {
        Some(std::path::Component::Normal(c)) => {
            let top = root.join(c);
            // A link would be kept where it leads, and itself replaced.
            if std::fs::symlink_metadata(&top).is_ok_and(|m| m.is_symlink()) {
                bail!("{} is a link: a session in a sandbox could replace your hooks", top.display());
            }
            if std::fs::symlink_metadata(&top).is_err() {
                std::fs::create_dir(&top).with_context(|| format!("making {}", top.display()))?;
            }
            Ok(top)
        }
        Some(_) => Ok(full),
        None => bail!("core.hooksPath is the checkout itself: a session in a sandbox would write your hooks"),
    }
}

/// The variable that names a sandbox's quarantine of git objects.
const QUARANTINE: &str = "GIT_OBJECT_DIRECTORY";

/// Bring the git objects a sandboxed session made into the repository of
/// the checkout `dir`, each re-hashed from what it holds, as a push is
/// received: one named for an object it is not stays out. `env` is the
/// sandbox's, which names the quarantine; it is removed after.
pub fn import(env: &[(String, String)], dir: &Path) {
    let Some(q) = env.iter().find(|(k, _)| k == QUARANTINE).map(|(_, v)| PathBuf::from(v)) else { return };
    // Only what it wrote there: no link to elsewhere (a hidden repository),
    // no alternates of its own.
    let _ = std::fs::remove_dir_all(q.join("info"));
    crate::git::sweep(&q);
    // Its worktree's git directory too, before git reads it (a pipe for
    // HEAD); the rest is held after.
    // The checkout as marked before the session ran, not by a `.git` it
    // could have made below.
    let top = crate::git::untrusted_root(dir).unwrap_or_else(|| varde_core::project::root(dir));
    if let (Some(gd), Some(c)) = crate::flow::git_dirs(&top) {
        if gd != c {
            crate::git::sweep(&gd);
        }
    }
    let reader = |args: &[&str]| {
        let mut c = crate::git::command(&top, args);
        c.env(QUARANTINE, &q).env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "");
        c
    };
    // What it holds, loose or packed, by size, nothing read yet: one that
    // inflates past what a repository holds (a zlib bomb) stays out, and
    // so does all of it past what a session makes.
    let listed = crate::git::bounded(&mut reader(&[
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectname) %(objectsize)",
    ]));
    let listed = listed.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut oids = String::new();
    let (mut count, mut total, mut over) = (0u64, 0u64, false);
    for (oid, size) in listed.lines().filter_map(|l| l.split_once(' ')) {
        match size.parse::<u64>() {
            Ok(n) if n <= MAX_OBJECT => {
                oids.push_str(oid);
                oids.push('\n');
                count += 1;
                total += n;
            }
            _ => over = true,
        }
    }
    if count > MAX_OBJECTS || total > MAX_TOTAL {
        eprintln!(
            "varde: what the session committed is over {MAX_OBJECTS} objects or {} GiB: left out",
            MAX_TOTAL >> 30
        );
        let _ = std::fs::remove_dir_all(&q);
        return;
    }
    if over {
        eprintln!("varde: an object the session made is over {} GiB: left out", MAX_OBJECT >> 30);
    }
    // Brought in as a push is received: packed from the quarantine, and
    // indexed again from what each holds, so one named for an object it is
    // not stays out.
    // Checked as a push is (`--strict`), but for what old repositories
    // hold and git only warns of: honest work is not lost for it. A git
    // too old to be told so is asked plainly.
    let lenient = "--strict=zeroPaddedFilemode=ignore,badTimezone=ignore,missingSpaceBeforeDate=ignore,\
                   missingSpaceBeforeEmail=ignore,badDate=ignore,badDateOverflow=ignore,badEmail=ignore,\
                   missingEmail=ignore,missingNameBeforeEmail=ignore,nulInCommit=ignore,\
                   extraHeaderEntry=ignore,fullPathname=ignore,emptyName=ignore";
    let bring = |strict: &str| -> Result<std::process::Output> {
        let mut f = tempfile::tempfile()?;
        {
            use std::io::{Seek, Write};
            f.write_all(oids.as_bytes())?;
            f.rewind()?;
        }
        let mut pack = reader(&["pack-objects", "--stdout", "-q"])
            .stdin(f)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let out = pack.stdout.take().context("its pack")?;
        let indexed = crate::git::bounded(
            crate::git::command(&top, &["index-pack", "--stdin", "--fix-thin", strict]).stdin(out),
        );
        let _ = pack.kill();
        let _ = pack.wait();
        indexed
    };
    let brought = count == 0
        || match bring(lenient) {
            Ok(o) if o.status.success() => true,
            Ok(o) if o.status.code() == Some(129) => bring("--strict").is_ok_and(|o| o.status.success()),
            Ok(o) => {
                eprintln!("varde: {}", String::from_utf8_lossy(&o.stderr).trim());
                false
            }
            Err(e) => {
                eprintln!("varde: {e:#}");
                false
            }
        };
    // Not brought in, kept: the session's commits are there.
    if brought {
        let _ = std::fs::remove_dir_all(&q);
    } else {
        eprintln!("varde: what the session committed could not be brought in; it is kept at {}", q.display());
    }
}

/// The largest object a session's commits bring in, and the most of them.
const MAX_OBJECT: u64 = 1 << 30;
const MAX_OBJECTS: u64 = 2_000_000;
const MAX_TOTAL: u64 = 8 << 30;

fn bwrap(
    write: &[PathBuf],
    private: &[PathBuf],
    kept: &[PathBuf],
    hidden: &[PathBuf],
    show: &[(PathBuf, bool)],
) -> Vec<String> {
    let mut a: Vec<String> =
        // Its own processes: none of them outlives it, and it sees no other.
        [
            "bwrap", "--die-with-parent", "--new-session", "--unshare-pid", "--ro-bind", "/", "/", "--dev-bind",
            "/dev", "/dev", "--proc", "/proc",
        ]
            .map(String::from)
            .to_vec();
    a.extend(["--tmpfs", "/tmp"].map(String::from));
    let bind = |a: &mut Vec<String>, how: &str, p: &Path| {
        let p = p.display().to_string();
        a.extend([how.to_string(), p.clone(), p]);
    };
    for d in private {
        a.extend(["--tmpfs".to_string(), d.display().to_string()]);
    }
    for w in write {
        bind(&mut a, "--bind", w);
    }
    for k in kept {
        bind(&mut a, "--ro-bind", k);
    }
    for h in hidden {
        let p = h.display().to_string();
        if h.is_dir() {
            a.extend(["--tmpfs".to_string(), p]);
        } else {
            a.extend(["--ro-bind".to_string(), "/dev/null".to_string(), p]);
        }
    }
    for (s, w) in show {
        let p = s.display().to_string();
        a.extend([if *w { "--bind" } else { "--ro-bind" }.to_string(), p.clone(), p]);
    }
    a.push("--".into());
    a
}

/// A sandbox-exec profile: everything allowed, then writes refused but
/// where they may go, then the logins hidden. Its last matching rule wins.
fn seatbelt(
    write: &[PathBuf],
    kept: &[PathBuf],
    hidden: &[PathBuf],
    show: &[(PathBuf, bool)],
    sockets: &[PathBuf],
    logins: bool,
) -> String {
    let q = |p: &Path| format!("\"{}\"", p.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
    let path =
        |p: &Path| if p.is_dir() { format!("(subpath {})", q(p)) } else { format!("(literal {})", q(p)) };
    let mut out = String::from("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n");
    // Its own temporary directory is among `write`; of the shared ones
    // only the per-user caches (`…/C`), where Apple's tools keep theirs.
    let mut places: Vec<String> =
        vec!["  (subpath \"/dev\")".into(), "  (regex #\"^/private/var/folders/[^/]+/[^/]+/C/\")".into()];
    places.extend(write.iter().map(|w| format!("  {}", path(w))));
    out.push_str(&places.join("\n"));
    out.push_str(")\n");
    if !kept.is_empty() {
        let k: Vec<String> = kept.iter().map(|k| format!("  {}", path(k))).collect();
        out.push_str(&format!("(deny file-write*\n{})\n", k.join("\n")));
    }
    if !hidden.is_empty() {
        let h: Vec<String> = hidden.iter().map(|h| format!("  {}", path(h))).collect();
        out.push_str(&format!("(deny file-read* file-write*\n{})\n", h.join("\n")));
    }
    for (s, w) in show {
        let what = if *w { "file-read* file-write*" } else { "file-read*" };
        out.push_str(&format!("(allow {what} {})\n", path(s)));
    }
    // Sockets run what they are sent outside it (tmux, docker, an ssh
    // agent), and these programs ask launchd or an app to: refused, but
    // for name lookups and the system log.
    out.push_str(
        "(deny network-outbound (remote unix-socket (path-regex #\"^/\")))\n\
         (allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n\
         (allow network-outbound (remote unix-socket (path-literal \"/private/var/run/syslog\")))\n\
         (deny process-exec (regex #\"/(launchctl|osascript|open|security)$\"))\n\
         (deny lsopen)\n\
         (deny appleevent-send)\n\
         (deny mach-lookup (global-name \"com.apple.coreservices.launchservicesd\"))\n",
    );
    for s in sockets {
        let filter = if s.is_dir() {
            format!("path-regex #\"^{}/\"", regex_escape(&s.display().to_string()))
        } else {
            format!("path-literal {}", q(s))
        };
        out.push_str(&format!("(allow network-outbound (remote unix-socket ({filter})))\n"));
    }
    // The keychain's tool and git's helpers, which hand out what is stored.
    if !logins {
        out.push_str(
            "(deny process-exec (literal \"/usr/bin/security\") (regex #\"/git-credential-[^/]*$\"))\n",
        );
    }
    out
}

/// Refuse `what` from inside a sandbox: what checks a task, what bounds
/// it and what starts next are yours, not a session's to change.
pub fn not_inside(what: &str) -> Result<()> {
    if std::env::var_os(FENCED).is_some_and(|v| !v.is_empty()) {
        bail!("{what} is not for a session in a sandbox: it is the user's to do");
    }
    Ok(())
}

/// The directory a file a session named must be in to be read for it: its
/// checkout, when this runs for a session in a sandbox or for what one
/// wrote (a plan's trail); `None` when it is yours to name any.
pub fn bounded_root() -> Option<PathBuf> {
    if inside() {
        return std::env::current_dir().ok();
    }
    HELD.with(|h| h.borrow().as_ref().map(|(_, root)| root.clone()))
}

/// Whether this runs for a session in a sandbox (the broker's mark).
pub fn inside() -> bool {
    std::env::var_os(FENCED).is_some_and(|v| !v.is_empty())
}

/// Refuse reaching `task` from inside a sandbox unless it is in the
/// session's own project: another's tasks and memory are not its to read.
pub fn own_project(store: &Store, task: &str) -> Result<()> {
    if !inside() {
        return Ok(());
    }
    let run = crate::run_env().context("a session in a sandbox without its run")?;
    let project = |sql: &str, id: &str| -> Result<Option<String>> {
        Ok(store.conn.query_row(sql, [id], |r| r.get(0)).optional()?.flatten())
    };
    let mine = project("SELECT t.project FROM run r JOIN task t ON t.id = r.task WHERE r.id = ?1", &run)?;
    let theirs = project("SELECT project FROM task WHERE id = ?1", task)?;
    if mine.is_none() || mine != theirs {
        bail!("another project's task is not for a session in a sandbox to reach");
    }
    Ok(())
}

/// Refuse acting for `run` from inside a sandbox unless it is the
/// session's own: a note is memory another session is handed.
pub fn own_run(run: &str) -> Result<()> {
    if inside() && crate::run_env().as_deref() != Some(run) {
        bail!("another session's run is not for a session in a sandbox to write to");
    }
    Ok(())
}

/// What a round is told of its sandbox.
pub fn words(p: &Policy, fenced: bool) -> String {
    let mut out = String::new();
    if fenced {
        out.push_str(
            "You work in a sandbox: you can change files only in this checkout, and logins (ssh, git hosts, \
             cloud) are hidden unless this sandbox shows them. What needs more than it allows, leave to the \
             user: end with the exact steps for them to take.",
        );
    }
    if !p.deny.is_empty() {
        out.push_str(&format!(
            " Never run {}: write any such command into your result for the user to run.",
            p.deny.iter().map(|d| format!("`{d}`")).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

/// `varde sandbox [NAME] [-- CMD…]`: what a sandbox allows, or `CMD` run
/// in it here, to see.
pub fn cmd(paths: &crate::Paths, name: Option<&str>, command: &[String]) -> Result<i32> {
    let p = policy(name)?;
    if command.is_empty() {
        for l in describe(name.unwrap_or("default"), p.as_ref()) {
            println!("{l}");
        }
        return Ok(0);
    }
    let dir = std::env::current_dir()?;
    let Some(p) = p else { bail!("the sandbox off is none: run the command as it is") };
    let (mut argv, env) = wrap(&p, &dir, None, &paths.home, None)?;
    argv.extend(command.iter().cloned());
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .with_context(|| format!("starting {}", argv[0]))?;
    // What it left in a checkout, as after a session's run there.
    import(&env, &dir);
    // The checkout it was given, as marked before it ran.
    let held = if crate::git::untrusted(&dir) { crate::flow::hold(&dir) } else { vec![] };
    if !held.is_empty() {
        eprintln!(
            "varde: set aside what git would run as you, which it left at {} (renamed *.varde-held, a link to it removed): look before you use it",
            held.join(", ")
        );
    }
    Ok(status.code().unwrap_or(1))
}

/// What the sandbox `name` lets a session reach, for `varde sandbox`.
fn describe(name: &str, p: Option<&Policy>) -> Vec<String> {
    let s = config::get();
    let mut out = vec![];
    let state = match (enabled(), backend()) {
        (Ok(true), Some(b)) => format!("on ({}): background sessions and checks run in it", b.name()),
        (Ok(false), Some(_)) => {
            "off (sandbox.on = false): background sessions run on their agent's grant".into()
        }
        (Ok(false), None) => {
            format!("none here, so background sessions run on their agent's grant ({})", install_hint())
        }
        (Err(e), _) => format!("{e:#}"),
        (Ok(true), None) => unreachable!("on with no backend is an error"),
    };
    out.push(format!("Sandbox: {state}"));
    let Some(p) = p else {
        out.push(format!("{name}: none; the agent asks as its grant allows"));
        return out;
    };
    let tilde = |p: &Path| {
        p.strip_prefix(home())
            .map(|r| format!("~/{}", r.display()))
            .unwrap_or_else(|_| p.display().to_string())
    };
    out.push(String::new());
    out.push(format!("{name}:"));
    let mut writes = vec!["the checkout and its git directory (not its hooks or config)".to_string()];
    writes.push("the agent's own files, temporary files and build caches".into());
    writes.extend(s.sandbox.write.iter().cloned());
    writes.extend(p.write.iter().map(|w| tilde(w)));
    out.push(format!("  writes   {}", writes.join(", ")));
    // Those there are on this machine: the rest it would hide as well.
    let mut hidden: Vec<String> = HIDDEN
        .iter()
        .map(|h| h.to_string())
        .chain(s.sandbox.hide.iter().cloned())
        .filter(|h| !p.show.contains(&expand(h)) && expand(h).exists())
        .collect();
    hidden.push("varde's store".into());
    out.push(format!("  hides    {}", hidden.join(" ")));
    if !p.show.is_empty() {
        out.push(format!("  shows    {}", p.show.iter().map(|s| tilde(s)).collect::<Vec<_>>().join(" ")));
    }
    if !p.env.is_empty() {
        let e: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        out.push(format!("  sets     {}", e.join(" ")));
    }
    if !p.deny.is_empty() {
        out.push(format!("  denies   {}", p.deny.join(", ")));
    }
    out.push("  network  open: the agent needs its model".into());
    let mine: BTreeMap<&String, &config::Fence> = s.sandboxes.iter().collect();
    let mut others: Vec<String> = BUILT_IN.iter().map(|b| b.to_string()).collect();
    others.extend(mine.keys().map(|k| k.to_string()));
    others.retain(|o| o != name);
    out.push(String::new());
    out.push(format!(
        "Others: {}. varde sandbox NAME shows one; varde sandbox NAME -- CMD runs CMD in it here.",
        others.join(", ")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seatbelt_profile_refuses_writes_but_where_they_may_go_and_hides_logins() {
        let d = tempfile::tempdir().unwrap();
        let w = d.path().join("wt");
        let h = d.path().join("ssh");
        let k = d.path().join("wt/.git/hooks");
        std::fs::create_dir_all(&k).unwrap();
        std::fs::create_dir_all(&h).unwrap();
        let p = seatbelt(
            std::slice::from_ref(&w),
            std::slice::from_ref(&k),
            std::slice::from_ref(&h),
            &[],
            &[],
            false,
        );
        let at = |needle: &str| p.find(needle).unwrap_or_else(|| panic!("{needle} in {p}"));
        // Last match wins: each narrowing comes after what it narrows.
        assert!(at("(deny file-write*)") < at(&format!("(subpath \"{}\")", w.display())));
        assert!(
            at(&format!("(subpath \"{}\")", w.display())) < at(&format!("(subpath \"{}\")", k.display()))
        );
        assert!(
            p.contains(&format!("(deny file-read* file-write*\n  (subpath \"{}\"))", h.display())),
            "{p}"
        );
    }

    #[test]
    fn what_a_session_committed_comes_in_rehashed_and_a_forged_object_stays_out() {
        let r = crate::git::tests::repo();
        let dir = r.path();
        let q = tempfile::tempdir().unwrap();
        let in_q = |args: &[&str], input: &[u8]| {
            let mut c = crate::git::command(dir, args);
            c.env(super::QUARANTINE, q.path());
            let o = crate::git::bounded_input(&mut c, input).unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let good = in_q(&["hash-object", "-w", "--stdin"], b"real work\n");
        // A loose object under the name of another's content.
        let forged = in_q(&["hash-object", "-w", "--stdin"], b"what it claims\n");
        let other = in_q(&["hash-object", "-w", "--stdin"], b"what it holds\n");
        // A tree as old repositories have them (a zero-padded mode), which
        // git only warns of: honest work, brought in.
        let blob: Vec<u8> =
            (0..20).map(|i| u8::from_str_radix(&good[i * 2..i * 2 + 2], 16).unwrap()).collect();
        let mut tree = b"0100644 f\0".to_vec();
        tree.extend(&blob);
        let old = in_q(&["hash-object", "-t", "tree", "--literally", "-w", "--stdin"], &tree);
        let path = |o: &str| q.path().join(&o[..2]).join(&o[2..]);
        std::fs::remove_file(path(&forged)).unwrap();
        std::fs::rename(path(&other), path(&forged)).unwrap();
        let env = vec![(super::QUARANTINE.to_string(), q.path().display().to_string())];
        super::import(&env, dir);
        let has = |o: &str| crate::git::git(dir, &["cat-file", "-e", o]).is_ok();
        assert!(has(&good) && has(&other) && has(&old), "brought in, by what each holds");
        assert!(!has(&forged), "no object under a name not its own");
        assert!(!q.path().exists(), "the quarantine is gone");
    }

    #[test]
    fn hooks_in_the_checkout_are_kept_from_their_first_directory_there() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        // Missing, made: a session cannot make it, nor rename what holds it.
        assert_eq!(super::hooks_kept(r, Path::new(".husky/_")).unwrap(), r.join(".husky"));
        assert!(r.join(".husky").is_dir());
        assert_eq!(super::hooks_kept(r, &r.join("tools/hooks")).unwrap(), r.join("tools"));
        assert_eq!(super::hooks_kept(r, Path::new("/etc/hooks")).unwrap(), Path::new("/etc/hooks"));
        assert!(super::hooks_kept(r, Path::new(".")).is_err());
    }

    #[test]
    fn bwrap_hides_after_it_binds_and_shows_after_it_hides() {
        let d = tempfile::tempdir().unwrap();
        let gc = d.path().join("gcloud");
        std::fs::create_dir_all(&gc).unwrap();
        let adc = gc.join("adc.json");
        std::fs::write(&adc, "{}").unwrap();
        let a =
            bwrap(&[d.path().to_path_buf()], &[], &[], std::slice::from_ref(&gc), &[(adc.clone(), false)]);
        let at = |x: &Path| a.iter().position(|s| s == &x.display().to_string()).unwrap();
        assert!(at(d.path()) < at(&gc) && at(&gc) < at(&adc), "{a:?}");
        assert_eq!(a.last().map(String::as_str), Some("--"));
    }

    #[test]
    fn a_secret_is_told_by_its_words() {
        for k in [
            "GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "NPM_AUTH",
            "PGPASSWORD",
            "MYSQL_PWD",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(secret_name(k), "{k}");
        }
        for k in ["GIT_AUTHOR_NAME", "CLAUDE_CODE_MAX_OUTPUT_TOKENS", "PWD", "KEYMAP", "PATH"] {
            assert!(!secret_name(k), "{k}");
        }
    }

    #[test]
    fn the_built_in_sandboxes_resolve_and_an_unknown_one_is_named() {
        assert!(policy(None).unwrap().is_some_and(|p| p.show.is_empty()));
        assert!(policy(Some("push")).unwrap().is_some_and(|p| p.show.iter().any(|s| s.ends_with(".ssh"))));
        assert!(policy(Some("off")).unwrap().is_none());
        let e = policy(Some("nope")).unwrap_err().to_string();
        assert!(e.contains("default, push, off"), "{e}");
    }
}
