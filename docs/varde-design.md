# varde — design

varde is a standalone CLI that runs AI coding-agent sessions as tasks.
Each task keeps a small, bounded memory, its cairn, for the next
session to find. *Varde* is Norwegian for a cairn, the pile of stones
that marks a mountain trail for whoever comes next. Emacs and org-mode
are one client of it, and the reference integration for others.

The CLI is in `cli/`, and the Emacs client is `org-varde.el` and
`org-varde-board.el`. §12 lists the decisions behind it.

## 0. Summary

| | |
|---|---|
| **Core** | One Rust binary, `varde`. It owns tasks, runs, events, status, the attach decision, worktrees, memory, recall and doctor. |
| **State** | One SQLite database per machine. Status is derived and never stored. No daemon, and no sync. |
| **Agents** | Claude Code and pi, behind an adapter trait with an agent-neutral event vocabulary. Others can be added the same way. |
| **Liveness** | `varde run` is the parent process of the agent, and a hook finds the agent process of a session varde did not launch, so liveness is observed, not inferred. |
| **Memory** | One cairn per task, written by its updates, and one per project, written only by you (Q38): a fixed shape, hard caps, changed only by checked edits. It reaches every session through the agent's hooks, anchored bullets are checked against the code, and `varde eval` measures what it was worth. |
| **Standalone UI** | Plain commands with `--json`, plus a TUI board (`varde` with no arguments). |
| **Emacs** | A thin client. It renders a board varde has already grouped, and adds terminals, workspaces and the task view. Org meets varde through `varde:` links, which show where their tasks stand; nothing is synced, and org-varde writes nothing to org but the links you ask for (Q31). |
| **Clients** | The elisp is the reference integration, and `docs/clients.md` is the contract for others. |

## 1. Principles

- **One owner per fact.** varde owns every durable fact, and changes it
  only inside a transaction. Org holds only `varde:` links.
- **Status is derived** from events and process liveness, never stored.
- **Hooks never call the editor**, and nothing in a render path spawns
  a process.
- **No LLM in any interactive path.** Model calls run detached.
- **A recursion guard.** varde's own agent calls set `VARDE_INTERNAL=1`,
  which the hooks ignore.
- **Sessions are named before they start**: a run's id exists before its
  agent does, so nothing is matched up afterwards.
- **A first prompt is the signal that a session is real.**
- **The agent view is disabled for each launch**: a window varde opened
  holds one task's session, not a dashboard that starts sessions behind
  varde's back.
- **Branches are reused, never re-suffixed**, and a task's branch is
  followed through a rename (Q32).
- **A worktree is force-removed only when it is clean**, submodules
  included.
- **Word budgets, not sentence counts**, bound everything handed to a
  session.

## 2. Goals

1. **Reliable.** Every fact has exactly one owner and changes only
   inside a transaction.
2. **Efficient.** A hook takes under 10 ms at p99, the hooks that hand
   a session its memory under 20 ms, and `varde list --json` under
   15 ms at 1 000 tasks (Q35). No daemon.
3. **Standalone.** Fully usable in a plain terminal, with no Emacs or
   org.
4. **Adaptable.** Every command a client needs speaks `--json`, context
   can be passed in, and clients get the board pre-grouped.
5. **Emacs first-class.** org-varde is a full client, not a viewer.

Non-goals: MCP, orchestration, a daemon, LLM calls in any interactive
path, attaching a second front end to a live session, cross-machine
sync (§5), and embeddings (§7.4).

## 3. Architecture

```
 claude (hooks) ── varde hook <Event> ──► ingest ──┐
                                                   ├─► SQLite (~/.local/state/varde/varde.db, WAL)
 terminal shell ── varde run <run_id> ──► supervise┘        ▲
                   └─ child: claude …                       │
                                                            ├── varde list/show/recall … --json ◄── Emacs, others
 detached ──────── varde job <id> (haiku, memory update) ───┘         varde (TUI)            ◄── terminal users
```

- **Crates.** `clap`, `rusqlite` (bundled), `serde_json`, `ulid`,
  `anyhow`, `rustix`, and `ratatui` for the TUI. No async runtime:
  every long operation is a subprocess. Git is driven by shelling out
  to `git`.
- **Layout.** `varde-core` holds the store, the status machine, the
  attach decision, the board and memory (documents, update jobs,
  recall). `varde-agent` holds the trait and the Claude and pi
  adapters. `varde` holds the CLI, the hook, `varde run`, git,
  delivery, staleness, `eval` and the TUI. The logic is pure functions
  over the store, and tests never spawn the agent.
- **Paths.** The store is `$XDG_STATE_HOME/varde`
  (`~/.local/state/varde`); `$VARDE_HOME` overrides it, which is how
  tests are sandboxed.
- **Settings** (Q17). The user's live in `~/.config/varde/config.toml`
  (`$XDG_CONFIG_HOME`, or `$VARDE_CONFIG`): the agent and its command,
  memory updates and their model, the holdout, and where worktrees go
  and what their branches are called. A repository's live in its git
  config (`varde.worktree`, `varde.base`, `varde.setup`), so nothing is
  matched by path and nothing committed is ever run. A setting varde
  cannot read or does not know is ignored with a notice (and reported
  by `doctor`), so a typo never stops a session. Hooks read neither.
- **Environment.** Every process varde spawns sets `VARDE_INTERNAL=1`,
  so its own hooks do not track it. Sessions get `VARDE_RUN=<run_id>`,
  so `varde note` never has to guess an id.

## 4. Data model

| Entity | Key | Fields |
|---|---|---|
| **Task** | ULID | title, body, project, state (`open/done/dropped/parked`), inbox (bool), created, closed, changed (when its state, Inbox or link last changed, kept by a trigger), external (link, optional), origin (the task it was deferred from, optional), prompt_title (the title varde cut or the agent wrote, §6.2), named (when a model was asked to name it, §6.2) |
| **Run** | ULID | task, agent, native_id, parent_run, relation (`fresh/resume/fork`), launch (`managed` or `adopted`), cwd, pid + pid_start_time of the agent process, tty + its ctime (a closed pts number is soon reused), started, exit_code, cairn_cursor, holdout (§7.7), background (§6.5), done (§6.5) |
| **Event** | seq | run, kind (normalised), ts, raw payload, and on a turn end the sub-agents and shells it left running. Append-only. Aged out after 30 days by the memory jobs, a bounded batch at a time, apart from those a run's status still rests on. |
| **Worktree** | path | task, repo, branch, base, state (`active/removed/merged`) |
| **Cairn** | task | Status, `status_mine`, and version. Every version's text is kept in `cairn_history` with what changed. |
| **Item** | integer | a bullet (`cairn_item`) of a task's or project's cairn: section, position, text, anchor, mine, author and run, created, confirmed, state (`current` or `history`, with the reason it left and when), and whether its anchor resolved, and when that was checked. |
| **Project cairn** | project | Its version, each kept in `project_cairn_history` (§7.5). Its bullets are items. |
| **Note** | ULID | task, run, text, ts, folded (bool). Pinned input to the next update. |
| **Schedule** | task | rule (as typed), next_due, worktree (the answer a first start would ask for), fired, tried and error (§6.5) |
| **Job** | ULID | run, the transcript position it folds up to, kind (`fold` or `close`; `promote` was dropped by Q38), state, attempts, a beat while it waits on a model, its calls' tokens, and what its ADDs repeated (§7.7). Unique on (run, position, kind). Aged out after 30 days. |
| **Shown** | — | what each run was given (§7.3): kind, task, version, bullet or note ids. Aged out after 30 days, with the recall log. |

Rules:

- **Status is a pure function** of the run's recent events, whether its
  agent process is alive (pid plus start time, so a reused pid is not
  mistaken for it), and the task's state. The states are `working`,
  `delegated`, `waiting`, `blocked` and `exited`; done, parked and
  dropped are the task's, not a run's.
- **Events count in the order they happened.** A run's status is read
  from its newest event back to the last one that set it outright, by
  time and then arrival, on an index of (run, time). An event that
  waited in the pending file lands where it belongs. Only that tail is
  read, so aging out what lies before it changes nothing.
