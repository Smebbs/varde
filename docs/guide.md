# varde

The guide to the CLI and the terminal board. [`emacs.md`](emacs.md)
covers the Emacs client, and [`varde-design.md`](varde-design.md) says
why varde is built this way.

## Concepts

- A **task** is a piece of work you give an agent. Its **sessions** are
  the agent's conversations about it. Starting a task goes to its live
  session, resumes its last one, or starts fresh.
- A **cairn** is what a task's sessions learned, handed to the next
  one: a Status line, and short lists of Findings, Decisions and Open
  items. Each project keeps one too.
- The **board** groups tasks by what they need from you:

  | Group | Holds |
  |---|---|
  | Needs you | a live session whose turn ended (*your turn*) or that asks you something (*needs input*) |
  | Working | a live session at work |
  | Open | tasks with no session running |
  | Scheduled | tasks varde starts itself at a time, soonest first |
  | Inbox | sessions you started yourself, and work an agent left for later |
  | Done | done today, or not landed yet |
  | Parked | put aside (folded) |

  `●` is live (green working, yellow your turn, red needs input), `○`
  nothing running or already seen, `✓` done, `‖` parked, `✕` dropped.
  Every group is always shown, in the same order.
- A task ends **done** (`d`), **parked** (`P`) or **dropped** (`x`).
  Starting it again reopens it, and a dropped task's work is kept under
  a git ref.

## Install

varde needs git, and Claude Code or pi. tmux 3.0 or later is
recommended: inside it, each session gets a window of its own.

```sh
brew install smebbs/tap/varde
```

