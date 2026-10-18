//! The user's settings: `$VARDE_CONFIG`, else
//! `$XDG_CONFIG_HOME/varde/config.toml`, else `~/.config/varde/config.toml`.
//! Every key is optional. A repository's own settings live in its git
//! config (`varde.worktree`, `varde.base`, `varde.setup`), so nothing is
//! matched by path and nothing committed is ever run.
//!
//! Hooks never read this: what a key changes happens in `start`, `run`
//! and the detached jobs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The agent of a new conversation; `$VARDE_AGENT` wins.
    pub agent: Option<String>,
    pub agents: BTreeMap<String, Agent>,
    pub memory: Memory,
    pub worktree: Worktree,
    pub board: BoardConfig,
    /// The board's keys, by action: `park = "P"`.
    pub keys: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BoardConfig {
    /// `project` (the default) or `all`: whether the board opens kept to
    /// the project it is opened in, or showing every project.
    pub scope: Scope,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Project,
    All,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Agent {
    /// The program and leading arguments varde runs in place of the
    /// agent's own name, e.g. a wrapper script.
    pub command: Option<Vec<String>>,
    /// Arguments added to a session a schedule starts in the background,
    /// e.g. `["--permission-mode", "auto"]`: nobody is there to approve a
    /// tool, and varde widens nothing on its own.
    pub background: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Memory {
    /// `false` stops cairn updates, and with them every model call varde
    /// makes. Notes are still kept.
    pub enabled: bool,
    /// The model a cairn update uses; the agent's default when unset.
    pub model: Option<String>,
    /// `false` keeps the title varde cuts from a first prompt or a
    /// context, rather than have the model name the task (§6.2).
    pub titles: bool,
    /// The share of fresh sessions, 0 to 1, given no memory, so that
    /// `varde eval` can compare them with the rest. Off (0) by default.
    pub holdout: f64,
}

impl Default for Memory {
    fn default() -> Self {
        Memory { enabled: true, model: None, titles: true, holdout: 0.0 }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Worktree {
    /// `ask`, `always` or `never`: whether a task's first run gets a worktree.
    pub default: Option<Policy>,
    /// Where worktrees go: relative to the repository, or absolute.
    /// `{repo}` is the repository's directory name.
    pub dir: Option<String>,
    pub branch_prefix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Ask,
    Always,
    Never,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "ask" => Some(Policy::Ask),
            "always" => Some(Policy::Always),
            "never" => Some(Policy::Never),
            _ => None,
        }
    }

    /// As `start` takes it: `None` asks.
    pub fn want(self) -> Option<bool> {
        match self {
            Policy::Ask => None,
            Policy::Always => Some(true),
            Policy::Never => Some(false),
        }
    }
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("VARDE_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    home().join("varde/config.toml")
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
}

/// Reads the file; a missing one is the defaults.
pub fn load(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let c: Config = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
            if let Err(e) = check(&c) {
                anyhow::bail!("reading {}: {e}", path.display());
            }
            Ok(c)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The settings, read once. A broken file is reported once and then
/// treated as absent, so a typo never stops a session from starting;
/// `varde doctor` reports it as a failure.
pub fn get() -> &'static Config {
    &loaded().0
}

static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keep a broken file's warning off stderr, for a caller that reports it
/// its own way: a board, which owns the screen, or `doctor`.
pub fn quiet() {
    QUIET.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// What was wrong with the file, for a board, which owns the screen.
pub fn warning() -> Option<&'static str> {
    loaded().1.as_deref()
}

fn loaded() -> &'static (Config, Option<String>) {
    static CONFIG: OnceLock<(Config, Option<String>)> = OnceLock::new();
    CONFIG.get_or_init(|| {
        // Unit tests run with the defaults, never the user's own file.
        let file = if cfg!(test) { PathBuf::new() } else { path() };
        match load(&file) {
            Ok(c) => (c, None),
            Err(e) => {
                let warning = format!("ignoring what varde cannot use in the config: {e:#}");
                if !QUIET.load(std::sync::atomic::Ordering::Relaxed) {
                    eprintln!("varde: {warning}");
                }
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                (lenient(&text), Some(warning))
            }
        }
    })
}

/// What of a config file with something wrong in it can be used: every
/// setting it gets right, each key tried on its own, so one typo costs
/// only itself. A file that is not TOML at all still keeps memory
/// updates off when it says so, since that is what one would not want
/// to lose.
fn lenient(text: &str) -> Config {
    use serde::de::IntoDeserializer;
    let ok = |t: &toml::Table| -> bool {
        Config::deserialize(toml::Value::Table(t.clone()).into_deserializer())
            .is_ok_and(|c| check(&c).is_ok())
    };
    let Ok(file) = text.parse::<toml::Table>() else {
        let off = text.lines().any(|l| l.split('#').next().unwrap_or("").replace(' ', "") == "enabled=false");
        let mut c = Config::default();
        c.memory.enabled = !off;
        return c;
    };
    let mut good = toml::Table::new();
    for (key, value) in file {
        let mut one = toml::Table::new();
        match value {
            // A table keeps the keys it gets right.
            toml::Value::Table(inner) => {
                let mut kept = toml::Table::new();
                for (k, v) in inner {
                    let mut probe = kept.clone();
                    probe.insert(k.clone(), v.clone());
                    one.insert(key.clone(), toml::Value::Table(probe.clone()));
                    if ok(&one) {
                        kept = probe;
                    }
                }
                good.insert(key, toml::Value::Table(kept));
            }
            value => {
                one.insert(key.clone(), value.clone());
                if ok(&one) {
                    good.insert(key, value);
                }
            }
        }
    }
    Config::deserialize(toml::Value::Table(good).into_deserializer()).unwrap_or_default()
}

/// The first value in `c` varde cannot use.
fn check(c: &Config) -> std::result::Result<(), String> {
    if let Some((name, _)) = c.agents.iter().find(|(_, a)| a.command.as_ref().is_some_and(Vec::is_empty)) {
        return Err(format!("agents.{name}.command is empty: name the program, or leave the key out"));
    }
    crate::tui::check_keys(&c.keys)?;
    if !(0.0..=1.0).contains(&c.memory.holdout) {
        return Err(format!("memory.holdout is a share of sessions, from 0 to 1, not {}", c.memory.holdout));
    }
    Ok(())
}

impl Config {
    /// The command that runs `agent`, when the user set one.
    pub fn command(&self, agent: &str) -> Option<&[String]> {
        self.agents.get(agent)?.command.as_deref().filter(|c| !c.is_empty())
    }

    /// `argv`, as the agent's adapter made it, with the user's command for
    /// `agent` in place of its program. `$VARDE_CLAUDE` and the like win.
    pub fn launch(&self, agent: &str, mut argv: Vec<String>) -> Vec<String> {
        let pinned = std::env::var_os(format!("VARDE_{}", agent.to_ascii_uppercase())).is_some();
        if let (false, Some(command), false) = (pinned, self.command(agent), argv.is_empty()) {
            argv.splice(0..1, command.iter().cloned());
        }
        argv
    }

    /// `argv`, a background launch's, with the user's arguments for
    /// `agent` after its program.
    pub fn background(&self, agent: &str, mut argv: Vec<String>) -> Vec<String> {
        if let (Some(a), false) = (self.agents.get(agent), argv.is_empty()) {
            argv.splice(1..1, a.background.iter().cloned());
        }
        argv
    }

    /// Where `repo`'s worktrees go, without `..`: a session's directory is
    /// matched against it, and hooks report that one resolved.
    pub fn worktree_dir(&self, repo: &Path) -> PathBuf {
        let dir = self.worktree.dir.as_deref().unwrap_or(".worktrees");
        let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = dir.replace("{repo}", &name);
        let dir = match dir.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
            None => PathBuf::from(dir),
        };
        varde_core::project::normalize(&repo.join(dir))
    }

    pub fn branch_prefix(&self) -> &str {
        self.worktree.branch_prefix.as_deref().unwrap_or("varde/")
    }
}

/// A repository's own settings, from its git config (and the global one).
#[derive(Debug, Default)]
pub struct Repo {
    pub worktree: Option<Policy>,
    pub base: Option<String>,
    pub setup: Option<String>,
    /// A value that made no sense, which is then treated as unset: a typo
    /// never stops a session.
    pub warning: Option<String>,
}

impl Repo {
    pub fn read(repo: &Path) -> Result<Repo> {
        let out = crate::git::command(repo, &["config", "-z", "--get-regexp", r"^varde\."])
            .output()
            .context("running git")?;
        // 1 is "no such key".
        if !out.status.success() && out.status.code() != Some(1) {
            return Ok(Repo::default());
        }
        let mut r = Repo::default();
        for entry in String::from_utf8_lossy(&out.stdout).split('\0').filter(|e| !e.is_empty()) {
            let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
            let value = value.trim();
            match key {
                "varde.worktree" => match Policy::parse(value) {
                    Some(p) => r.worktree = Some(p),
                    None => {
                        r.warning = Some(format!(
                            "ignoring git config varde.worktree {value:?}: use ask, always or never"
                        ))
                    }
                },
                "varde.base" if !value.is_empty() => r.base = Some(value.to_string()),
                "varde.setup" if !value.is_empty() => r.setup = Some(value.to_string()),
                _ => {}
            }
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_optional_and_unknown_ones_are_refused() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.memory.enabled && c.agent.is_none() && c.branch_prefix() == "varde/");
        assert_eq!(c.board.scope, Scope::Project);
        let c: Config = toml::from_str(
            r#"
            agent = "pi"
            [agents.claude]
            command = ["claude-wrapper", "--x"]
            background = ["--permission-mode", "auto"]
            [memory]
            enabled = false
            [worktree]
            default = "always"
            dir = "../{repo}.worktrees"
            branch_prefix = "me/"
            [board]
            scope = "all"
            "#,
        )
        .unwrap();
        assert_eq!(c.command("claude").unwrap(), ["claude-wrapper", "--x"]);
        std::env::remove_var("VARDE_CLAUDE");
        assert_eq!(c.launch("claude", vec!["claude".into(), "-n".into()]), ["claude-wrapper", "--x", "-n"]);
        assert_eq!(c.launch("pi", vec!["pi".into()]), ["pi"]);
        let bg = c.background("claude", vec!["claude".into(), "-p".into()]);
        assert_eq!(bg, ["claude", "--permission-mode", "auto", "-p"]);
        assert_eq!(c.background("pi", vec!["pi".into(), "-p".into()]), ["pi", "-p"]);
        assert_eq!(c.worktree.default, Some(Policy::Always));
        assert_eq!(c.board.scope, Scope::All);
        assert_eq!(c.worktree_dir(Path::new("/src/app")), Path::new("/src/app.worktrees"));
        assert!(toml::from_str::<Config>("[worktree]\ndefualt = \"always\"").is_err());
    }

    #[test]
    fn a_typo_costs_only_its_own_setting() {
        let c = lenient(
            "agnet = \"pi\"\n[memory]\nenabled = false\nmodle = \"x\"\n[keys]\npark = \"z\"\nparc = \"y\"\n",
        );
        assert!(!c.memory.enabled, "memory stays off");
        assert_eq!(c.keys.get("park").map(String::as_str), Some("z"));
        assert!(!c.keys.contains_key("parc"));
        // Not TOML at all: memory still off when the file says so.
        assert!(!lenient("[memory\nenabled = false\n").memory.enabled);
        assert!(lenient("[memory\nenabled = true\n").memory.enabled);
    }

    #[test]
    fn an_empty_agent_command_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[agents.claude]\ncommand = []\n").unwrap();
        let e = load(&file).unwrap_err();
        assert!(format!("{e:#}").contains("agents.claude.command is empty"), "{e:#}");
    }

    #[test]
    fn a_holdout_is_a_share_of_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[memory]\nholdout = 0.2\n").unwrap();
        assert_eq!(load(&file).unwrap().memory.holdout, 0.2);
        std::fs::write(&file, "[memory]\nholdout = 15\n").unwrap();
        assert!(format!("{:#}", load(&file).unwrap_err()).contains("from 0 to 1"));
    }

    #[test]
    fn a_repository_says_what_it_wants_in_its_git_config() {
        let r = crate::git::tests::repo();
        crate::git::git(r.path(), &["config", "varde.worktree", "never"]).unwrap();
        crate::git::git(r.path(), &["config", "varde.setup", "make deps"]).unwrap();
        let repo = Repo::read(r.path()).unwrap();
        assert_eq!((repo.worktree, repo.setup.as_deref()), (Some(Policy::Never), Some("make deps")));
        crate::git::git(r.path(), &["config", "varde.worktree", "sometimes"]).unwrap();
        let repo = Repo::read(r.path()).unwrap();
        assert_eq!((repo.worktree, repo.setup.as_deref()), (None, Some("make deps")));
        assert!(repo.warning.unwrap().contains("sometimes"));
    }
}