- **Every run's process is known when it can be** (Q25). `varde run`
  records the process it launches; when that is a wrapper, the first
  hook finds the agent below it and records that instead. For a session
  varde did not launch, the hook walks up to the nearest process with
  the agent's name (`claude` or `node`; `pi`, `node` or `bun`), past the
  hook shell and wrappers like `timeout`. With none found it records
  none, since a wrong one would end a live session when it exits. Once
  the process is gone the session has ended, whatever its last event
  said. `done`, `park` and `drop` stop only processes varde launched.
- **Parked is put aside, not done.** It keeps its worktree, its sessions
  are stopped as `done` stops them, and starting it reopens it.
- **The Inbox is what you did not create** (§6.2). Starting a task takes
  it out. It is never aged out: what you do not want, you drop.
- **Dropped is thrown away**, but its worktree's work is first kept
  under a ref (§6.3), so starting it again brings it back.
- **One resume or fork is one new Run row.** The Task never changes
  because of one, except that a closed task whose conversation is
  resumed is open again (§6.2).
- **History is append-only.** A bullet that leaves a document becomes
  history; notes and earlier versions are never edited in place.

## 5. Machines

Each machine is its own world. There is no sync and no shared store,
and rows record no host: worktree paths, pids, ttys and transcripts
only mean something where they happened. Only memory could travel, and
that would be an export or a remote `recall`, not a merged store. A
host on every row filtered the board by hostname, which macOS changes
with the network, and twice hid every task.

## 6. Commands

The same verbs serve people and scripts. `--json` output carries `"v"`,
the version of its shape: `list` and `show` are at 2, the rest at 1.
Usage is in `docs/guide.md`; what a client relies on is in
`docs/clients.md`. Commands only clients and agents use are left out of
`--help` (Q15).

| Command | Does |
|---|---|
| `varde` | The TUI board. Groups: Needs you, Working, Open, Scheduled, Inbox, Done and Parked (folded); every group is always shown, in that order (Q29). Done holds tasks closed today, and older ones whose work has not landed. Rows show their git stage (§6.3), `●` for unread, and `← <short>` of a deferred task's origin. RET suspends the board and attaches; inside tmux it opens the session in a window of its own (Q6). `--popup` quits once a key has gone to a session, so a tmux popup closes. Opened in a repository, it keeps to that project's rows and counts the rest (Q26). Background failures go to its message line, never close it. After a newer varde migrates the store, it re-execs as that varde. |
| `varde list [--all] [--project [DIR]] [--json]` | The board as data: rows already grouped, ordered and labelled. Every group is present (Q29). `--all` adds old done tasks, and dropped ones in a group of their own. A row's `busy` says a live session is mid-turn, so a client asks before closing it. |
| `varde new [<title>] [--project DIR] [--context -] [--external REF] [--when RULE]` | Creates a task in the checkout it runs in, a linked worktree of your own included (Q24). `--context` is the body. With no title, one is cut from the context and a model names the task, detached (§6.2, Q28). |
| `varde new <title> --from <run>` | Used by the agent: work deferred from the run's task, into the Inbox (§6.4). |
| `varde schedule <task> RULE`, `--off`, `--now`, `--install` | Starts the task at a time, or repeatedly (§6.5). `--install` puts in the heartbeat that runs `varde tick`. |
| `varde start <task> [--fresh\|--fork\|--run RUN] [--worktree always\|never] [--project DIR] [--context-file F] [--agent A] [--take-over] [--json]` | Reopens a closed task, makes the attach decision (§6.1), prepares the worktree and context, and records the run without launching it. It returns a plan: `run` (`{run, cwd, command}`), `focus` (`{run, tty}`), or `ask` when only the user can answer (worktree, project, take-over), and then nothing is changed (Q18). A resume or fork is always by the agent whose conversation it continues. Outside git, or in a repo with no commits, there is no worktree. |
| `varde run <run>` | Runs in the terminal's shell: forks the agent with the terminal inherited, records pid and exit code, forwards SIGTERM and waits. |
| `varde attach <task>` | `start` then `run`, for terminal users. It asks its own questions; on a running task it goes to its tmux pane, or says where it runs. |
| `varde next [--board]` | Inside tmux, goes to the window of the next task in Needs you, round to the first, and to the board once none is left. For a tmux key, so it reports in tmux's status line. Outside tmux it exits 1 and never touches the default tmux server. |
| `varde done [<task>] [--remove [--force]] [--keep-running] [--json]` | Closes the task and asks nothing (§6.3); on a repeating task it ends this run instead (§6.5, Q34). It never merges. It stops live runs `varde run` launched, before any worktree is removed: SIGTERM now, and a detached `varde reap` sends SIGKILL after 5 s. `--remove` waits for them and checks the worktree again. A session that closes its own task is left running (stopping it would kill the call), and its worktree is removed later by the refresh. Inside a session (`VARDE_RUN`), `done`, `park` and `show` with no task mean its own. |
| `varde park [<task>]` | Moves the task to Parked and stops its sessions. Its worktree is never touched. |
| `varde drop <task>` | Stops its sessions, keeps the work under `refs/varde/discarded/<slug>` (§6.3), and removes the worktree and branch. Anything already gone is not an error. |
| `varde reopen <task>` | Back to Open without starting it. |
| `varde accept <task>…` | Inbox tasks into Open. |
| `varde show [<task>] [--history] [--no-git] [--json]` | The task, its cairn first (Q30), each line with its id (`#12`), then its runs, lineage and git status. `--json` adds `usage` and `limits`, the caps a client counts against. Each run says why it has its status. `--no-git` skips git and `gh`. |
| `varde edit <task> --add SECTION TEXT \| --set ID TEXT \| --rm ID \| --restore ID` | Changes one cairn line, as the user's, in one transaction (§7.1, Q30). `--rm status` hands a user Status back to the updates. |
| `varde edit <task> [--print\|--stdin]` | The whole cairn, context above it, in `$EDITOR` or a client's editor (§7.1). |
| `varde show --project [DIR]`, `varde edit --project [DIR]` | The same for the project's cairn (§7.5). |
| `varde recall <words> [--all\|--project DIR]` | Searches memory, this project first (§7.4). |
| `varde eval [--since D] [--all]` | Reports what memory was worth (§7.7). Read-only. |
| `varde doctor` | Checks the store and schema, the `varde` on PATH, the config, the agent's command, each agent's hooks (Claude's plugin with every event this varde has; pi's extension), pending events, failed or stuck memory updates, stale bullets (counted, never failed), and worktrees whose directory is gone. Each failure says how to fix it. |
| `varde note <text>` | Used by the agent: pins a finding for the next update (§7.2). |
| `varde agent` | Used by clients: the agent and argv for a session of the user's own. |
| `varde set`, `link`, `unlink`, `seen` | Used by clients: change a title, context or project (`--project` is refused with an active worktree or live session), record or forget where a task also lives (linking accepts it), and mark a task's results read. |
| `varde hook [--agent A] <Event>` | Called by the plugin. Always prints a JSON object and exits 0. |

`varde job`, `reap`, `git-refresh`, `name` and `tick` are started
detached by varde itself (`tick` also by the heartbeat, §6.5).

**Unread.** Needs you lists unread rows first: a turn ended or a
question asked with no `seen` since. A client calls `seen` when you look
at the task's terminal, and `start` does whenever it plans `focus`; the
TUI and `attach` clear it once they get you there. In tmux, `varde run`
clears it while its pane is on screen. A `seen` is an event, so status
is unaffected. A background run's result is read only by opening that
run (§6.5).

### 6.1 The attach decision

`start` makes one decision, through one pure function:

```
plan(task, runs, worktrees, now) -> Focus(run) | Resume(run) | Fork(run) | Fresh
                                 + Reuse(path) | Restore(branch) | Create | None
```