Or take a binary from the
[latest release](https://github.com/Smebbs/varde/releases/latest), or
build it with Rust 1.88 or later:

```sh
cargo install --locked --git https://github.com/Smebbs/varde varde
```

The hooks call `varde` by name, so it must be on your PATH: for
`cargo install`, add `~/.cargo/bin`.

In Claude Code, install the plugin. It holds only hooks, each calling
`varde hook <Event>`:

```
/plugin marketplace add Smebbs/varde
/plugin install varde@varde
```

For [pi](https://pi.dev), link the extension from a clone of this
repository. It forwards pi's events to `varde hook --agent pi`:

```sh
mkdir -p ~/.pi/agent/extensions    # or $PI_CODING_AGENT_DIR/extensions
ln -s "$PWD/cli/pi/varde.ts" ~/.pi/agent/extensions/varde.ts
```

Then run `varde doctor`. It checks the store, the PATH, the config, the
hooks and the memory updates, and says how to fix what fails.

## Commands

```sh
varde                        # the board
varde --popup                # the board in a tmux popup, closed once you go to a session
varde new Fix the login bug  # a task in this checkout (--context TEXT: what the agent is told)
varde new --context "$(cat issue.md)"  # no title: a model names it
varde attach k3m9qz          # start it, resume it, or go to where it runs
varde attach k3m9qz --fork   # branch off its conversation (--fresh: a new one)
varde next                   # in tmux: the window of the next task that needs you
varde done k3m9qz            # close it; its worktree goes once its work has landed
varde park k3m9qz            # put it aside, worktree kept
varde drop k3m9qz            # drop it; its work is kept under refs/varde/discarded/
varde reopen k3m9qz          # back to Open, without starting it
varde accept k3m9qz          # an Inbox task into Open
varde schedule k3m9qz every weekday 09:00  # varde starts it then, a fresh session each time (--off stops it)
varde new Triage mentions --when "every day 06:00" --worktree never  # a task that starts itself
varde set k3m9qz --project ~/src/api  # move it to another project
varde show k3m9qz            # where it stands, its cairn, its sessions
varde edit k3m9qz --add findings The cache key omits the locale.
varde recall locale cache    # what earlier tasks learned (--all: every project)
varde show --project         # the project cairn
varde list                   # the board as text (--all: closed tasks too)
varde eval                   # what memory was worth here
```

A task is named by the six characters the board shows; the first four
do while they are unique. Inside a session varde runs, `done`, `park`
and `show` with no task mean its own. `varde help <command>` describes
each command.

**Projects.** A task belongs to the checkout it was made in. A
worktree you made yourself is a project of its own; one varde made
belongs to its repository. `varde set --project` moves a task once it
has no live session and no unlanded worktree.

**Worktrees.** On a task's first run in a repository, varde asks
whether it gets a worktree (`--worktree always|never`, or the settings
below, decide instead). It is `.worktrees/<slug>` on branch
`varde/<slug>`, cut from your base branch. Submodules are checked out,
ignored files listed in `.worktreeinclude` are copied, and `git config
varde.setup` runs. The branch tracks the base, so magit and `git
status` show the task's work as the commits ahead of it. Rename it as
you like: varde follows the branch the worktree has checked out, one
the agent cut there included, and never deletes a branch that was there
before the task. After `done`, the worktree and its branch are
removed once nothing can be lost: no uncommitted changes, every commit
in the base branch or a merged PR, no session running. Until then the
task stays under Done.

**Sessions you start yourself.** A plain `claude` or `pi` session
becomes an Inbox task after its first prompt, and stays there once it
ends until you accept, start or close it. `claude -p` and the Agent SDK
are not tracked.

**Scheduled and repeating tasks.** `varde schedule <task> RULE` makes
varde start the task itself, in the background: a fresh session with
no terminal (the agent's print mode), on the board as any other. A run
that found nothing to report says so (`varde done` on its own task) and
goes quietly back to Scheduled; one that found something waits in Needs
you, and RET (or `varde attach`) opens it in your terminal. RET while it still works asks
whether to take it over: it stops there and carries on with you. Nobody
is there to approve a tool your settings do not allow, so such a tool
is refused, and the session says what it could not do. To let it decide
for itself, give the agent the arguments for that in `background` (see
Configure), e.g. Claude's `["--permission-mode", "auto"]`. RULE is one of:

- `at mon 09:00`, `at 2026-11-02 07:00`, `at tomorrow`: once.
- `every weekday 09:00`, `every day 06:00`, `every mon,thu`,
  `every weekend 10:00`: on those days.
- `every 30m`, `every 4h`, `every 2d`: that long after it last started.
- An org timestamp: `<2026-10-19 Mon 09:00 ++1w>`.

A time left out is midnight, the first moment the machine is on that
day: `every day` is "when I open my laptop in the morning". A start
missed while the machine was off or asleep happens as soon as it is
on, once, however many were missed. Each run of a repeating task is a
fresh conversation, given the task's cairn, so tell it what to do in
its context and let the cairn carry how far it got. The next run waits
for the last one's cairn update, and stops a session of it left idle
for 15 minutes; one you are still in is left alone.

On a repeating task, `d` (`varde done`) ends this run, not the series:
its session stops, what it found counts as read, and the task waits
for its next time. Its row shows `↻` while a run is at work or waiting
for you, to say so. To end the series, take the schedule off (`varde
schedule <task> --off`, or `e` and clear `When:`), then `done` closes
the task as any. `park` and `drop` stop the schedule, and `reopen`
resumes it.

Every run you have not opened keeps the row in Needs you, so a week
away buries nothing: RET opens the newest one, and the next RET the one
before. The view (`v`) lists every run of a repeating task under its
cairn, newest first: `●` one you have not opened, `quiet` one that
found nothing. RET on one opens it (`varde attach <task> --run <run>`).
Nobody is there to
answer when it starts, so a first start that would ask about a
worktree needs `--worktree always|never` up front.

Once, run `varde schedule --install`: it installs a systemd user timer
(on macOS a LaunchAgent) that runs `varde tick` every minute. Without
it, schedules fire only while a board is open, and `varde doctor` says
so.

## The board

`varde` opens the board, kept to the project you are in (`.` shows
every project). The footer shows the keys the row needs most; `?`
shows them all.

| Key | Does |
|---|---|
| `RET` | Go to the task's session: start it, resume it, or go to where it runs. |
| Tab / S-Tab | The next / previous task that needs you. |
| `s` | In tmux, start it in a window of its own and stay on the board. |
| `R` / `F` | Start a fresh conversation / fork the latest. |
| `n` / `e` | New task / edit its title and context. |
| `d` / `P` / `x` | Done / park / drop. `x` asks; `d` and `P` ask only about a busy session. |
| `u` | Reopen the tasks last closed here. |
| `a` | Accept an Inbox task. |
| `m` | Move it to another project. |
| space | Mark it; `a`, `d`, `P` and `x` then act on every marked task. |
| `v` | The view: the task's cairn, and its project's on the other tab. |
| `E` | The whole cairn in `$VISUAL` or `$EDITOR`. |
| `/` | Recall. `j`/`k` pick a hit, RET views it, `s` starts it. |
| `=` | The task's diff. |
| `j` `k` `gg` `G` `{` `}` | Move by task, to the ends, by group. |
| C-d / C-u | Half a page. |
| `z` | Fold the group. |
| `.` | This project, or every project. |
| C-l | Redraw and read the board again. |
| `q` / Esc | Close the pane, else quit (`q` only, unless configured). |

**The new-task form.** `n` opens it on the title; Tab and Shift-Tab
move between the title, `When:` and the context, as in org-varde's
form. RET on the title, or C-s, creates the task. With no title, a
model names it from the context. C-x C-e writes it in `$EDITOR`. Esc
puts the draft away, and the next `n` brings it back. Prompts edit text
as a shell does (C-w, C-u, C-k, Alt-b/f, C-y).

### The view

`v` opens a pane under the board with two tabs: **Task** (the cairn,
then the notes waiting, the context and the sessions) and **Project**
(the project's cairn). Lines you wrote are marked `›`; a line about
code that is gone ends in a yellow `?`. Other keys work as on the
board, for the task in view.

| Key | Does |
|---|---|
| `[` / `]` | The Task / Project tab. |
| `j` / `k` | Pick a line. |
| `e` | Edit the line. |
| `o` / `a` | Add a line to this section / to a section picked by key (`f`, `d`, `o`, `s`; `c`, `g`, `h` in a project cairn). |
| `x` / `u` | Remove the line / bring it back. |
| `X` | Drop the task. |
| `E` | The whole cairn in `$EDITOR`. |
| `J` / `K` | The next / previous task. |
| `q` / Esc / `v` | Back to the board. |

A line you are typing is never lost: Esc puts it away, and the same key
on the same line brings it back.

### Inside tmux

Keep the board a key away in a popup (tmux 3.3 or later; on 3.2, leave
out `-B`), and bind `varde next`:

```sh
# ~/.tmux.conf
bind-key b display-popup -E -B -w 90% -h 85% "varde --popup"  # prefix b: the board
bind-key N run-shell -b 'varde next'                           # prefix N: the next task that needs you
bind-key B run-shell -b 'varde next --board'                   # prefix B: the board
```

Or run the board in a window of its own (`tmux new -s varde varde`).
Each session gets a tmux window named after its task, which rings the
bell when it needs you; prefix `l` goes back to the board. The board
closes a closed task's windows once their shell is idle. `run-shell`
uses tmux's environment, so `varde` must be on the PATH tmux started
with.

Outside tmux, the board runs one session at a time: RET hands it the
terminal, and the board comes back when the agent exits.

## Cairns

A cairn is short on purpose, since every session is handed it: at most
200 words, a Status (30 words), up to 7 Findings, 5 Decisions and 5 Open
items (40 words each). A project's cairn holds Conventions, Gotchas and
How-to (5, 7 and 5 lines).

**Updates.** A session's work goes into its task's cairn when it ends,
before it compacts, and at most every 30 minutes while it runs; closing
a task updates it once more. The update is a detached call to the
agent (`claude -p` on haiku, or pi's default model), with varde's own
prompt and no tools, reading only the transcript since the last update.
It answers with edits, which varde checks against the caps and filters
for secrets, URLs and commands. A task closed as done may add to its
project's cairn. The agent can pin a finding with `varde note`.

**Editing.** A line you write is yours: updates never change or drop
it. Edit in the board's view, in Emacs, or by the ids `varde show`
prints:

```sh
varde edit k3m9qz --add findings The cache key omits the locale.
varde edit k3m9qz --set 12 The cache key omits locale and region.
varde edit k3m9qz --rm 12          # recall no longer finds it
varde edit k3m9qz --restore 12
varde edit k3m9qz --rm status      # hand your Status back to the updates
varde edit --project --add gotchas 'launch.rs tests need tmux.'
varde edit k3m9qz                  # the whole cairn in $EDITOR
```

Nothing is lost: `varde show --history` has every earlier version.

**Stale lines.** A line naming a path or identifier that is no longer
in the code is shown `(stale?)`, and the next update fixes or drops it.

**Delivery.** Through the hooks: a fresh session is given the project
cairn, its task's cairn, the task it was deferred from and related
lines from the project; a resume, what changed since it last saw them.
It stays within about 500 words, introduced as facts to check, not
instructions.

**Recall.** `varde recall` (`/` on the board) searches cairn lines,
removed lines, notes and project cairns, one hit per task.

**Evaluation.** `varde eval` reports, over the last 30 days, how much of
what sessions were shown they used, what was repeated or stale, and
what the updates cost. `memory.holdout` gives a share of fresh sessions
no memory for comparison.

## Configure

Nothing needs configuring. `~/.config/varde/config.toml` (or
`$VARDE_CONFIG`) changes the defaults:

```toml
agent = "claude"                  # or "pi"

[agents.claude]
command = ["claude"]              # e.g. a wrapper that ends in: exec claude "$@"
background = []                   # added to a scheduled session, e.g. ["--permission-mode", "auto"]

[memory]
enabled = true                    # false: no model calls
# model = "sonnet"                # default: haiku for claude, pi's default for pi
titles = true                     # false: no model-written titles
holdout = 0.0

[worktree]
default = "ask"                   # ask, always or never
dir = ".worktrees"                # or e.g. "~/worktrees/{repo}"
branch_prefix = "varde/"

[board]
scope = "project"                 # or "all"

[keys]                            # rebind a board key by action
# park = "p"
# quit = "esc"
```

The actions are `start`, `background`, `fresh`, `fork`, `next-needs`,
`prev-needs`, `new`, `edit`, `done`, `park`, `drop`, `accept`, `move`,
`mark`, `view`, `cairn`, `editor`, `diff`, `recall`, `down`, `up`,
`first`, `last`, `next-group`, `prev-group`, `half-down`, `half-up`,
`fold`, `scope`, `refresh`, `quit` and `help`.

A repository's own settings live in its git config, so nothing
committed is ever run:

```sh
git config varde.worktree always       # its first runs get a worktree
git config varde.base develop          # the branch worktrees start from and land in
git config varde.setup "pnpm install"  # run in each new worktree
```

**What varde trusts.** It runs only what you set yourself, as you: the
agent `command` in your config, and `varde.setup` in a repository's
local git config. Memory is written by agents and handed to later
sessions, so treat it as the agent's own output: what a model writes is
kept free of secrets, URLs, commands piped into a shell and orders to
sessions; your own lines are never filtered.

**Files.** The store is `~/.local/state/varde/varde.db` (`$VARDE_HOME`
for the directory), readable by you alone. It holds your prompts.
Sessions varde launches carry `VARDE_RUN`; its own agent calls carry
`VARDE_INTERNAL=1`, which the hooks ignore.

## Troubleshooting

`varde doctor` checks all of this.

- **No sessions on the board, or no status changes.** The hooks are
  missing or old: install the plugin again.
- **Another `varde` on the PATH.** `doctor` says which.
- **`varde next` does nothing.** It works from a tmux key, with `varde`
  on tmux's PATH.
- **A setting is ignored.** One varde cannot use is left out, with a
  notice.

## Upgrading and uninstalling

Install the new binary as you did the old one; the store upgrades
itself. Keys that moved since 1.0 are in
[`CHANGELOG.md`](../CHANGELOG.md).

To uninstall, `/plugin uninstall varde@varde` (for pi, remove the
link), remove the binary and `~/.local/state/varde`, and in each
repository remove any worktree left in `.worktrees/`, the refs under
`refs/varde/`, and the `.worktrees/` line in `.git/info/exclude`.

## Clients

`list`, `show`, `new`, `start`, `edit`, `done`, `park`, `drop`,
`recall`, `eval`, `doctor` and `agent` take `--json`.
[`clients.md`](clients.md) is the contract for a client in another
editor.
