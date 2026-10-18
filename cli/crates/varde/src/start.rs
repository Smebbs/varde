//! `varde start` and `varde attach`: decide what starting a task means,
//! prepare the directory, and record the run before anything launches.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use varde_core::plan::{self, Plan, RunFacts, Session, Tree, Want, WorktreeFacts};
use varde_core::store::{self, Store};
use varde_core::{memory, project, task};

use crate::{config, git, now_ms, process, run, tmux, Paths};
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub struct Options {
    pub want: Want,
    /// `Some(true)` for a worktree, `Some(false)` for none, `None` to ask.
    pub worktree: Option<bool>,
    pub context: Option<String>,
    /// The agent asked for; `None` for the default. A resume or fork is
    /// always by the agent whose conversation it continues.
    pub agent: Option<String>,
    /// The project a first run is in, where the task's own is unsure.
    pub project: Option<PathBuf>,
    /// Stop a session its schedule started in the background, and carry
    /// it on here: the answer to [`Ask::TakeOver`].
    pub take_over: bool,
    /// Continue this run's conversation rather than the newest: its id,
    /// or the end of it `varde show` prints.
    pub run: Option<String>,
}

/// The agent a fresh run uses when none is asked for.
pub fn default_agent() -> String {
    std::env::var("VARDE_AGENT")
        .ok()
        .filter(|a| !a.is_empty())
        .or_else(|| config::get().agent.clone())
        .unwrap_or_else(|| "claude".into())
}

/// The command that runs `agent`, as `varde run` starts it but with none
/// of varde's arguments: `$VARDE_CLAUDE` and the like, else the config's
/// command, else the agent's own name.
pub fn agent_command(agent: &dyn varde_agent::Agent) -> Vec<String> {
    config::get().launch(agent.name(), vec![agent.program()])
}

/// `varde agent`: the agent of a new conversation and its command, for a
/// client that opens a session of your own (`docs/clients.md`).
pub fn agent(as_json: bool) -> Result<()> {
    let name = default_agent();
    let Some(adapter) = varde_agent::by_name(&name) else {
        bail!("unknown agent {name:?}: set agent to \"claude\" or \"pi\" in the config (or $VARDE_AGENT)");
    };
    let command = agent_command(&*adapter);
    if as_json {
        println!("{}", json!({"v": 1, "agent": name, "command": command}));
    } else {
        let quoted: Vec<String> = command
            .iter()
            .map(|a| {
                let plain =
                    !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
                if plain {
                    a.clone()
                } else {
                    format!("'{}'", a.replace('\'', r"'\''"))
                }
            })
            .collect();
        println!("{}", quoted.join(" "));
    }
    Ok(())
}

pub enum Outcome {
    /// Already running in a terminal varde launched or knows. `fork` is
    /// the run `--fork` would branch off, if any.
    Focus { run: String, fork: Option<String> },
    /// A run is recorded and ready for `varde run`. `notices` are what
    /// preparing it did that the user should hear about, such as a
    /// worktree restored; the caller shows them, since a TUI owns its screen.
    Ready { run: String, cwd: PathBuf, notices: Vec<String> },
    /// A first run needs an answer only the user can give, and there was
    /// no terminal to ask on. Starting again with it carries on.
    Ask(Ask),
}

pub enum Ask {
    /// `--project`: the task's project is unsure. `choices` are the
    /// likely ones, its own guess first.
    Project { choices: Vec<String> },
    /// `--worktree`: whether a first run in a repository gets one.
    Worktree,
    /// `--take-over`: the task's session is working in the background,
    /// started by its schedule, where nobody can go to it.
    TakeOver { run: String },
}

impl Ask {
    pub fn message(&self) -> String {
        match self {
            Ask::Project { choices } if !choices.is_empty() => {
                format!(
                    "this task's project is unsure: pass --project <dir>, likely {}",
                    choices.join(" or ")
                )
            }
            Ask::Project { .. } => "this task's project is unsure: pass --project <dir>".into(),
            Ask::Worktree => {
                "this task has no worktree yet: pass --worktree always or --worktree never".into()
            }
            Ask::TakeOver { .. } => {
                "this task is working in the background: pass --take-over to stop it and carry it on here"
                    .into()
            }
        }
    }
}