Table tests cover every case; clients only carry out the plan. `Focus`
means "switch to where this run is": the TUI for the run it is
suspended on, or in tmux the pane on the run's tty (else the one tagged
`@varde_task`); Emacs through the buffer tagged with the run, or the
buffer on the run's pty for a session varde did not launch. Elsewhere
it names the window or tty.

Focus goes to the newest run that is live and reachable, even behind a
newer launch that died before writing anything; behind a newer run,
only one whose process varde knows counts. Once a session's process is
gone it is resumed, never focused or forked.

A run `start` recorded under a minute ago that no `varde run` has
launched yet is what starting again gives (unless another conversation,
agent, worktree choice, project or context is asked for), so a double
RET, or Emacs and a terminal at once, never puts two agents on one
tree. Simultaneous starts are settled under the store's write lock.

### 6.2 Ad-hoc sessions

A session started by typing `claude` in any repo becomes a Task on its
first prompt; being asked something separates it from the CLI's
pre-warmed workers. The title is the first prompt cut to about 8 words,
so the hook waits on no model.

A first prompt is often a greeting, so a model then names the task
(Q28): one small detached call to the agent and model the cairn updates
use, with varde's own system prompt and `VARDE_INTERNAL=1`. It reads the
title so far and the task's context or first prompt (cleaned, cut to
4 000 characters), and answers one line, kept only as a title is kept:
at most 8 words, nothing memory would refuse. `NO TITLE` or more than
one line gives none. A task `varde new` or the board makes is named by
a `varde name` started as it is made; one a hook made is named by the
next `varde git-refresh` (§6.3), at most three at a time.

Only titles varde wrote are renamed: an ad-hoc session's, one cut from
a context, deferred work's (§6.4), and one an edit cleared. A title you
typed, or a linked task's, never is. A task is asked about once:
`named` is set first by one conditional write, and a failure keeps the
cut title. The rename is conditional too, so a title you set meanwhile
wins; `prompt_title` keeps the cut one. varde never renames a branch
(§6.3). `memory.titles = false` or `memory.enabled = false` turns it
off. Every title is one line with no control characters.

The task sits in the **Inbox** until you act on it, and gets a cairn
like any other.

Scripted sessions are not recorded: Claude tells its hooks how it was
started in `CLAUDE_CODE_ENTRYPOINT` (`cli` in a terminal, `sdk-cli` for
`claude -p`, `sdk-ts`/`sdk-py` for the Agent SDK). The exception is a
background run of a schedule, which `varde run` marks with
`VARDE_BACKGROUND` (§6.5). A session started under another's claim
(`VARDE_RUN`) in a process of its own, like a `claude` a tool runs, is
recorded but never becomes a task. pi says nothing of the kind, so a
`pi -p` with the extension is tracked.

A conversation varde already has goes on in its task when it is resumed
outside varde (`claude --resume`, `claude -c`), reopening a closed task:
a live session is never hidden under Done or Parked.

