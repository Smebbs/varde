use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use varde_core::plan::Want;
use varde_core::store::Store;

mod config;
mod deliver;
mod doctor;
mod done;
mod eval;
mod git;
mod hook;
mod job;
mod list;
mod memory_cmd;
mod pending;
mod process;
mod run;
mod stale;
mod start;
mod sweep;
mod tasks;
mod tmux;
mod tui;

#[derive(Parser)]
#[command(
    name = "varde",
    version,
    about = "Run AI coding-agent sessions as tasks, with memory",
    after_help = "With no command, varde opens the board in the terminal."
)]
struct Cli {
    /// With no command, the board opens in the terminal.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the board.
    List {
        #[arg(long)]
        json: bool,
        /// Include old done tasks and dropped ones, and list the rows of
        /// collapsed groups (Parked).
        #[arg(long)]
        all: bool,
    },
    /// Create a task.
    New {
        #[arg(required = true)]
        title: Vec<String>,
        /// Project directory; defaults to the repository you are in.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Task description, or `-` to read it from stdin.
        #[arg(long)]
        body: Option<String>,
        /// Where the task also lives, e.g. `org`.
        #[arg(long, hide = true)]
        external: Option<String>,
        /// Defer it from this run (used by the agent): it goes to the Inbox,
        /// in the project of the run's task unless --project names another,
        /// and names that task as its origin.
        #[arg(long, value_name = "RUN", conflicts_with = "external", hide = true)]
        from: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change a task's title, body or project.
    Set {
        task: String,
        #[arg(long)]
        title: Option<String>,
        /// The body, or `-` to read it from stdin.
        #[arg(long)]
        body: Option<String>,
        /// Move the task to the project DIR is in. Refused while it has an
        /// active worktree or a live session, which belong to the old checkout.
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
    },
    /// Move Inbox tasks into Open.
    Accept {
        #[arg(required = true)]
        tasks: Vec<String>,
    },
    /// Record where a task also lives, e.g. `org`. It leaves the Inbox.
    #[command(hide = true)]
    Link { task: String, external: String },
    /// Forget where a task lives elsewhere.
    #[command(hide = true)]
    Unlink { task: String },
    /// Show a task: its cairn, runs and worktree; or a project's cairn.
    Show {
        /// The task; inside a session varde runs, its own.
        task: Option<String>,
        /// The cairn of the project DIR (default: here) is in, in place of a task.
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".", conflicts_with_all = ["task", "no_git"])]
        project: Option<PathBuf>,
        /// Every earlier version of the cairn, and why it changed.
        #[arg(long)]
        history: bool,
        /// Leave out the git and PR status, which runs git and gh.
        #[arg(long)]
        no_git: bool,
        #[arg(long)]
        json: bool,
    },
    /// Mark what a task's sessions finished as read (used by clients when
    /// you look at one).
    #[command(hide = true)]
    Seen { task: String },
    /// Pin a finding for the task's cairn (used by the agent).
    #[command(hide = true)]
    Note {
        /// The run; defaults to VARDE_RUN.
        #[arg(long)]
        run: Option<String>,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Search what earlier tasks learned.
    Recall {
        #[arg(required = true)]
        words: Vec<String>,
        /// Every project, not just the one you are in.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Edit a task's cairn and the context above it, or a project's cairn,
    /// in $EDITOR; your lines are kept by later updates.
    Edit {
        #[arg(required_unless_present = "project")]
        task: Option<String>,
        /// The cairn of the project DIR (default: here) is in, in place of a task.
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".", conflicts_with = "task")]
        project: Option<PathBuf>,
        /// Read the edited text from stdin instead of opening an editor.
        #[arg(long)]
        stdin: bool,
        /// Print the text to edit, for a client's own editor, and exit.
        #[arg(long, conflicts_with = "stdin")]
        print: bool,
    },
    /// Run one queued cairn update (started by varde itself).
    #[command(hide = true)]
    Job { id: String },
    /// Decide how to start a task and record the run, without launching it
    /// (for clients; `attach` does both).
    Start {
        task: String,
        #[command(flatten)]
        how: StartArgs,
        #[arg(long)]
        json: bool,
    },
    /// Start a task in this terminal.
    Attach {
        task: String,
        #[command(flatten)]
        how: StartArgs,
    },
    /// Close a task; its worktree goes once its work has landed.
    Done {
        /// The task; inside a session varde runs, its own.
        task: Option<String>,
        /// Remove the worktree; refused with uncommitted or unmerged work,
        /// or changed files from .worktreeinclude.
        #[arg(long)]
        remove: bool,
        /// With --remove: remove it even so. Unmerged commits stay on the branch.
        #[arg(long, requires = "remove")]
        force: bool,
        /// Leave the task's running sessions alone; by default they are
        /// stopped, and starting the task again resumes them.
        #[arg(long, hide = true)]
        keep_running: bool,
        #[arg(long)]
        json: bool,
    },
    /// Put a task aside; starting it again picks it back up.
    ///
    /// It leaves the board's working groups, and its worktree is left as it is.
    Park {
        /// The task; inside a session varde runs, its own.
        task: Option<String>,
        /// Leave the task's running sessions alone; by default they are
        /// stopped, and starting the task again resumes them.
        #[arg(long, hide = true)]
        keep_running: bool,
        #[arg(long)]
        json: bool,
    },
    /// Throw a task away; starting it again brings it back.
    ///
    /// Its sessions are stopped. Work in its worktree is kept under
    /// refs/varde/discarded/, and the worktree is removed.
    Drop {
        task: String,
        #[arg(long)]
        json: bool,
    },
    /// Move a done, parked or dropped task back to Open.
    Reopen { task: String },
    /// Launch a run `varde start` prepared, in this terminal.
    #[command(hide = true)]
    Run { run: String },
    /// Wait for a closed task's sessions to go, then SIGKILL any that linger.
    /// Spawned detached by `varde done`, `park` and `drop`; not meant to be run by hand.
    #[command(hide = true)]
    Reap {
        task: String,
        /// A run to leave alone: the one `done` ran under.
        #[arg(long)]
        spare: Vec<String>,
    },
    /// Check the git work of every task on the board, and remove done
    /// tasks' worktrees whose work has landed. Started by `varde list`.
    #[command(hide = true)]
    GitRefresh,
    /// Report what memory was worth: reach, use, re-discovery, fidelity,
    /// staleness and cost, from the store and the agents' transcripts.
    Eval {
        /// Every project, not just the one you are in.
        #[arg(long)]
        all: bool,
        /// Only runs started since a date (2026-10-01) or a number of days
        /// (7d). It looks back 30 days at most.
        #[arg(long, value_name = "D")]
        since: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Check the installation and the store.
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct StartArgs {
    /// Always a new conversation.
    #[arg(long, conflicts_with = "fork")]
    fresh: bool,
    /// Branch off the latest conversation, leaving it as it is.
    #[arg(long)]
    fork: bool,
    /// Whether a new run gets its own git worktree.
    #[arg(long, value_enum)]
    worktree: Option<WorktreeArg>,
    /// Extra context for a fresh run, e.g. from an editor.
    #[arg(long, hide = true)]
    context_file: Option<PathBuf>,
    /// The agent for a new conversation (default: $VARDE_AGENT, else `agent`
    /// in the config, else claude). A resume or fork is by the agent it continues.
    #[arg(long)]
    agent: Option<String>,
    /// The project of a first run, for a task deferred or without one.
    #[arg(long)]
    project: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum WorktreeArg {
    Always,
    Never,
}

impl StartArgs {
    fn options(self) -> Result<start::Options> {
        let context = match &self.context_file {
            Some(f) => Some(std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?),
            None => None,
        };
        Ok(start::Options {
            want: if self.fresh {
                Want::Fresh
            } else if self.fork {
                Want::Fork
            } else {
                Want::Continue
            },
            worktree: self.worktree.map(|w| matches!(w, WorktreeArg::Always)),
            context,
            agent: self.agent,
            project: self.project,
        })
    }
}

/// Where varde keeps its state: `$VARDE_HOME`, else the XDG state dir.
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn from_env() -> Paths {
        // Empty is unset: an empty path would put the store in the current directory.
        let home = std::env::var_os("VARDE_HOME").filter(|h| !h.is_empty());
        let home = home.map(PathBuf::from).unwrap_or_else(|| {
            let state = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
                });
            state.join("varde")
        });
        Paths { home }
    }

    pub fn db(&self) -> PathBuf {
        self.home.join("varde.db")
    }

    pub fn pending(&self) -> PathBuf {
        self.home.join("pending.jsonl")
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("varde.log")
    }

    /// Make the store's directory if it is missing, readable by you
    /// alone: it holds your prompts and notes.
    pub fn make_home(&self) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.home)
    }

    /// Open the store, creating it on first use, and apply any events a
    /// hook could not write directly.
    pub fn open(&self, busy: Duration) -> Result<Store> {
        self.make_home()?;
        let mut store = Store::open(&self.db(), busy)?;
        // Events left pending never stop a command: what this drain could
        // not apply waits for the next one, or for `doctor`.
        let _ = pending::drain(self, &mut store);
        Ok(store)
    }
}