pub fn start(paths: &Paths, reference: &str, opts: Options, as_json: bool) -> Result<()> {
    let mut store = paths.open(Duration::from_secs(2))?;
    let id = task::resolve(&store, reference)?;
    match prepare(&mut store, &id, opts, false)? {
        Outcome::Focus { run, fork } => {
            // The client is sent to the live session: that is looking at it.
            store::mark_seen(&store.conn, &id, now_ms())?;
            let tty = tty_of(&store, &run)?;
            if as_json {
                println!("{}", json!({"v": 1, "action": "focus", "run": run, "tty": tty}));
            } else {
                println!("{}", running(&id, &run, fork.as_deref(), &whereabouts(&id, tty.as_deref()).1));
            }
        }
        Outcome::Ask(ask) if as_json => {
            let out = match ask {
                Ask::Project { choices } => {
                    json!({"v": 1, "action": "ask", "ask": "project", "choices": choices})
                }
                Ask::Worktree => json!({"v": 1, "action": "ask", "ask": "worktree"}),
                Ask::TakeOver { run } => json!({"v": 1, "action": "ask", "ask": "takeover", "run": run}),
            };
            println!("{out}");
        }
        Outcome::Ask(ask) => bail!(ask.message()),
        Outcome::Ready { run, cwd, notices } => {
            for n in notices {
                eprintln!("varde: {n}");
            }
            let command = format!("varde run {}", crate::memory_cmd::short_run(&store.conn, &run));
            if as_json {
                println!("{}", json!({"v": 1, "action": "run", "run": run, "cwd": cwd, "command": command}));
            } else {
                println!("cd {}\n{command}", cwd.display());
            }
        }
    }
    Ok(())
}

/// `start` and `run` in one step, for a terminal.
pub fn attach(paths: &Paths, reference: &str, opts: Options) -> Result<i32> {
    let mut store = paths.open(Duration::from_secs(2))?;
    let id = task::resolve(&store, reference)?;
    let interactive = unsafe { libc::isatty(0) } == 1;
    let mut outcome = prepare(&mut store, &id, opts.clone(), interactive)?;
    if let Outcome::Ask(Ask::TakeOver { .. }) = outcome {
        let q = format!(
            "{} is working in the background. Take it over? It stops, and carries on here.",
            task::short(&id)
        );
        if interactive && ask(&q)? {
            outcome = prepare(&mut store, &id, Options { take_over: true, ..opts }, interactive)?;
        }
    }
    match outcome {
        Outcome::Focus { run, fork } => {
            let (pane, place) = whereabouts(&id, tty_of(&store, &run)?.as_deref());
            // Inside tmux, go there, as the board does.
            if let Some((t, pane)) = tmux::here().zip(pane) {
                if t.go(&pane).is_ok() {
                    store::mark_seen(&store.conn, &id, now_ms())?;
                    eprintln!("varde: {} is already running; went to {place}", task::short(&id));
                    return Ok(0);
                }
            }
            bail!(running(&id, &run, fork.as_deref(), &place))
        }
        Outcome::Ask(ask) => bail!(ask.message()),
        Outcome::Ready { run, notices, .. } => {
            for n in notices {
                eprintln!("varde: {n}");
            }
            drop(store);
            run::run(paths, &run)
        }
    }
}

/// The terminal `run`'s session is in, if known.
pub fn tty_of(store: &Store, run: &str) -> Result<Option<String>> {
    Ok(store.conn.query_row("SELECT tty FROM run WHERE id = ?1", [run], |r| r.get(0))?)
}

/// Where `task`'s live session is: its tmux pane, if tmux has it, and
/// the place to name, such as `tmux window work:2 k2f0g3 Fix it` or
/// `/dev/pts/7`.
fn whereabouts(task: &str, tty: Option<&str>) -> (Option<String>, String) {
    let t = tmux::here().unwrap_or_else(tmux::server);
    let pane =
        t.panes().ok().and_then(|ps| tmux::find(&ps, task, tty).map(|p| (p.id.clone(), p.window.clone())));
    match (pane, tty) {
        (Some((id, window)), _) => (Some(id), format!("tmux window {window}")),
        (None, Some(tty)) => (None, tty.to_string()),
        (None, None) => (None, "another terminal".to_string()),
    }
}