A launched session that moves to another conversation (`/clear`,
`/resume`, pi's `/new` and `/fork`) keeps its claim, under a new id,
from the same process: a new run of the same task, its parent the
claimed run (a resume of a conversation another task has goes on there
instead). `varde run` follows it, watches the newest run of its process
for interrupts, and on exit ends and folds every open run of the
process. Only that newest run is the session's, for `start`, `done`,
`park` and `drop`.

Two things are kept apart:

- **Inbox: did you ask for this task?** An ad-hoc session or deferred
  work (§6.4). Accepting, linking, starting or closing it takes it out.
  Nothing leaves the Inbox or Open by itself; only Done empties itself.
- **Link: where else does the task live?** `external` names a *task
  source*, which then owns the title and body (Q1). org-varde no longer
  uses it (Q31); the core keeps it for other clients. Terminals host
  runs and never link a task.

### 6.3 Git status

A task's work lives in a **checkout** and must reach a **target**: its
worktree and the base, or, without one, the project checkout and its
upstream. `git::facts` works out, in two or three `git` calls:

- `ahead`, `behind`, `changes` (what `git status` lists), and
  `unpushed` against the upstream.
- `landed`: the branch's own commits are all in the base. A branch that
  has moved since it was cut (its reflog's first entry) and is 0 ahead
  has landed, and so has one whose PR merged at exactly its tip, which
  covers squash and rebase merges. So has one whose commits all reached
  the base as copies (`git cherry`), or whose whole diff is one commit
  there: work picked, rebased or squashed by hand. A commit after the
  merge is not landed.
- `pr` (number, state, draft, checks, link) through `gh pr view`, only
  for a pushed branch that is not the default. Any failure (10 s
  timeout) is just no PR.
- one `stage`, the earliest unfinished step: `edited`, `committed`,
  `pushed`, `pr`, `landed`, else `clean`.

`show` works it out when asked (`--no-git` skips it, since `gh` is a
network call). `done --remove` asks `gh` only with unmerged commits.

**On the board.** One worktree's git takes about 10 ms, past `list`'s
budget, so the board reads a cache, `git_state`: a stage, a label
(`committed`, `PR #12 ✓`) and when it was checked, per tracked task (an
active worktree, or open in a project checkout with a run). `list`
starts one detached `varde git-refresh` (one at a time) when a row was
checked before its task last changed or over 60 s ago; it writes all
rows in one transaction, and the store watch redraws the board.

**Closing.** Saying you are done and landing the work are separate,
since people land work in different ways. So `done` asks nothing: you
say whether you want the work (`done`) or not (`drop`), and varde
follows git.

- `done` removes a worktree whose work has landed at once. The
  refresher removes any other done task's worktree and branch once
  nothing can be lost: no changes, the work landed, no session
  running. Until then the task stays in
  Done with its stage shown, as the reminder. A worktree whose repo is
  gone is marked removed.
- A closed PR is not discarded, since it may be reopened: the task
  waits, showing `PR #12 closed`, until you drop it.
- `drop` keeps everything under `refs/varde/discarded/<slug>`: one
  commit with the branch as first parent, the worktree's files
  (uncommitted and untracked included) as its tree, and any earlier
  drop as a further parent. Starting again restores branch and files,
  and deletes the ref if it held nothing else.
- Ignored files are not kept, as `git worktree remove` treats them,
  except that a worktree is not removed while a `.worktreeinclude` file
  differs from the main checkout's (`done --remove` asks for `--force`).
- The project checkout is never cleaned up. It is yours.

**Making a worktree.** It is cut from the local base. Submodules are
not checked out: fetching every one again held a start up for minutes,
so the session fetches the ones it needs. Ignored files a committed
`.worktreeinclude` names (gitignore syntax, as Claude Code reads it)
are copied from the main checkout; a failure is reported, not fatal. An existing
worktree is reused, so a half-failed start can be run again. `git config
varde.setup` runs in the terminal before the agent, when the worktree
is made or brought back; a failure is reported and the session starts
anyway.

**The task's branch** (Q32). The branch a task's worktree has checked
out is the task's, whatever it is called: a branch made no earlier than
the worktree (its reflog's first entry, which a rename keeps) is
recorded as the task's, and the one it replaced is deleted once the new
one has all its commits. A branch that existed before the task stays
yours and is never deleted. The branch tracks its local base, so magit
and `git status` show the task's work as the commits ahead; such a
branch counts as unpushed with no PR, and a plain `git push` refuses
it, so nothing lands by accident. Sessions in the worktree are told to
commit on its branch, since agents left alone cut their own.

**In a worktree of your own** (Q24). A task made in a linked worktree
of yours belongs to that checkout: its worktree is cut from, and lands
in, your worktree's branch, which wins over `varde.base` (a detached
`HEAD` falls back to the base). It goes beside the others in the main
checkout's worktree directory, with `.worktreeinclude` files from yours.
A worktree varde made belongs to the repository it came from, which
only the store knows.

No LLM is involved: whether work can be lost is a question git answers
exactly. Landing it stays with the task's own session.

### 6.4 Deferred work

Harvesting follow-up TODOs from every session buries the real ones, so
varde files deferred work only on your word: the guidance (§8.1) tells
the agent to run `varde new --from <run>` for work you ask it to leave
for later, or agree to when it asks, never for work only it thought of.
No harvest, no dedup, no cap.

The task lands in the Inbox with `origin` set, in the origin's project.
That project is a guess, so its first start asks, offering it first
(Q18); so does a task with no project. Its first session is told the
origin and where it stands (§7.3).

### 6.5 Scheduled and repeating tasks

A schedule makes varde start a task at a time, as you would (Q33). It
runs in the background: `varde tick` runs `start --fresh` and then
`varde run` with no terminal, which launches the agent in print mode
(`claude -p`, `pi -p`), logs its output beside the store, and marks the
run `background`. From there it is any session: hooks track it (the
hook honours `VARDE_BACKGROUND` over Claude's `sdk-cli`), its status is
derived as any run's, and the guidance tells it nobody is watching.

- **The rule** (`schedule.rs`, pure functions over local time):
  `at [DAY|DATE] [HH:MM]` once, `every day|weekday|weekend|mon,thu
  [HH:MM]`, `every 30m|4h|2d|1w` from when it last fired, or an org
  timestamp (`+`/`++` keep to the base's slots, `.+` counts from the
  firing). A time left out is midnight: the first moment the machine is
  on that day. Kept as typed, with when it is next due.
- **Nobody is there when it fires,** so what a first start would ask
  (Q18) is settled when the schedule is made: the worktree answer is
  stored, and a task with an unsure project is refused.
- **No daemon.** `varde tick` fires what is due and exits. The
  heartbeat is a systemd user timer (`OnCalendar=minutely`,
  `Persistent`) or a LaunchAgent (`StartInterval 60`) written by
  `varde schedule --install`, which also keeps the sessions it starts
  alive (`KillMode=process`, `AbandonProcessGroup`); it holds no facts.
  A board's refresh starts a tick when a row is due, at most every
  30 s, and `doctor` says when no heartbeat runs.
- **First available time.** A start missed while asleep fires on wake,
  and missed slots coalesce, as in anacron: once, with the next slot the
  first after now.
- **Fresh from the cairn.** Every occurrence is a new conversation of
  the same task, given its cairn (§7.3). A tick waits while the task has
  a cairn update queued or running, so the next run gets the last one's
  lessons, and while a session of it is at work or was answered in the
  last 15 minutes; a session varde started that has been idle longer is
  stopped, its conversation kept. The cairn is the series' memory; what
  each run found is in its conversation.
- **Lifecycle.** No new state. Only open tasks fire, so `park` pauses
  the schedule, `drop` stops it, and `reopen` resumes it (at once if it
  came due). A one-shot is spent once it starts. `--now` starts the
  next run at once, to try a schedule out.
- **A start held up says why.** Past due by 5 minutes it is `late`; one
  held up by a session of it says `↻ due`, and `↻ late` after a day.
  `show` and `varde schedule TASK` say what holds it, derived as the
  tick decides.
- **`done` ends the run, not the series** (Q34). On a repeating task it
  stops the sessions, marks its runs read, and leaves the task open;
  a run its schedule started, stopped so, is marked `done`. On a board,
  marking unread runs nobody opened is asked first. With nothing live
  or unread it refuses, saying how to stop the series, and `--remove`
  is refused: the worktree is the series'. The series ends with its
  schedule (`schedule --off`, or clearing `When:`); then `done` closes
  the task. On a board, `d` between runs asks "stop repeating and close
  it?" and does both; `u` does not undo that.
- **Nothing to report.** A session that runs `varde done` on its own
  task sets its run's `done`, the one fact this adds: that run never
  needs you. It ends that run only. Its own run is the one it descends
  from, or the one its `VARDE_RUN` names from an agent's tool shell.
  From an agent's shell, `done`, `park` and `drop` never stop a session
  of yours, and a `done` varde cannot place is refused. The guidance
  tells a background run to do this only when it changed and found
  nothing, since anything it changed is for you to see; one that does
  not is in Needs you, so a mistake costs a look, never a finding.
- **A failure** to start leaves it due, says why (`late`, `show`,
  `doctor`), and is tried again after 10 minutes.
- **Its results are yours.** A background run that ended without
  `done`, and nobody has opened, is your turn, unread, in Needs you:
  derived from `ended`, `done` and any `seen`, never stored. Every such
  run counts, so a run that fires before you read the last buries
  nothing; the row says how many (`↻ 3 unread`). RET opens the newest,
  and only opening a run reads it. A run at work keeps the row in
  Working, saying what waits. varde shows no output of its own.
- **On the board** a scheduled task is in Scheduled, soonest first,
  showing when it starts (`↻ Mon 09:00`) until a session is in Needs
  you or Working, where `↻` says `d` ends this run. The view lists its
  sessions newest first (`●` unread, `quiet` ended with `done`, when
  you last resumed it), folding the quiet ones past the newest 10; RET
  on one continues it (`start --run`).
- **Taking it over.** While it works there is no terminal to go to, so
  `start` asks (`takeover`); `--take-over` stops it and plans again,
  which resumes it here. What the step under way had not written is
  lost.
- **Permissions.** Print mode refuses a tool your settings do not
  allow, and the session says what it could not do. varde widens
  nothing: `agents.<agent>.background` arguments (e.g. Claude's
  `--permission-mode auto`) go on background launches only.

## 7. Memory

Memory is varde's reason to exist beside an agent's own session list,
so it has to be provably useful: it reaches the sessions that need it,
it stays true, and varde can show what it was worth (Q19).

**Why it is built this way.** An evaluation of the first design, a
whole cairn rewritten by each update and handed only to a fresh run,
found it wrote good notes and then lost them: the task's own cairn
reached 0 of 106 sessions, "related" lines were relevant about 1 time
in 8, each rewrite lost 58–72% of the previous findings, anchors had
drifted, and agents re-derived the same setup facts task after task.
Outside evidence pointed the same way: citations validated before use
help (GitHub Copilot), whole-document rewrites collapse (ACE), and
keyword search holds up on a small, identifier-heavy corpus.

### 7.1 Shape

Each task has one cairn. It is a document, not a log, and at most 200
words:

```
Status     one line, ≤ 30 words: where this stands now
Findings   ≤ 7 bullets — durable facts: root causes, gotchas, measurements
Decisions  ≤ 5 bullets — what was chosen, and why
Open       ≤ 5 bullets — what is unresolved
```

- **Bullets are items** with a stable id, the run that wrote it, when it
  was last confirmed, and an optional **anchor** (§7.6). A bullet keeps
  its exact words until something changes it, and is at most 40 words.
- **The cap limits what is shown, not what is known.** A bullet that
  leaves stays in history with the reason, and recall still finds it,
  unless the user took it back (Q36). Folded notes stay searchable too.
- **Your lines** (`varde edit`) are marked yours; updates never change
  or drop them, and a Status you set is yours (`status_mine`). In the
  whole-document editor an unchanged line keeps its id wherever it
  moves, and a changed one is a new bullet. The editor carries
  `#: version N`: an edit of a version an update has since replaced is
  refused and kept in a file, since saving it would undo the update.
- **One line at a time.** `--add`, `--set` and `--rm` change one line in
  one transaction on the cairn as it is, so an update landing meanwhile
  is never undone. The caps hold as for updates. A removed line goes to
  history as `removed by you` and `--restore` brings it back; the old
  words of a changed line go as `edited by you`. Recall finds neither
  (Q36). Every client edits this way (Q30).
- **The context comes first.** The editor opens with a `Context:` block,
  the task's body, saved to the body and not capped. A linked task's
  body is its link's (Q1), so the block is refused there.
- **Your lines are capped too.** An edit past the caps, or without the
  cairn's shape, is refused with the reason and kept in a file. Your
  lines count against every session's 500 words, and an update counts
  them too, or its ADDs would push the cairn past the caps.

### 7.2 Updates

- **Edits, not rewrites.** An update gets the current bullets with ids
  (yours marked `[mine]`), pinned notes, which bullets are stale, and
  the transcript since the run's cursor. It answers with `ADD <section>`,
  `EDIT <id>`, `DROP <id> <reason>`, `CONFIRM <id>` and `STATUS`. Code
  applies and checks them and records each change with its reason.
- **The test.** *Would a future session on this task, or a similar
  one, need this?* No edits is a normal answer.
- **Anchors.** A finding about code names a path or backticked
  identifier, a symbol rather than a line number (§7.6).
- **What counts.** A bug fixed, with its cause, and an environment fact
  that surprised the session.
- **Strict format.** Every non-blank line is an edit (code fences
  aside), or the reply is exactly `NO EDITS` or `UNCHANGED`. Anything
  else is asked for once more and never applied in part. An update
  never names its task (§6.2).
- **No fuzzy dedupe.** An `ADD` confirms a bullet only when its words
  match, give or take case and punctuation: a fuzzy match had turned
  "Tests fail on Linux" into a confirmation of "Tests pass on Linux".
  An `EDIT` that would duplicate another bullet is refused.
- **Caps refuse, they do not evict.** Over-long bullets or Status, an
  `ADD` to a full section, and anything past 200 words are refused.
  Drops and confirmations apply first, then edits and Status, then
  ADDs, so a `DROP` makes room. A document already over the caps is
  settled by moving the least recently confirmed non-user bullet to
  history as `cap`, Open before Decisions before Findings.
- **Memory is data, not orders** (Q22). What an update writes may not
  hold a URL, a command piped into a shell (or `curl`/`wget` piped or
  substituted), an instruction to sessions, or a secret. Instructions
  are matched as whole phrases ("ignore previous", "you must", "from
  now on you", "future sessions should" and the like), so findings that
  merely use the words pass. Secrets are key-like tokens (`sk_live_…`,
  `ghp_…`, bearer tokens) or values given to keys like `password` or
  `token`. Your own lines are not judged.
- **Refusals go back once** with the reasons, and the reply with fewer
  refusals is applied.
- **Notes.** `varde note` pins a finding for the next update. Anything
  the secrets filter flags is replaced by `[redacted]` before it is
  stored, and the agent is told. A note is folded only when the applied
  edits took it up.
- **Nudged once.** A `NO EDITS` while notes are pinned is told to fold
  them; a stretch that reads like a reported fix is told so once.
- **Close-out.** Closing, parking or dropping queues a last update
  (kind `close`) that brings Status to where the task ended and settles
  Open: each item goes as done, stays with why, or says it was still
  open, and never with a reason the material does not give. A
  close-out skips a fold of the same run still queued, and a fold
  queued meanwhile makes it read further, so no transcript is read
  twice.
- **Triggers.** The run ends, a compaction is about to happen, a turn
  ends 30 minutes after the last update, or the task closes. The cursor
  makes every trigger idempotent; a fold starts from the furthest any
  run of the transcript got.
- **Hygiene.** The update runs with varde's own system prompt and none
  of the user's settings, `CLAUDE.md` or tools, the material on stdin.
  Claude runs with `--system-prompt`, `--safe-mode` (which keeps the
  user's sign-in, unlike `--bare`), `--tools ""` and `--output-format
  json`, in the store's directory; pi with `--system-prompt` and its
  `--no-*` flags. Inputs are cleaned of invisible and bidi characters
  and secret-like lines.
- **Cost.** One headless call: `claude -p` on haiku, or pi's default
  model, unless `memory.model` says otherwise; tokens are recorded per
  job. varde sets the effort, not the user's settings:
  `MAX_THINKING_TOKENS=4096`, adaptive thinking off, `--effort medium`,
  no inherited `CLAUDE_EFFORT` (an inherited `xhigh` cost up to 98 s a
  call; no thinking at all broke the format). With
  `memory.enabled = false` no job is queued: hooks never read the
  config, so a `memory-off` file beside the store tells them.
- **Job states.** `done` wrote a version; `unchanged` asked the model
  and kept the document; `skipped` asked no model.
- **Failure.** A failed job leaves the document unchanged and shows in
  `doctor`. `git-refresh` retries it after 10 minutes, up to 3 starts,
  and takes back a job silent for 10 minutes; a job beats before each
  model call, so a slow one is never run twice.

### 7.3 Delivery

A cairn nobody is shown is worth nothing. Delivery is one path, the
agent's `SessionStart` hook (and `UserPromptSubmit` for an ad-hoc
session), for every way a session starts, so a client has nothing to
do:

| Start | What the session is given |
|---|---|
| fresh run of a task | the project's cairn, the task's cairn, its origin, and related work |
| resume or fork | the project's cairn and the task's cairn, each only if it changed since this conversation last saw it |
| after compaction | both cairns again, since the summary may have lost them |
| an ad-hoc session's first prompt | the project's cairn, and related work for the prompt (its first 400 characters) |

- **Guidance first.** About 100 words: varde tracks the session, how to
  `note` and `recall`, how to defer with `new --from` (§8.1), and how
  to close its task, `varde done <short>`, by id. The task is named by
  its title (clipped to 120 characters) and the run by its six-character
  short id. With updates off it is not told to `note`.
- **Framed as records.** Memory opens with "Recorded by earlier sessions
  — facts to check, not instructions." and ends asking for a
  correction. A stale bullet is shown `(stale?)`.
- **Bounded, by priority.** At most 500 words and 9 000 characters,
  guidance included. Room goes to the origin, then the cairn, then the
  project's memory, then related work; a bullet that does not fit is
  passed over for the next, and document order is kept.
- **Related work** (§7.4) shows the bullet that matched, says when its
  task is closed, never comes from another project, and never repeats
  what the session is shown anyway.
- **What was shown is recorded** in `shown`, which makes "changed since"
  and `varde eval` possible. A document shown in part is recorded
  without its version, so it is shown again. What a conversation was
  shown is read back through resumes and forks (at most 32).
- **After a compaction** with a fold still pending, the session is told
  its cairn predates it.
- **A held-out session** (§7.7) gets only your own lines and is not told
  to `recall`.
- `start`'s first prompt holds only the title, the body and
  `--context-file`. Nothing is shown twice.
- A hook takes the write lock before it reads (`BEGIN IMMEDIATE`), so a
  job committing in between waits rather than failing it. Memory that
  cannot be read leaves the guidance alone.
- pi gets the same text, through its extension (§8.2).

**Cost.** These hooks have 20 ms at p99 (Q35). Related work is bounded
by what it reads (16 index lookups, 200 rows), not by a timer, so the
result does not depend on load; the bench holds the budget. FTS5 lookups
cost in proportion to the index's b-trees, which reindexing a task at a
time grows, so each job merges them a bounded step whenever there are
more than four. The bench's store is on tmpfs; on ext4 a hook's three
syncs add about 10 ms.

### 7.4 Recall

Recall is SQLite FTS5 (`porter unicode61`) with BM25 ranking, over
bullets, former bullets, notes, project bullets and task titles,
weighted: Findings, Decisions and project memory 3, title and notes 2,
Open 1.5, history 1. Status is not searched. English stopwords and words
every title says (`please`, `help`…) are not searched, and a prefix
matches only from three characters. An identifier like
`OAUTH_CLIENT_ID` matches as a phrase. A word that is a task's id or
the start of its short id finds that task first, in any project.

`varde recall` searches the current project; with no hit there it shows
the best from every project, and `--all` searches everywhere. Each hit is
a task's best matching bullet or note, with its state and staleness;
title-only hits come last. Every title is indexed, so a task already
filed is found before a second is made (`varde new` indexes as it makes
it; a hook's task is indexed by the next `git-refresh`, since hooks write
no index). Every recall is logged with who asked, its run and its hits.

**Related work,** offered unasked by the hooks (§7.3), needs more than a
score, since a BM25 score depends on the index's size. A row must match
two of the query's *specific* terms (in at most 4% of the index, or any
term under 200 rows) or one specific identifier, as whole stemmed
words. Matches are ranked by terms matched, then your lines, then
findings and decisions over notes over open items and history, then the
newest; a task shows its first row. It stays in the task's project,
since another project may be another client's. The project cairn is
never offered, since every session already gets it.

No embeddings. The caller is an LLM that rephrases a query that misses,
the corpus is small and full of identifiers, and the misses measured
came from stopwords, scope and what was indexed, not vocabulary.

### 7.5 Project cairn

Some facts belong to no one task: how to build, the tests that fail on
one machine, a convention, a preference. Each project keeps one more
capped document, its **project cairn** (Q38):

```
Conventions  ≤ 5 bullets
Gotchas      ≤ 7 bullets
How-to       ≤ 5 bullets — commands that work, each with its anchor
```

- **One document type.** Its bullets are items with a project in place
  of a task, so edits, caps, your lines, history, recall and staleness
  work as a cairn's. No Status, at most 200 words; over the caps it
  gives up Conventions, then How-to, then Gotchas.
- **Written by you.** Only `varde edit --project` and the view's Project
  tab change it; no update does (Q38).
- **Where it shows.** First in every session in the project (§7.3), in
  `varde show --project`, and in the view's Project tab. A project is a
  task's project as recorded when it was made (§6.3).
- **Small on purpose.** Context files that restate the code cost tokens
  and did not help; anchored, recurring gotchas pay.

### 7.6 Staleness

Memory that is wrong is worse than none. A bullet whose anchor no longer
resolves is marked `(stale?)` wherever it is shown, and `doctor` counts
them without failing. The next update of that document is told which
are stale, to fix or drop. Nothing is deleted on a guess.

- **The anchor rule.** A bullet's anchor is its first backticked path or
  identifier, else its first plain path, with line numbers and call
  arguments dropped (`project::root(cwd)` is `project::root`). Generic
  words (varde's own names, `git`, `tmux`, any plain word of five letters
  or fewer) are passed over, since they always resolve. Outside
  backticks, a word anchors only with a code shape (`::`, snake_case,
  `--` inside) or when it is plainly a path: a code file's extension, or
  a relative path under a source directory (`src/`, `lib/`, `tests/`,
  `docs/`…). Counting any `a/b` had marked `before/after` and
  `origin/main` stale.
- **Resolving.** The checkout is the task's active worktree, else its
  project; one that cannot be read leaves marks as they were. A path
  resolves when it is on disk or a tracked file ends with it; a path
  outside the checkout (absolute, `~/`, `../`) is never stale. An
  identifier resolves when it names a tracked file or `git grep -q -I
  -F` finds it; `Type::method` and `a.b.c` resolve when each part is
  found, since code writes them apart.
- **When.** After a fold, its cairn's anchors are checked. Each
  `git-refresh` checks at most 50 anchors unchecked for 10 minutes,
  oldest first, within 2 seconds.

### 7.7 Measuring it

varde records what it needs to judge its memory, and `varde eval`
reports it, offline, over the store and the agents' transcripts. It
changes nothing.

- **Recorded:** what each run was shown (`shown`) and every recall
  (`recall_log`), both for 30 days; tokens per job; and per job the
  ADDs that repeated a bullet (`job.repeated`) or nearly repeated one
  its conversation was shown (`job.like_shown`, token Jaccard ≥ 0.8).
- **Reach:** by start kind, runs whose task had a cairn, how many were
  given it, and runs given their project's cairn.
- **Use:** shown bullets whose anchor appears in a later tool call's
  input in that run; agent recalls followed by a `varde show` of a hit.
- **Re-discovery:** bullets a job added that were already said by one
  the conversation was shown (ignored), one in its project (missed), or
  only in another project (out of reach).
- **Fidelity:** the share of each folded note's specifics found in later
  bullets, and still in the cairn; bullets that left, by reason; churn
  between versions.
- **Staleness:** shown anchored bullets marked stale now.
- **Cost:** tokens by job kind and per bullet used; jobs that changed
  nothing.
- **Holdout** (Q23). `memory.holdout` (0 to 1, default 0) withholds
  memory from that share of fresh sessions, to compare orientation
  (tool calls and minutes before the first edit) and re-discovery.
  Hooks never read the config, so `start` decides it for a fresh run it
  launches, from a stable hash of the run's id, and writes it on the
  run. A resume, fork or compaction inherits it; runs `start` did not
  launch are never held out. A held-out session still gets your own
  lines.

## 8. The Claude adapter

```rust
trait Agent {
    fn name(&self) -> &'static str;
    fn normalize(&self, event: &str, payload: &Value) -> Option<Norm>; // started|prompt|turn_end|blocked|unblocked|answered|idle|ended|compacting …
    fn new_native_id(&self) -> Option<String>;          // None: the agent picks its own
    fn launch(&self, how: Launch /*New{id,name,prompt}|Resume{id}|Fork{from,id}*/) -> (argv, env);
    fn family_key(&self, transcript: &Path) -> Option<String>; // links a fork varde did not start
    fn in_family(&self, transcript: &Path, key: &str) -> bool;
    fn digest(&self, transcript: &Path, from: u64, budget: usize) -> io::Result<(String, u64)>;
    fn interrupted(&self, line: &str) -> bool;          // a transcript line that is Esc
    fn tool_calls(&self, transcript: &Path) -> io::Result<Vec<ToolCall>>; // for varde eval
    fn headless(&self, model: Option<&str>, system: &str) -> (argv, env); // material on stdin, VARDE_INTERNAL=1
    fn headless_reply(&self, stdout: &str) -> Result<Answer, String>;     // the reply and its tokens
    fn hook_reply(&self, event: &str, context: &str) -> Value;
    fn keep(&self, payload: Value) -> Value;            // what of a payload is stored
    fn process_names(&self) -> &'static [&'static str]; // what its process is called, for the hook to find
    fn scripted(&self) -> bool;                         // from the hook's environment: not tracked
}
```

The transcript's path comes with each event. Outside the adapter nothing
names an agent. An agent that cannot be given a session id before launch
returns none: the run takes the id of the first event that claims it
through `VARDE_RUN`.

Claude details, all inside the adapter (checked against 2.1.280):

- **Launch.** `--session-id` (a UUIDv4 minted before launch), `-n`,
  `--resume` / `--fork-session`, `CLAUDE_CODE_DISABLE_AGENT_VIEW=1`,
  and `CLAUDE_CODE_ENTRYPOINT=cli`, since the CLI keeps an inherited
  entry point. The first prompt goes after `--`, with a space in front
  when it begins with `/`.
- **Scripted sessions.** `CLAUDE_CODE_ENTRYPOINT` starting `sdk-` (§6.2).
- **Transcripts.** The directory layout, and the digest: each prompt
  with the report that answered it, within a budget.
- **Unblocking.** No event fires when a permission dialog is answered,
  so `unblocked` is inferred from the first `PostToolUse` after a
  `PermissionRequest`.
- **Failures.** `PostToolUseFailure` and `StopFailure` count as the
  events they replace; without them a failed tool left the run
  `blocked` and a failed turn left it `working`.
- **Goals.** `/goal` starts each next turn itself, after a `Stop`, with
  no `UserPromptSubmit`, so the first `PostToolUse` after a turn end
  makes the run `working` again (not after an interrupt). varde has no
  loop of its own.
- **Idle.** `Notification`/`idle_prompt`, only after a finished turn. A
  session that has just started or resumed is `waiting`, except after a
  compaction, which leaves the status as it was.
- **Background work.** `Stop` lists `background_tasks`. With a sub-agent
  still running the turn ends `delegated`, not "needs you"; each
  sub-agent wakes the session with a `<task-notification>` prompt, and
  the last ends in a plain `Stop`. Shells are shown but never hold a
  session out of Needs you, since a server never finishes.
  `SubagentStart`/`SubagentStop` are not used: they also fire for the
  CLI's own helpers.
- **Interrupts.** Esc fires no hook. Claude writes `[Request interrupted
  by user…]` to the transcript, which `varde run` checks once a second,
  recording `interrupted` (making the run `waiting`) as of the line's
  own timestamp. Esc before the first token writes nothing there; only
  Claude's undocumented `sessions/<pid>.json` goes `idle`, so a run
  `working` by its events whose file has said `idle` for 1.5 s is
  recorded `interrupted`. Anything unexpected in that file says
  nothing, and no hook reads it. A session varde did not launch keeps
  its status until its next event or its process is gone.
- **Lineage.** `--resume` keeps the native id (`source: "resume"`), and
  varde records a new Run. `--fork-session` mints a new id (`source:
  "fork"`) and nothing names the parent. Forks varde starts are named
  in advance (`--resume X --fork-session --session-id Y`). A fork varde
  did not start is linked through its transcript: every session of one
  conversation starts with the same first message uuid, so at the
  fork's `SessionStart`, and before its first prompt, varde compares
  the first 16 KB of the transcripts in its directory, and the fork
  joins the task of the earliest family member found.

The fixtures are real hook payloads and transcripts.

### 8.1 Installation

The plugin contains only `hooks.json`, and every entry calls
`varde hook <Event>`. No skill, no shell script. Guidance comes from the
`SessionStart` output of `varde hook` (§7.3), so it always matches the
installed binary, and a pinned plugin has nothing that can go stale but
its list of events. `varde doctor` compares that list with the binary's,
and the plugin's version is raised whenever its events change.

### 8.2 pi

pi (earendil-works/pi, checked against 1.1.0) has no shell hooks.
`cli/pi/varde.ts` is its plugin: an extension that forwards session
events to `varde hook --agent pi <event>` and holds no logic. It sends
`session_id`, `session_file`, `cwd`, and `reason`, `prompt` or
`aborted`; at `session_start` also the header's `timestamp`,
`parentSession`, and the time of the last message.

| pi event | varde |
|---|---|
| `session_start` (`startup`, `resume`, `fork`, `new`) | `started`; `new` is like Claude's `/clear`, `reload` is ignored. pi says `startup` for any session it opens at start, so a `startup` session with no message is new, one with a `parentSession` whose messages all predate its header is a fork, and any other with messages is resumed. |
| `before_agent_start` | `prompt`. What `session_start`, `session_compact` and this hook handed back (§7.3) rides along as a hidden extension message. |
| `agent_settled` | `turn_end`, or `interrupted` with `aborted`. Not `agent_end`: pi may retry, compact or run queued work after it. |
| `ui_prompt_start` / `ui_prompt_end` | `blocked` / `answered` (an extension's dialog is what blocks); answered mid-turn, `unblocked`. Closing one of two dialogs is `dialog_closed`, still `blocked`. |
| `session_before_compact` / `session_compact` | `compacting` / `compacted` |
| `session_shutdown` | `ended` |

- **Launch.** `--session-id` and `-n`, the first prompt after `--` with a
  space in front when it begins with `@` or `/`, `--session <id>` to
  resume (in the directory the run reported, where pi filed it), and
  `--fork <id> --session-id <new>`. There is no idle event.
- **Forks** are linked directly through the header's `parentSession`.
- **Interrupts.** Esc ends the turn with `aborted`; the aborted reply
  (`stopReason: "aborted"`) is left out of the digest.
- **Memory updates.** `pi -p` with no tools, session, extensions, skills
  or context files.
- **Scripted sessions.** pi says nothing, so `pi -p` with the extension
  is tracked.

## 9. The Emacs client

| Concern | Design |
|---|---|
| Link | Org meets varde through `varde:` links only (Q31): `[[varde:SHORT][Title]]` names a task, `[[varde:project:DIR][…]]` a project's cairn. `org-varde-insert-link` completes by title (open tasks first, then closed, then project cairns, annotated) from the rows last fetched, refreshing them in the background. Markdown gets `[Title](varde:SHORT)`. `y` copies one; `:store`, `:complete` and `:export` work. Following one opens the view. |
| Status on links | `:activate-func` puts the board's glyph and word after each task link, in an overlay, from the board's rows or a `list --all` fetched from an idle timer: font-lock spawns nothing. A buffer is flushed only when a status its links show changed. |
| New task | `org-varde-new-here` on a heading or region opens the new-task form with the title, the text (without drawers, planning or logbook) and a `From:` link back. The project is the inherited `VARDE_PROJECT`, else the file's repository, else asked. Once `varde new` answers, the link goes in as the heading's first line of text, or below the region (never inside a table, block, drawer or list), unsaved. On a heading that links an open task, it offers that instead. A form left unchanged is dropped by `C-c C-k`; a changed one is kept as a draft. |
| DONE | A heading moved to a done keyword (not a repeating one) whose own text links open tasks asks once to close them, as `d` does (`org-varde-close-with-heading`). One way only: closing a task never changes the heading. Each board refresh closes the terminal and workspace of a task done today unless its terminal still runs something. |
| Review | `org-dblock-write:varde` (`#+BEGIN: varde :project DIR :state open\|done\|all :closed 7d`) fills a table from one `list --all --json`: glyph, link, project, the cairn's Status and the day it closed. `cairn_status` comes only with `--all`, so the frequent refresh stays small. |
| Migration | `org-varde-convert-links` turns each heading's `VARDE_TASK` into a link line, removes the property and runs `varde unlink`, in the buffer or (with a prefix) every agenda and `org-directory` file. It saves nothing. |
| Org writes | Only the links `org-varde-new-here` and `org-varde-convert-links` insert, and neither saves. |
| Board keys | The terminal board's wherever Emacs allows (Q29), in `org-varde-board-mode-map`; the header shows only the main few and `?` the rest. Nothing destructive sits on a motion key, so park is `P` and a session of your own `N`. `x` asks before dropping; `u` reopens what `d`, `P` or `x` last closed. Under evil the mode's keys win in normal state, except `v`/`V` (`K` views) and `z` (`za` folds). Rows are laid out as the terminal board's, without ids. Every group is drawn, an empty one as one quiet line. |
| Board | Renders the rows of `varde list --json`. It watches the state directory (and `varde.db-wal` itself, for kqueue), and refreshes with an asynchronous `varde list` 0.2 s after the last change and at most 1 s after the first, and whenever the board comes into view. The render spawns nothing. It keeps to the project of the buffer it was opened from (Q26). A newer `v` than it reads is said once instead of rows. |
| Launch | Runs `varde start --json`, opens a terminal running your shell (the first of `org-varde-terminals` installed, or `org-varde-terminal-function`), and types the plan's `varde run <run>` into it. The buffer is tagged with the task; RET after the agent exits resumes there. `N` runs the agent `varde agent --json` names, or `org-varde-agent-command`. |
| Workspaces | Optional, for Doom: named by the task's short id, behind one check that workspaces are loaded (`org-varde-workspaces`), failures reported, not raised. `org-varde-next-needs-you` goes from anywhere to the next task that needs you, from the board last fetched, relative to the task of the terminal or view you are in. `org-varde-board` comes back to the board's workspace. |
| Seen | Looking at a task's terminal, in the selected window while Emacs has focus, runs `varde seen` if the board shows it unread. |
| Memory | Never written to org. `v`, `org-varde-show` or a link opens the task's view from `show --json --no-git`: the cairn first, then context, notes, sessions, git. The keys are the terminal view's (Q30), each a `varde edit --add/--set/--rm/--restore … --json` that answers with the cairn shown next; the prompt counts words against `limits`, and a refusal asks again with varde's reason. `E` edits the whole document; `[`/`]` switch to the Project tab. The edits run synchronously: each is one short transaction, and its answer is needed to ask again. |

## 10. Practices

- **Tests.** Property tests for the status machine and the attach
  decision. Golden fixtures for the adapter.
- **Ingest never loses an event.** If the database cannot be written
  within 100 ms, `varde hook` appends to `pending.jsonl`, and the next
  command ingests that first.
- **Schema.** Numbered migrations on `user_version`, and a refusal to
  open a schema newer than the binary knows.
- **Budgets.** `cargo test --release -p varde --test bench -- --ignored
  --nocapture` builds 1 000 tasks of 2 runs and 500 events each, each
  with a cairn and notes, and a project cairn, and times `varde list
  --json` and hooks end to end, process start included (§2, §7.3). The
  store is on tmpfs, so it leaves out a disk's syncs.
- **CI.** Runs on Linux and macOS, because the mac is where the
  `date +%s%N` and git 2.39 bugs turned up.

## 11. Deliberately left out

What looks like a missing feature is often one of these.

- Saving user buffers. The links org-varde inserts are left unsaved.
- A heading per task, kept in step (Q31): task properties on headings,
  keywords driving tasks and back, archiving, reconciling, an index
  over every org file, and an agenda status column. Links replace them.
- LLM titles on the hook's path, or more than once (Q28).
- An append-only memory log, checkpoints, and a summary at done.
- Auto-filed follow-up tasks. Deferred work becomes an Inbox task only
  when you asked for it (§6.4).
- Hiding old Inbox tasks. You drop what you do not want.
- Conflict hand-off by typing into a session.
- `done --merge`. Merging is the step people most want to do their own
  way, varde notices work landed by any route, and it was the only
  thing that made varde write to the base branch.
- Workspaces named by title slugs; they are named by the short id.
- Whole-document cairn rewrites, fuzzy dedupe, and memory in the first
  prompt (§7).
- Promotion into the project's cairn (Q38).

## 12. Decisions

| # | Question | Decision |
|---|---|---|
| Q1 | Who owns a task | varde: identity, title, lifecycle and memory. Org holds only `varde:` links (Q31). |
| Q2 | Sync | None. Machines are isolated (§5). Recall uses FTS5; embeddings deferred. |
| Q4 | Ad-hoc sessions | They become tasks automatically and wait in the Inbox until you act on them. |
| Q5 | Terminal | A shell with `varde run <run>` typed into it. |
| Q6 | Standalone viewing | Plain commands and a TUI board. Inside tmux the board hosts each session in a window of its own, tagged `@varde_task`, `@varde_owned` and `@varde_home` with the board's environment, so the run lands in the board's store; RET reuses the task's idle shell. Only windows the board opened are closed, once back at an idle shell. While a board runs it rings a window's bell when its task needs you and marks the on-screen task seen (`varde run` also marks its own pane). Outside tmux it suspends. |
| Q8 | Agents | Claude, then pi, behind one adapter trait. |
| Q9 | Memory model | One bounded cairn per task; notes are pinned inputs; your lines are protected; history is kept. Updates are edits (Q19). |
| Q10 | Git scope | Worktree lifecycle only. varde never merges or writes to the base, and does no conflict hand-off. PRs are read, not opened: a merged PR counts as landed. |
| Q12 | Claude integration | A plugin holding only hooks; guidance and memory come from the binary. No skill. |
| Q13 | Emacs board | Native, rendering varde's pre-grouped rows. |
| Q15 | Fewer options | Every surface shows only what is used daily; the rest is hidden but reachable (hidden commands and flags, the `?` menu). Discarding work and dropping a task are one act, `drop`, which always keeps the work under a ref. |
| Q17 | Settings | A config file for what is the user's, a repository's git config for what is the repository's. Nothing matched by path, nothing committed run, hooks read neither, a broken setting never stops a session. |
| Q18 | Questions only the user can answer | `start` never guesses a first run's worktree or a deferred task's project: it returns `ask` and changes nothing, and the client asks and calls again. |
| Q19 | Memory, rebuilt | The first cairn reached no session and lost what it learned (§7). Now: bullets with ids, edits checked against caps, delivery through hooks for every start, anchors checked against code, and what was shown recorded for `varde eval`. |
| Q22 | Memory is data, not orders | A project cairn reaches every session, so one obliging model could plant an order in all of them. Memory is framed as records; updates may not write URLs, piped commands, instructions or secrets; notes are redacted; related work stays in its project. |
| Q23 | Holdout | `memory.holdout`, off by default, withholds memory from a stable share of fresh sessions so `eval` can compare. |
| Q24 | A worktree of your own | A task belongs to the checkout it was made in, a linked worktree included, and its worktree starts from and lands in that checkout's branch. |
| Q25 | Sessions varde did not launch | The hook finds the agent's process by name and records it; the session ends when the process is gone. With none found, none is recorded, since a wrong one would end a live session. |
| Q26 | A board kept to its project | A board opened in a repository shows that project's rows once it has one, and says how many tasks elsewhere need you. It is only a view: the sweep, bells and reconciling still see every task. `.` shows all; `varde list` keeps every project unless `--project` asks. |
| Q27 | Nothing typed is lost to one key | Every place you write keeps the text when you leave it (drafts on Esc, C-g, a failed create, a refused edit). Prompts edit text readline's way, and both boards share keys wherever Emacs allows. |
| Q28 | Titles written by a model, at creation | A title cut from a first prompt is often a greeting, and a board is read by its titles. One small detached call names a task when it is made, so no hook or interactive path waits. Only titles varde wrote, asked once, a typed title always wins. Naming in the cairn update was tried first and came too late. |
| Q29 | A stable board, one keymap | Every group always shown in a fixed order, so the layout never jumps; folding is the user's. vi keys where vi has one, the same on both boards; nothing destructive on a motion key. The footer shows the keys the row needs most. |
| Q30 | Cairns edited a line at a time | `show` and the views present a cairn the same way, and every client edits one line by id (`--add/--set/--rm/--restore`): a whole document open for minutes raced the updates. Removing asks nothing, since `--restore` undoes it. `$EDITOR` stays for bulk changes. |
| Q31 | Org through links | Keeping a heading per task in step took about 1 050 lines and 85 tests and still had two sources of truth. Now notes link tasks, links show status, a heading can make a task, DONE offers to close its tasks one way, and a dynamic block makes a review. Nothing is synced, so nothing disagrees. |
| Q32 | The task's branch | Tracked by the worktree, not the name: a branch cut or renamed in the worktree is the task's, and a pre-existing one is never deleted. It tracks its local base. Rejected: detached-HEAD worktrees until the user names a branch, and dropping the `varde/` prefix by default (`branch_prefix = ""` does). |
| Q33 | Scheduled and repeating tasks | A schedule starts a fresh background run in print mode; once done it is an ordinary task you resume. Opening it mid-work takes it over. Rejected: hosting it in tmux to watch live, and a pty server of varde's own. No daemon: a heartbeat runs `varde tick` each minute. Org's `SCHEDULED` never drives varde. |
| Q34 | Done on a repeating task | `done` ends the run, not the series, which ends with its schedule. A run with nothing to report says so with `done` on its own task: one stored fact, `run.done`. Each unread run keeps the row in Needs you and is read only by opening it. Rejected: `done --series`, deriving "nothing to report" from events, and putting findings in the cairn, which is the next run's memory, not a log. |
| Q35 | Latency budgets | A hook 10 ms at p99, the memory hooks 20 ms, `varde list --json` 15 ms at 1 000 tasks. Tighter ones failed on machine load alone, and neither is felt beside an agent that takes a second to start. |
| Q36 | What you take back is not recalled | Lines you remove, and the old words of lines you change, stay in history and `--restore` but not in recall's index, since a removed line may be wrong. Lines updates drop are still recalled. |
| Q37 | Repeating tasks in daily use | Derived, never stored: the unread count on the row, `↻ due` and what holds a start up, how many runs `done` ended, a paused schedule on a parked task, every run in the view, and on a board `d` between runs asks to end the series. Left out: run numbers. |
| Q38 | The project cairn is yours | Nothing promotes into it: an audit found every promoted bullet was one task's facts, acted on 11% of the times shown. A fact true of most tasks is rarely what a task finds, so you write it. |
| Q39 | Repeating tasks reviewed by whole flows | `tests/schedule_flows.rs` walks weeks of a series and checks after every step that the right runs are unread and every surface agrees. A session's own `done` ends only its run; an agent never stops a session of yours; a run at work keeps its row in Working. Open: a notification for a finding, pruning `cairn_history` for frequent series, refreshing a series' worktree once its work lands, and letting background runs call `varde done`/`note` unprompted. |