/// The task `task` names, or else the one the session this runs in is
/// working on: an agent told to close its task knows only its
/// `VARDE_RUN`. That is the newest run of the session's process, which
/// `/resume` may have moved to another task. With neither, the usage
/// error `subcommand` would give for a missing task.
fn own_task(paths: &Paths, task: Option<String>, subcommand: &str) -> Result<String> {
    if let Some(t) = task {
        return Ok(t);
    }
    let Some(run) = run_env() else {
        use clap::CommandFactory;
        let mut cli = Cli::command();
        cli.build();
        let sub = cli.find_subcommand_mut(subcommand).expect("a subcommand of varde");
        let missing = "the following required arguments were not provided:\n  <TASK>";
        sub.error(clap::error::ErrorKind::MissingRequiredArgument, missing).exit()
    };
    let store = paths.open(Duration::from_secs(2))?;
    store
        .conn
        .query_row(
            "SELECT task FROM run WHERE task IS NOT NULL AND (id = ?1 OR (pid > 0
               AND (pid, coalesce(pid_start, 0)) = (SELECT pid, coalesce(pid_start, 0) FROM run WHERE id = ?1)))
             ORDER BY started DESC, rowid DESC LIMIT 1",
            [&run],
            |r| r.get(0),
        )
        .with_context(|| format!("VARDE_RUN names run {run}, which has no task; name the task"))
}