/// `task k2f0g3 is already running in …`, and what `--fork` would do.
fn running(task: &str, run: &str, fork: Option<&str>, place: &str) -> String {
    let short = task::short(task);
    let fork = match fork {
        Some(f) => format!("varde attach {short} --fork branches off run {}", task::short(f)),
        None => format!("varde attach {short} --fresh starts another conversation"),
    };
    format!("task {short} is already running in {place} (run {}); {fork}", task::short(run))
}

/// How long a run recorded by `start` and not launched is still the one
/// starting the task again gives.
const UNLAUNCHED_MS: i64 = 60_000;
/// How long a worktree's setup may run before the run counts as gone.
const SETUP_MS: i64 = 10 * 60_000;

pub fn prepare(store: &mut Store, task_id: &str, opts: Options, interactive: bool) -> Result<Outcome> {
    // Planned again once a background session it takes over has stopped.
    let again = Options { take_over: false, ..opts.clone() };
    let (title, body, project, origin): (String, Option<String>, Option<String>, Option<String>) = store
        .conn
        .query_row("SELECT title, body, project, origin FROM task WHERE id = ?1", [task_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    // The newest run as this start found it, read before anything else:
    // another start that records one after this is seen at the insert.
    // Read later, a run recorded just after the check below would pass
    // for an old one, and two runs be planned.
    let seen: Option<String> = store
        .conn
        .query_row(
            "SELECT id FROM run WHERE task = ?1 ORDER BY started DESC, rowid DESC LIMIT 1",
            [task_id],
            |r| r.get(0),
        )
        .optional()?;
    // A run recorded moments ago and not launched yet is what a second
    // start is after too: a double RET, or Emacs and a terminal at once.
    // Planning another would put two agents on one tree, so it gets the
    // same run, which `varde run` launches only once. Not once the task
    // was closed since, which may have taken its worktree, nor when the
    // second start asks for something the run is not: it is planned as
    // any start is.
    if opts.want == Want::Continue && opts.run.is_none() {
        let waiting: Option<(String, String, String, Option<String>)> = store
            .conn
            .query_row(
                "SELECT id, cwd, agent, prompt FROM run
                 WHERE id = (SELECT id FROM run WHERE task = ?1 ORDER BY started DESC, rowid DESC LIMIT 1)
                   AND launch = 'managed' AND pid IS NULL AND exit_code IS NULL AND started > ?2
                   AND (SELECT state FROM task WHERE id = ?1) = 'open'",
                params![task_id, now_ms() - UNLAUNCHED_MS],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((run, cwd, agent, prompt)) = waiting
            .filter(|w| Path::new(&w.1).is_dir() && in_project(store, task_id, project.as_deref(), &w.1))
        {
            let tree = crate::sweep::active_worktree(store, task_id)?.map(|w| w.0);
            let in_tree = tree.is_some_and(|t| Path::new(&cwd).starts_with(t));
            let same = opts.agent.as_ref().is_none_or(|a| *a == agent)
                && opts.worktree.is_none_or(|w| w == in_tree)
                && (opts.context.is_none()
                    || prompt == Some(preamble(&title, body.as_deref(), opts.context.as_deref())))
                && opts.project.as_ref().is_none_or(|d| {
                    project_of(store, d).is_ok_and(|d| project.as_deref() == Some(&*d.to_string_lossy()))
                });
            if same {
                return Ok(Outcome::Ready { run, cwd: PathBuf::from(cwd), notices: vec![] });
            }
        }
    }
    // Two starts at once (Emacs and a terminal, two `attach`es) both plan
    // from what they read: the one that finds a run the other recorded
    // since takes it, as a start moments later would, and says so.
    let continuing = opts.want == Want::Continue;
    let raced = |conn: &rusqlite::Connection| -> Result<Option<Outcome>> {
        if !continuing {
            return Ok(None);
        }
        let run: Option<(String, String)> = conn
            .query_row(
                "SELECT id, cwd FROM run WHERE task = ?1 AND launch = 'managed'
                 AND id = (SELECT id FROM run WHERE task = ?1 ORDER BY started DESC, rowid DESC LIMIT 1)",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .filter(|(id, _)| Some(id) != seen.as_ref());
        Ok(run.map(|(run, cwd)| Outcome::Ready {
            run,
            cwd: PathBuf::from(cwd),
            notices: vec!["another start of this task got there first; this is its session".into()],
        }))
    };
    let started: bool =
        store.conn.query_row("SELECT EXISTS (SELECT 1 FROM run WHERE task = ?1)", [task_id], |r| r.get(0))?;
    // Deferred work took its project from the session it came from, and a
    // task without one was written down anywhere: the first run asks. Not
    // once another was named for the deferred work (`new --from --project`,
    // `set --project`).
    let unsure = !started && project_unsure(store, origin.as_deref(), project.as_deref())?;
    let chosen = match opts.project {
        Some(_) if started => {
            bail!(
                "a task keeps the project it started in; varde set {} --project <dir> moves it",
                task::short(task_id)
            )
        }
        Some(dir) => Some(project_of(store, &dir)?),
        None if unsure => {
            if !interactive {
                return Ok(Outcome::Ask(Ask::Project { choices: projects(store, project.as_deref())? }));
            }
            Some(ask_project(store, &projects(store, project.as_deref())?)?)
        }
        None => None,
    };
    let project = chosen.as_ref().map(|p| p.display().to_string()).or(project);
    // A repository's own settings win over the user's.
    let mut repo_cfg = match &project {
        Some(p) if Path::new(p).is_dir() => config::Repo::read(Path::new(p))?,
        _ => config::Repo::default(),
    };
    let opts_worktree = opts.worktree;
    let worktree = opts.worktree.or_else(|| repo_cfg.worktree.or(config::get().worktree.default)?.want());
    // A task moved to another project leaves its conversations in the
    // old one: a start there would run in the old checkout.
    let mut runs = run_facts(store, task_id)?;
    runs.retain(|r| r.cwd.as_deref().is_none_or(|c| in_project(store, task_id, project.as_deref(), c)));
    // Opening a run continues its conversation, not the newest; asked for
    // none, that of the newest result nobody has read, unless a session
    // is live to go to.
    let target = match &opts.run {
        Some(r) => Some(crate::memory_cmd::resolve_run(&store.conn, r)?),
        None if opts.want == Want::Continue && !runs.iter().any(|r| r.live()) => {
            store::unread_results(&store.conn, task_id)?.into_iter().next()
        }
        None => None,
    };
    let opened = match &target {
        Some(t) => conversation(store, task_id, t)?,
        None => vec![],
    };
    if target.is_some() {
        runs.retain(|r| opened.contains(&r.id));
    }
    let wt = worktree_facts(store, task_id)?;
    let Plan { session, tree } = plan::plan(&runs, wt.as_ref().map(|w| &w.0), opts.want, worktree);

    // A session its schedule started in the background has no terminal to
    // go to: it is taken over, stopped and carried on here, or asked about.
    if let Session::Focus { run } = &session {
        let background = crate::done::background(store, run)?;
        if background && !opts.take_over {
            return Ok(Outcome::Ask(Ask::TakeOver { run: run.clone() }));
        }
        if background {
            crate::done::stop_run(store, run)?;
            return prepare(store, task_id, again, interactive);
        }
    }
    // Only opening a run reads what it found.
    let now = now_ms();
    for run in &opened {
        store::read_result(&store.conn, run, now)?;
    }
    if let Session::Focus { run } = session {
        // Starting a task picks it up, even when that is only going to
        // the session it kept. The caller marks it seen once it gets there.
        reopen(&store.conn, task_id)?;
        store.conn.execute("UPDATE task SET inbox = 0 WHERE id = ?1", [task_id])?;
        let fork = match plan::plan(&runs, wt.as_ref().map(|w| &w.0), Want::Fork, worktree).session {
            Session::Fork { run, .. } => Some(run),
            _ => None,
        };
        return Ok(Outcome::Focus { run, fork });
    }

    // Known before anything is made, so a bad name leaves nothing behind.
    let agent_name = match &session {
        Session::Resume { run, .. } | Session::Fork { run, .. } => {
            let theirs: String =
                store.conn.query_row("SELECT agent FROM run WHERE id = ?1", [run], |r| r.get(0))?;
            if let Some(asked) = opts.agent.as_deref().filter(|a| *a != theirs) {
                bail!("the task's latest session is {theirs}'s; pass --fresh to start one with {asked}");
            }
            theirs
        }
        _ => opts.agent.clone().unwrap_or_else(default_agent),
    };
    let agent = varde_agent::by_name(&agent_name)
        .with_context(|| format!("varde does not know the agent {agent_name}: it runs claude or pi"))?;

    let project = project.map(PathBuf::from);
    let here = match &project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let mut notices: Vec<String> = repo_cfg.warning.take().into_iter().collect();
    // Outside a repository there is no worktree to make, so none is asked about.
    let tree = match tree {
        Tree::Ask | Tree::Create if !project::root(&here).join(".git").exists() => {
            if tree == Tree::Create {
                notices
                    .push(format!("{} is not a git repository; running without a worktree", here.display()));
            }
            Tree::Root
        }
        // Nor in one with no commits, which a worktree cannot start from,
        // unless the worktree was asked for here rather than by a default.
        Tree::Ask | Tree::Create
            if (tree == Tree::Ask || opts_worktree.is_none()) && !git::has_commits(&here) =>
        {
            notices.push(format!("{} has no commits yet; running without a worktree", here.display()));
            Tree::Root
        }
        Tree::Ask if interactive => {
            if ask(&worktree_question(&title, project.as_deref(), crate::list::columns().saturating_sub(7)))?
            {
                Tree::Create
            } else {
                Tree::Root
            }
        }
        Tree::Ask => return Ok(Outcome::Ask(Ask::Worktree)),
        t => t,
    };
    // Questions take a while: another start may have recorded its run.
    if let Some(outcome) = raced(&store.conn)? {
        return Ok(outcome);
    }
    let cwd = match &tree {
        Tree::Use(p) => PathBuf::from(p),
        Tree::Root | Tree::Ask => here,
        Tree::Create => {
            let repo = project.as_deref().context("the task has no project to make a worktree in")?;
            let cfg = config::get();
            let layout = git::Layout {
                dir: cfg.worktree_dir(&git::main_checkout(repo)),
                prefix: cfg.branch_prefix().to_string(),
                base: repo_cfg.base.clone(),
            };
            let slug = git::slug(&title, &task::short(task_id));
            // Made again where it was, a worktree is on the branch it had.
            let before = wt
                .as_ref()
                .map(|(w, _, _)| w)
                .filter(|w| Path::new(&w.path).file_name().is_some_and(|n| n == slug.as_str()))
                .map(|w| w.branch.as_str());
            let new = match git::create(repo, &slug, &layout, before) {
                Ok(new) => new,
                // Being made by a start racing this one: once there, it is
                // reused, and the run that start records is taken below.
                Err(e) => {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    match git::create(repo, &slug, &layout, before) {
                        Ok(new) => new,
                        Err(_) => return raced(&store.conn)?.ok_or(e),
                    }
                }
            };
            notices.extend(new.notices);
            // A task that had this worktree before keeps its row, made new.
            store.conn.execute(
                "INSERT INTO worktree (path, task, repo, branch, base, state, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6)
                 ON CONFLICT (path) DO UPDATE SET task = ?2, repo = ?3, branch = ?4, base = ?5,
                   state = 'active', created = ?6, closed = NULL",
                params![
                    new.path.to_string_lossy(),
                    task_id,
                    repo.to_string_lossy(),
                    new.branch,
                    new.base,
                    now_ms()
                ],
            )?;
            if let (true, Some(setup)) = (new.made, &repo_cfg.setup) {
                git::mark_setup(&new.path, setup)?;
            }
            new.path
        }
        Tree::Restore { path, branch } => {
            let (repo, base) = match &wt {
                Some((_, repo, base)) => (repo.clone(), base.clone()),
                None => bail!("no worktree record to restore {path} from"),
            };
            let made = git::restore(Path::new(&repo), Path::new(path), branch, &base)?;
            store
                .conn
                .execute("UPDATE worktree SET state = 'active', closed = NULL WHERE path = ?1", [path])?;
            // Only the directory the session ran in may be gone.
            if let Some(more) = made {
                notices.push(format!("restored worktree {path} on {branch}"));
                notices.extend(more);
                if let Some(setup) = &repo_cfg.setup {
                    git::mark_setup(Path::new(path), setup)?;
                }
            }
            // A conversation continues in the directory it ran in, which
            // may be below the worktree's root.
            let cwd = match &session {
                Session::Resume { cwd, .. } | Session::Fork { cwd, .. }
                    if Path::new(cwd).starts_with(path) =>
                {
                    PathBuf::from(cwd)
                }
                _ => PathBuf::from(path),
            };
            std::fs::create_dir_all(&cwd)?;
            cwd
        }
    };

    // A second agent beside a live one edits the same files.
    if !matches!(session, Session::Resume { .. })
        && runs.iter().any(|r| r.live() && r.cwd.as_deref().is_some_and(|c| same_tree(Path::new(c), &cwd)))
    {
        notices.push(format!("another session of this task is live in {}", cwd.display()));
    }

    // Empty until the first event says, for an agent that picks its own.
    let new_id = || agent.new_native_id().unwrap_or_default();
    let run = ulid::Ulid::new().to_string();
    // Memory reaches the session through its hooks (`deliver`): the first
    // prompt holds only what the client knows.
    let (native, parent, relation, prompt) = match &session {
        Session::Resume { run, .. } => (native_of(store, run)?, Some(run.clone()), "resume", None),
        Session::Fork { run, .. } => (new_id(), Some(run.clone()), "fork", None),
        _ => (new_id(), None, "fresh", Some(preamble(&title, body.as_deref(), opts.context.as_deref()))),
    };
    // A share of fresh sessions is held out of memory, to measure what it
    // is worth; decided here, since a hook never reads the config. Left
    // unset while no share is.
    let share = config::get().memory.holdout;
    let holdout = (relation == "fresh" && share > 0.0).then(|| memory::held_out(&run, share));
    let tx = store.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Checked again under the write lock, where it cannot be missed.
    if let Some(outcome) = raced(&tx)? {
        return Ok(outcome);
    }
    // A resume needs no cursor of its own: a fold starts from the furthest
    // any run on its transcript got (`memory::enqueue`).
    tx.execute(
        "INSERT INTO run (id, task, agent, native_id, parent_run, relation, cwd, started, launch, prompt, holdout)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'managed', ?9, ?10)",
        params![
            run,
            task_id,
            agent.name(),
            native,
            parent,
            relation,
            cwd.to_string_lossy(),
            now_ms(),
            prompt,
            holdout
        ],
    )?;
    // Starting a task picks it up: back from done, parked or dropped, and
    // out of the Inbox, since starting it is taking it on.
    reopen(&tx, task_id)?;
    tx.execute("UPDATE task SET inbox = 0 WHERE id = ?1", [task_id])?;
    if let Some(p) = &chosen {
        tx.execute("UPDATE task SET project = ?2 WHERE id = ?1", params![task_id, p.to_string_lossy()])?;
    }
    tx.commit()?;
    notices.extend(crate::doctor::unhooked(agent.name()));
    Ok(Outcome::Ready { run, cwd, notices })
}

/// The runs of `run`'s conversation: it and every run that continued it,
/// resumed or forked. `run` must be `task_id`'s.
fn conversation(store: &Store, task_id: &str, run: &str) -> Result<Vec<String>> {
    let all: Vec<(String, Option<String>)> = store
        .conn
        .prepare("SELECT id, parent_run FROM run WHERE task = ?1 ORDER BY started, rowid")?
        .query_map([task_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if !all.iter().any(|(id, _)| id == run) {
        bail!("run {} is not one of {}'s", task::short(run), task::short(task_id));
    }
    // Oldest first, so a parent is in before its children.
    let mut out = vec![run.to_string()];
    for (id, parent) in &all {
        if parent.as_ref().is_some_and(|p| out.contains(p)) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Whether a task's project is unsure before its first run: it has none,
/// or only the one it took from the task it was deferred from.
pub fn project_unsure(store: &Store, origin: Option<&str>, project: Option<&str>) -> Result<bool> {
    Ok(match (origin, project) {
        (_, None) => true,
        (Some(o), Some(p)) => store.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM task WHERE id = ?1 AND project = ?2)",
            [o, p],
            |r| r.get(0),
        )?,
        (None, Some(_)) => false,
    })
}

/// Back to Open from done, parked or dropped; whether it was any of them.
pub fn reopen(conn: &rusqlite::Connection, task_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE task SET state = 'open', closed = NULL WHERE id = ?1 AND state IN ('done', 'parked', 'dropped')",
        [task_id],
    )? > 0)
}

/// Whether a run of the task in `cwd` ran in `project`: in its checkout,
/// or in a worktree the task had of it.
fn in_project(store: &Store, task_id: &str, project: Option<&str>, cwd: &str) -> bool {
    let Some(project) = project else { return true };
    Path::new(cwd).starts_with(project)
        || store
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM worktree WHERE task = ?1 AND repo = ?2
                   AND (?3 = path OR substr(?3, 1, length(path) + 1) = path || '/'))",
                params![task_id, project, cwd],
                |r| r.get(0),
            )
            .unwrap_or(true)
}

/// Whether two directories are one checkout's, one below the other.
fn same_tree(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The project `dir` is in, refusing a directory that does not exist.
pub fn project_of(store: &Store, dir: &Path) -> Result<PathBuf> {
    let dir = dir.canonicalize().map_err(|_| anyhow::anyhow!("there is no directory {}", dir.display()))?;
    Ok(project::of(&store.conn, &dir))
}

fn native_of(store: &Store, run: &str) -> Result<String> {
    Ok(store.conn.query_row("SELECT native_id FROM run WHERE id = ?1", [run], |r| r.get(0))?)
}

/// What a fresh run is told first: the task as the client knows it.
fn preamble(title: &str, body: Option<&str>, context: Option<&str>) -> String {
    let mut out = title.to_string();
    for part in [body, context].into_iter().flatten().map(str::trim).filter(|p| !p.is_empty()) {
        out.push_str("\n\n");
        out.push_str(part);
    }
    out
}

pub fn run_facts(store: &Store, task_id: &str) -> Result<Vec<RunFacts>> {
    let mut stmt = store.conn.prepare(
        "SELECT id, cwd, transcript, launch, pid, pid_start, tty, started, exit_code IS NULL, tty_ctime FROM run
         WHERE task = ?1 ORDER BY started DESC, rowid DESC",
    )?;
    let rows = stmt.query_map([task_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, bool>(8)?,
            r.get::<_, Option<i64>>(9)?,
        ))
    })?;
    let now = now_ms();
    let mut out = Vec::new();
    for row in rows {
        let (id, cwd, transcript, launch, pid, pid_start, tty, started, open, tty_ctime) = row?;
        let status = store::run_status(&store.conn, &id)?;
        // `varde run` has claimed the run (pid 0) and runs the worktree's
        // setup before it launches the agent: live, for as long as a
        // setup may take. A process that ended this conversation, or moved
        // on from it to another (perhaps another task's), is not this
        // run's session.
        let setting_up = pid == Some(0) && open && now - started < SETUP_MS;
        let process_alive = (launch == "managed").then(|| {
            setting_up
                || (pid.is_some_and(|p| process::alive(p, pid_start.unwrap_or(0)))
                    && status != Some(varde_core::status::Status::Exited)
                    && !store::moved_on(&store.conn, &id).unwrap_or(false))
        });
        out.push(RunFacts {
            status: store::effective_status(
                status,
                pid,
                pid_start,
                tty.as_deref().map(|t| (t, tty_ctime)),
                &process::alive,
            ),
            process_alive,
            process_known: pid.is_some(),
            transcript_exists: transcript.as_deref().is_some_and(|t| Path::new(t).is_file()),
            cwd_exists: cwd.as_deref().is_some_and(|c| Path::new(c).is_dir()),
            tty,
            cwd,
            id,
        });
    }
    Ok(out)
}

/// The task's latest worktree, with its repo and base.
fn worktree_facts(store: &Store, task_id: &str) -> Result<Option<(WorktreeFacts, String, String)>> {
    Ok(store
        .conn
        .query_row(
            "SELECT path, branch, state, repo, base FROM worktree WHERE task = ?1
             ORDER BY created DESC LIMIT 1",
            [task_id],
            |r| {
                let path: String = r.get(0)?;
                let state: String = r.get(2)?;
                Ok((
                    WorktreeFacts {
                        exists: Path::new(&path).is_dir(),
                        path,
                        branch: r.get(1)?,
                        active: state == "active",
                    },
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )
        .optional()?)
}

/// The projects a task could be in: its own first, then the repos of
/// other tasks, latest first.
pub fn projects(store: &Store, own: Option<&str>) -> Result<Vec<String>> {
    let mut stmt = store.conn.prepare(
        "SELECT project FROM task WHERE project IS NOT NULL GROUP BY project ORDER BY MAX(created) DESC",
    )?;
    let known = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<String> = own.map(String::from).into_iter().collect();
    for p in known {
        if !out.contains(&p) && Path::new(&p).join(".git").exists() {
            out.push(p);
        }
    }
    Ok(out)
}

/// `varde projects`: [`projects`], one a line, or as JSON for a client's
/// project picker (`docs/clients.md`).
pub fn list_projects(paths: &Paths, as_json: bool) -> Result<()> {
    let known = projects(&paths.open(Duration::from_secs(2))?, None)?;
    if as_json {
        println!("{}", json!({"v": 1, "projects": known}));
    } else {
        known.iter().for_each(|p| println!("{p}"));
    }
    Ok(())
}

fn ask_project(store: &Store, choices: &[String]) -> Result<PathBuf> {
    // As the boards' picker lists them: by name, then directory.
    let width = choices.iter().map(|p| crate::list::project_name(Some(p)).width()).max().unwrap_or(0);
    for (i, p) in choices.iter().enumerate() {
        let name = crate::list::pad(crate::list::project_name(Some(p)), width + 2);
        eprintln!("  {}  {name}{}", i + 1, crate::tasks::tilde(p));
    }
    eprint!("Project for this task? [number or directory, default 1] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    let dir = match line.parse::<usize>() {
        _ if line.is_empty() => choices.first().context("no project to offer: type a directory")?.clone(),
        Ok(n) => choices.get(n.wrapping_sub(1)).with_context(|| format!("no project numbered {n}"))?.clone(),
        Err(_) => line.to_string(),
    };
    project_of(store, Path::new(&dir))
}

/// Whether to start the task titled `title` in a worktree of its own,
/// in `project`, in `width` columns, the title cut to fit:
/// `Start "Fix the login" in a worktree of its own (app)?`. The board,
/// the CLI and Emacs ask it in the same words.
pub fn worktree_question(title: &str, project: Option<&Path>, width: usize) -> String {
    let project =
        project.map(|p| format!(" ({})", crate::list::project_name(p.to_str()))).unwrap_or_default();
    let room = width.saturating_sub("Start \"\" in a worktree of its own?".len() + project.width()).max(12);
    let title = crate::list::clip(&varde_core::title::clean(title), room);
    format!("Start \"{title}\" in a worktree of its own{project}?")
}

/// Yes by default; asked again until the answer is a yes or a no, so
/// keys meant for something else never count as one.
fn ask(question: &str) -> Result<bool> {
    loop {
        eprint!("{question} [Y/n] ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            bail!("no answer; nothing was started");
        }
        match yes_or_no(&line) {
            Some(yes) => return Ok(yes),
            None => eprintln!("Answer y or n."),
        }
    }
}

fn yes_or_no(line: &str) -> Option<bool> {
    match line.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worktree_question_names_the_task_and_its_project() {
        assert_eq!(
            worktree_question("Fix the login\x1b[2J page", Some(Path::new("/src/app")), 80),
            "Start \"Fix the login[2J page\" in a worktree of its own (app)?"
        );
        let long = worktree_question("Make the cache key take the locale and the region", None, 60);
        assert_eq!(long, "Start \"Make the cache key take t…\" in a worktree of its own?");
        assert_eq!(long.width(), 60);
    }

    #[test]
    fn only_a_yes_or_a_no_answers() {
        assert_eq!(yes_or_no("\n"), Some(true));
        assert_eq!(yes_or_no("Yes\n"), Some(true));
        assert_eq!(yes_or_no(" n \n"), Some(false));
        assert_eq!(yes_or_no("nquit\n"), None);
        assert_eq!(yes_or_no("x"), None);
    }
}