/// The run a session varde launched or tracks carries (`VARDE_RUN`).
pub fn run_env() -> Option<String> {
    std::env::var("VARDE_RUN").ok().filter(|r| !r.is_empty())
}

/// The project the current directory is in, or `None` for every one.
pub fn scope(store: &Store, all: bool) -> Result<Option<String>> {
    if all {
        return Ok(None);
    }
    Ok(Some(varde_core::project::of(&store.conn, &std::env::current_dir()?).display().to_string()))
}

/// Text given on the command line, or read from stdin for `-`.
pub fn arg_or_stdin(text: Option<String>) -> Result<Option<String>> {
    if text.as_deref() != Some("-") {
        return Ok(text);
    }
    Ok(Some(std::io::read_to_string(std::io::stdin())?))
}

/// Each earlier version of a task's or a project's cairn, and why it changed.
pub fn print_history(versions: &[varde_core::memory::Version]) {
    for v in versions {
        println!("\n-- {} by {} --", v.ts, v.author);
        for line in v.cairn.render().lines().chain(v.reason.iter().flat_map(|r| r.lines())) {
            println!("  {line}");
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// This binary, to start again. Linux names a running binary that has
/// been replaced, as by an upgrade, `<path> (deleted)`: then the new one
/// at that path, else `varde` on PATH.
pub fn exe() -> PathBuf {
    std::env::current_exe().map_or_else(|_| "varde".into(), resolve_exe)
}

/// Start `varde <args>` in its own session, so it outlives the caller and
/// the terminal it ran in. `what` names it in an error.
pub fn detach(args: &[&str], what: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe());
    cmd.args(args)
        .env("VARDE_INTERNAL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().with_context(|| format!("starting {what}"))?;
    // Reap it when it ends, so a long-lived caller (the TUI) does not
    // collect zombies; a short-lived one exits without waiting.
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn resolve_exe(p: PathBuf) -> PathBuf {
    if p.exists() {
        return p;
    }
    match p.to_str().and_then(|s| s.strip_suffix(" (deleted)")).map(PathBuf::from) {
        Some(new) if new.exists() => new,
        _ => "varde".into(),
    }
}

fn main() -> ExitCode {
    // Read leniently: an argument that is not UTF-8 is clap's to refuse.
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    // `varde show … | head` ends quietly, as other commands do. Not where
    // a closed pipe must not stop the work: the hook, the cairn job (which
    // writes to the model), a session, or the board.
    if !matches!(args.get(1).map(String::as_str), None | Some("hook" | "job" | "run")) {
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    let paths = Paths::from_env();
    // `varde hook [--agent NAME] <Event>` records one agent event, read as
    // JSON from stdin. It must print JSON and exit 0 whatever it is given,
    // so it is read before clap, which would exit 2 on what it does not
    // know.

    if args.get(1).map(String::as_str) == Some("hook") {
        let (agent, event) = hook::args(&args[2..]);
        hook::run(&paths, &agent, &event);
        return ExitCode::SUCCESS;
    }
    let cli = Cli::parse();
    let result = dispatch(&paths, cli.cmd);
    // Leave with the agent's own exit code.
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("varde: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(paths: &Paths, cmd: Option<Cmd>) -> Result<i32> {
    let Some(cmd) = cmd else { return tui::run(paths).map(|_| 0) };
    let result = match cmd {
        Cmd::List { json, all } => list::run(paths, json, all),
        Cmd::Doctor { json } => doctor::run(paths, json),
        Cmd::Eval { all, since, json } => eval::run(paths, all, since.as_deref(), json),
        Cmd::New { title, project, body, external, from, json } => {
            tasks::new(paths, &title, project, body, external, from, json)
        }
        Cmd::Accept { tasks } => tasks::accept(paths, &tasks),
        Cmd::Link { task, external } => tasks::link(paths, &task, &external),
        Cmd::Unlink { task } => tasks::unlink(paths, &task),
        Cmd::Show { project: Some(dir), history, json, .. } => {
            memory_cmd::show_project(paths, &dir, history, json)
        }
        Cmd::Show { task, history, no_git, json, .. } => {
            tasks::show(paths, &own_task(paths, task, "show")?, history, !no_git, json)
        }
        Cmd::Seen { task } => tasks::seen(paths, &task),
        Cmd::Note { run, text } => memory_cmd::note(paths, run, &text),
        Cmd::Recall { words, all, json } => memory_cmd::recall(paths, &words, all, json),
        Cmd::Edit { project: Some(dir), stdin, print, .. } => {
            memory_cmd::edit_project(paths, &dir, stdin, print).map(drop)
        }
        Cmd::Edit { task, stdin, print, .. } => {
            memory_cmd::edit(paths, &task.unwrap_or_default(), stdin, print).map(drop)
        }
        Cmd::Set { task, title, body, project } => tasks::set(paths, &task, title, body, project),
        Cmd::Job { id } => job::run(paths, &id),
        Cmd::Start { task, how, json } => how.options().and_then(|o| start::start(paths, &task, o, json)),
        Cmd::Attach { task, how } => return how.options().and_then(|o| start::attach(paths, &task, o)),
        Cmd::Done { task, remove, force, keep_running, json } => done::done(
            paths,
            &own_task(paths, task, "done")?,
            done::Options { remove, force, discard: false, keep_running },
            json,
        ),
        Cmd::Park { task, keep_running, json } => {
            done::park(paths, &own_task(paths, task, "park")?, keep_running, json)
        }
        Cmd::Drop { task, json } => done::drop(paths, &task, json),
        Cmd::Reopen { task } => done::reopen(paths, &task),
        Cmd::Run { run } => return run::run(paths, &run),
        Cmd::Reap { task, spare } => done::reap(paths, &task, &spare),
        Cmd::GitRefresh => sweep::run(paths),
    };
    result.map(|()| 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_binary_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("varde");
        std::fs::write(&bin, "").unwrap();
        assert_eq!(resolve_exe(bin.clone()), bin);
        let deleted = PathBuf::from(format!("{} (deleted)", bin.display()));
        assert_eq!(resolve_exe(deleted.clone()), bin);
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(resolve_exe(deleted), PathBuf::from("varde"));
    }
}
