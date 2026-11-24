# varde

The CLI and the terminal board. Emacs is in [`emacs.md`](emacs.md).

## Concepts

- A **task** is a piece of work for an agent; its **sessions** are the
  agent's conversations about it.
- A **cairn** is what those sessions learned, handed to the next one: a
  Status line plus short lists of Findings, Decisions and Open items.
  Each project has one too.
- The **board** groups tasks by what they need from you:

  | Group | Holds |
  |---|---|
  | Needs you | a live session whose turn ended (*your turn*) or that asks you something (*needs input*) |
  | Working | a live session at work |
  | Open | tasks with no session running |
  | Scheduled | tasks varde starts itself, soonest first |
  | Inbox | sessions you started yourself, and work an agent left for later |
  | Done | done today |
  | Parked | put aside (folded) |

  `●` live (green working, yellow your turn, red needs input), `○` idle
  or seen, `✓` done, `‖` parked, `✕` dropped. Every group is always
  shown, in this order.
- A task ends **done** (`d`), **parked** (`P`) or **dropped** (`x`).
  Starting it again reopens it. A dropped task's work is kept under a
  git ref.

## Install

You need git, and Claude Code or pi. tmux is optional.

```sh
brew install smebbs/tap/varde
# or, with Rust 1.88+:
cargo install --locked --git https://github.com/Smebbs/varde varde
```

Or grab a binary from the
[latest release](https://github.com/Smebbs/varde/releases/latest).
A beta is a pre-release, which Homebrew and "latest" leave out: take its
binary from its [release page](https://github.com/Smebbs/varde/releases),
or `cargo install --locked --git https://github.com/Smebbs/varde --tag
v1.6.0-beta3 varde`. On macOS a downloaded binary is quarantined until
you clear it: `xattr -d com.apple.quarantine ./varde`.
`varde` must be on your PATH, since the hooks call it by name.

Claude Code: install the plugin (it's just hooks calling `varde hook`).

```
/plugin marketplace add Smebbs/varde
/plugin install varde@varde
```

[pi](https://pi.dev): link the extension from a clone of this repo.

```sh
mkdir -p ~/.pi/agent/extensions    # or $PI_CODING_AGENT_DIR/extensions
ln -s "$PWD/cli/pi/varde.ts" ~/.pi/agent/extensions/varde.ts
```

Then `varde doctor` checks the setup and says how to fix what's wrong.

## Commands

```sh
varde                        # the board
varde --popup                # the board in a tmux popup, closed once you go to a session
varde new Fix the login bug  # a task in this checkout (--context TEXT: what the agent is told)
varde new --context "$(cat issue.md)"  # no title: a model names it
varde attach k3m9qz          # start it, resume it, or go to where it runs
varde attach k3m9qz --fork   # branch off its conversation (--fresh: a new one)
varde next                   # in tmux: the window of the next task that needs you
varde done k3m9qz            # close it; its worktree goes once its work has landed, or a day later
varde park k3m9qz            # put it aside, worktree kept
varde drop k3m9qz            # drop it; its work is kept under refs/varde/discarded/
varde reopen k3m9qz          # back to Open, without starting it
varde accept k3m9qz          # an Inbox task into Open
varde schedule k3m9qz every weekday 09:00  # varde starts it then (--off stops it)
varde new Triage mentions --when "every day 06:00" --worktree never  # a task that starts itself
varde set k3m9qz --project ~/src/api  # move it to another project
varde show k3m9qz            # where it stands, its cairn, its sessions
varde edit k3m9qz --add findings The cache key omits the locale.
varde recall locale cache    # what earlier tasks learned (--all: every project)
varde show --project         # the project cairn
varde list                   # the board as text (--all: closed tasks too)
varde eval                   # what memory was worth here
```

A task is named by the six characters the board shows, or the first
four while they're unique. Inside a session varde runs, `done`, `park`
and `show` without a task mean that session's task. `varde help
<command>` has the rest.

**Projects.** A task belongs to the checkout it was made in. A
worktree you made yourself counts as its own project; one varde made
belongs to its repo. `varde set --project` moves a task once it has no
live session and no unlanded worktree.

**Worktrees.** On a task's first run in a repo, varde asks whether it
gets a worktree (or `--worktree always|never` and the settings below
decide). It's `.worktrees/<slug>` on branch `varde/<slug>`, cut from
your base branch and tracking it. Submodules are left for the session
to fetch, ignored files listed in `.worktreeinclude` are copied, and
`git config varde.setup` runs. Rename the branch if you like: varde follows what
the worktree has checked out, and never deletes a branch that existed
before the task. After `done`, the worktree and branch go once nothing
can be lost (clean, every commit in the base or a merged PR, no
session running). A worktree whose work has not landed a day after
`done` is put away as `drop` does it, under `refs/varde/discarded/`,
and starting the task brings it back. The task leaves Done the day
after it closed either way; `varde list --all` still shows it.

**Sessions you start yourself.** A plain `claude` or `pi` session shows
up in the Inbox after its first prompt, and stays there until you
accept, start or close it. `claude -p` and the Agent SDK aren't
tracked.

### Scheduled and repeating tasks

`varde schedule <task> RULE` makes varde start the task itself, in the
background: a fresh session with no terminal (the agent's print mode).
RULE is one of:

- `at mon 09:00`, `at 2026-11-02 07:00`, `at tomorrow`: once.
- `every weekday 09:00`, `every day 06:00`, `every mon,thu`,
  `every weekend 10:00`: on those days.
- `every 30m`, `every 4h`, `every 2d`: that long after the last start.
- An org timestamp: `<2026-10-19 Mon 09:00 ++1w>`.

No time means midnight, i.e. the first moment the machine is on that
day. Starts missed while the machine was off fire once when it's back.
Run `varde schedule --install` once to add a systemd user timer (a
LaunchAgent on macOS) that runs `varde tick` every minute; without it,
schedules only fire while a board is open.

A run with nothing to report says so (`varde done` on its own task) and
goes quietly back to Scheduled. One that found something waits in
Needs you; RET (or `varde attach`) opens it in your terminal, and RET
while it's still working offers to take it over. Each unopened run
keeps the row in Needs you, and RET opens them newest first. The view
(`v`) lists every run (`●` unopened, `quiet` found nothing); RET opens
one (`varde attach <task> --run <run>`).

Nobody is around to approve tools, so a scheduled session runs on what
varde grants its agent, or, given one, in a sandbox, where it asks for
nothing (see [The sandbox](#the-sandbox-beta)): `varde set <task>
--sandbox default`. One that needs a login a sandbox hides (`gh`, `az`)
gets one that shows it: `push`, or one of yours. For the same reason, a first start that would ask about a
worktree needs `--worktree always|never` up front.

Each run of a repeating task is a fresh conversation given the cairn,
so put what to do in the context and let the cairn track progress. A
run waits for the previous one's cairn update, and stops an idle
session of it after 15 minutes (one you're in is left alone). `d` ends
the current run, not the series: its session stops and what it found
counts as read. The row shows `↻` while a run is working or waiting.
To end the series, take the schedule off (`varde schedule <task>
--off`, or `e` and clear `When:`), then `done`. `park` and `drop` pause
the schedule; `reopen` resumes it.

### Loops, checks and trails (beta)

A **check** is how varde knows a task's work is good rather than being
told: a command it runs itself in the task's checkout, a review by
another model, or both.

```sh
varde check fix1 -- cargo test -p parser    # passes when it exits 0
varde check fix1 --review --rubric review.md  # then another model reads the diff
varde check fix1 --verify 'run it and use /verify-app'  # then an agent checks it, with its tools and skills
varde check fix1 --guard 'tests/*'            # read-only to its sessions; a change is flagged
varde check fix1 --now                        # run it here and see
varde approve fix1                            # you say it is good
```

`--verify` is the strongest check for work a command cannot judge: a
fresh session on the review profile, in the task's sandbox, with its
tools and your skills, asked what you wrote (a file or the text). It
ends with PASS or FAIL, and what it touched to look is put back. A
skill is the how (`/verify-app` knows how to test your app); varde is
when it runs, on which model, in which sandbox, and what its answer
means for the loop. The same goes for a step's prompt: name the skill
(`/spec`, `/simplify`) and every round runs it.

A **loop** runs a task in fresh background sessions until its check
passes. Each round gets the task's cairn and how the last check failed.
The board says what it is doing and what its check said
(`↺ working ✗ 2 failed`, `↺ checking`, `↺ next round on deep`); the
rounds are a budget, named there only on the last (`· last round`).

```sh
varde loop fix1 --check 'cargo test' --max 3 --escalate deep
```

It stops when the check passes (`✓ verified`, in Needs you), after
`--max` rounds or `--minutes`, when two rounds fail the same way with no
change and no profile is left to escalate to (`■ stuck`; two rounds
failing the same way move it up one of `--escalate`'s, even when they
changed something, and two rejections by the review count as the same
way), when a round
asks you something (`? asked you`), or when a round touches the guard
(`⚑ touched tests/a.rs`). If a guarded test was wrong, fix it yourself
and give the guard again (`varde check fix1 --guard 'tests/*'`): it
takes the tests as they are now. `varde show fix1` lists every round. Each is
an ordinary session of the task, so RET in the view opens any of them.

`varde follow fix1` (`f` on the boards) watches a task at work without
taking it over: what each round says, the tools it calls, each check as
it lands, and the next round as it starts.

Opening the task (RET, `attach`) takes the loop over: your session is
the round now (`↺ with you`). Each of your turns that changes files
is checked when it ends, and a failure comes back to you in the session.
When you leave, the next round starts in the background.
`varde loop fix1 --pause` stops it; `varde loop fix1` sets it going again.

A **trail** is tasks joined by `after`: a task starts once what it
waits on is verified, or approved with `--approved`.

```sh
varde new "Store layer" --after spec --approved
varde trail spec                 # draw it
varde trail spec --start         # set it going: each starts in the background when ready
```

A step with one upstream that nothing else follows works in its
upstream's worktree; others get a worktree cut from their upstream's
branch. varde merges nothing and lands nothing.

**Profiles** are an agent, a model and an effort: `quick` (haiku),
`build` (sonnet), `deep` (opus, high) and `review` ship with varde, and
yours go in the config. `varde profiles` lists them. `--profile` on
`new`, `start` and `attach` picks one.

**Recipes** are loops and trails written down once. `varde recipe
list` shows them, `varde recipe show bugfix --origin` draws one and says
where each value came from, and `varde recipe edit bugfix` gives you
your own copy. Built in:

| Recipe | What it does |
|---|---|
| `fix-until-green` | fresh rounds until a command passes |
| `bugfix` | a failing test you approve, fixed until green without touching tests, reviewed |
| `feature` | a spec you approve, built until green and reviewed, then simplified |
| `epic` | a spec and a plan of pieces you approve; the pieces built at once, each until green, then merged and checked whole |
| `change-plan` | a live system looked at read-only; you get `CHECKLIST.md` and make the changes yourself |
| `draft-revise` | writing: a draft, critiqued against your rubric, revised |
| `research` | a question explored on the deepest profile; a report |

```sh
varde new "Parser crashes on empty input" --recipe bugfix --var check='cargo test' --start
varde new "Login race" --recipe bugfix --set fix.output=draft-pr
varde new "Sync engine" --recipe epic --var check='cargo test' --start
```

With `epic`, the plan step writes `SPEC.md` and `trail.toml`, a recipe
of its own, and loops until `varde recipe check trail.toml` passes.
`varde show` on the plan says where both are, and `varde recipe show
<path>/trail.toml` draws the trail. `varde approve` on the plan makes its
tasks, in the plan's sandbox, and sets them going. Pieces that need
nothing of each other run at the same time, each in a worktree cut from
the plan's branch, and the last step merges their branches and runs the
whole check. At most `loops.parallel` (3) background sessions run at
once, across all your trails.

To land it, merge the last step's branch (`varde show` names it) as you
would any: it holds every piece, `SPEC.md` and `trail.toml` (delete
those if you don't want them kept). Then `varde done` each task of the
trail; done puts the pieces' worktrees away.

`varde recipe guide` prints every key with its default. It is written
for agents too: ask any session to "write a varde recipe that…" and it
finds the guide by itself. The Claude plugin also brings a skill,
`varde-loops`, so a session asked to "loop this until the tests pass" or
"split this feature" knows to set it up with varde; for pi, link it:
`ln -s "$PWD/cli/plugin/skills/varde-loops" ~/.pi/agent/skills/`. `varde recipe check FILE` says whether a
recipe is usable.

Loops need the heartbeat (`varde schedule --install`) to be sure the
next round starts; varde also wakes itself when a round, a check or a
cairn update ends. Install it from the shell your agent works in: it
carries that shell's `PATH` and the agent's provider variables
(`CLAUDE_CODE_USE_VERTEX`, `ANTHROPIC_*`, `AWS_*` and the like), and
says which secrets it left out. At most `loops.parallel` (3) background
sessions run at once.

### The sandbox (beta)

Nobody is at a background session to approve its tools. A task that asks
for a sandbox (`varde new --sandbox default`, `varde set <task> --sandbox
NAME`, or `sandbox = "…"` on a recipe step) runs it, and its check, in
the system's sandbox: bubblewrap on Linux (install `bubblewrap`),
`sandbox-exec` on macOS. It is yours to opt into: `sandbox.on = true`
puts every task in one unless it says `off`. Inside, the agent may use
any tool without asking. It can still only:

- write its checkout, its session files, temporary files and build caches;
- commit, unsigned (sign them as you land them, `git rebase -S`, if your
  repository asks for it). In a task with no worktree it changes files
  but cannot commit, since the repository's git directory decides what
  runs as you.

And it cannot:

- read your logins: ssh, gpg, cloud CLIs, gh, git credentials, browsers,
  or the tokens in your environment;
- reach what runs things outside it: your service manager, tmux, an ssh
  agent, docker, and on macOS launchd and other apps;
- touch varde's store. Each `varde` it runs is handed to the varde
  outside, which does only what a session may: hooks, `note`, `recall`,
  `new` (in its own project and sandbox), and reading its own project.
  Changing a check, a loop or a sandbox, approving, scheduling and
  starting things are yours.

The network stays open, since the agent needs its model. A task that
asks for a sandbox where none can start does not run without one: it
stops, saying why. A task made from inside a sandbox is always in it.
`varde sandbox`
shows what it allows; `varde sandbox -- CMD` runs a command in it, to
see; `varde doctor` checks that it, and the agent in it, start.

A task or a recipe step picks its sandbox: `default`; `push`, which
shows git's logins and your ssh agent (a `draft-pr` step gets it when
every task is sandboxed); `off`; or one of yours. On macOS, check that `push` can push with
`varde sandbox push -- git push --dry-run`.

```toml
# ~/.config/varde/config.toml
[sandboxes.azure-read]
write = ["~/.azure-reader"]                 # shown and writable: az refreshes its tokens there
env = { AZURE_CONFIG_DIR = "~/.azure-reader" }
pass = ["AZURE_SUBSCRIPTION_ID"]            # variables it keeps that look like a login
deny = ["az * create*", "az * delete*", "az * update*", "az * set*", "az deployment *", "terraform apply*"]
```

```sh
AZURE_CONFIG_DIR=~/.azure-reader az login       # once, as an identity with the Reader role only
varde sandbox azure-read -- az group list       # see what it sees
varde new "Move the app to the new subnet" --recipe change-plan --var sandbox=azure-read --start
```

The plan looks with the reader's login and ends in `CHECKLIST.md`: each
change with its command, how to check it, and how to undo it. You make
the changes. `deny` keeps the agent from a command it might run by
mistake, but a script can still run one, so the real limit is what the
login may do: give it the Reader role only.

Without a sandbox (the default, or with `sandbox.on = false`), a
background session runs on what varde grants its agent. Claude may change files in
its checkout, commit and run the check, and is refused everything else.
pi asks before nothing, so it does not run in the background without a
sandbox, unless you give the task the sandbox `off`.

A round that leaves a git repository of its own in its checkout is
flagged and its loop stops, set aside as `*.varde-held`: git you run
there (your prompt, magit) would run what that repository's config
names. So does a link it leaves out of the checkout to where a sandbox
writes (a cache, temporary files, a git directory), which is removed. Look at it before you rename it back. Set
`git config --global safe.bareRepository explicit` too (`varde doctor`
asks): it keeps git from running a bare repository it finds, which work
you land could carry as well.

What it is not: a wall against a program set on getting out. A build
cache it writes (a crate's source) is what you build from next, and the
checkout's own scripts (`Makefile`, `package.json`) are yours to read
before you run them. On macOS it is weaker than on Linux: sandbox-exec
allows what it does not name, so a program set on it can still reach
launchd and the keychain's service; and a round's processes can outlive
it. On macOS, a repository under `~/Documents`,
`~/Desktop` or iCloud Drive is closed to the heartbeat's sessions
unless you give `varde` Full Disk Access; keep projects elsewhere.

## The board

`varde` opens the board, kept to the project you're in (`.` shows
every project). The footer shows the keys the row needs most; `?`
shows them all.

| Key | Does |
|---|---|
| `RET` | Go to the task's session: start it, resume it, or go to where it runs. |
| Tab / S-Tab | The next / previous task that needs you. |
| `s` | In tmux, start it in a window of its own and stay on the board. |
| `R` / `F` | Start a fresh conversation / fork the latest. |
| `n` / `e` | New task / edit its title, project and context. |
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
| `L` | Loop the task until a command passes; on a loop at work it pauses, paused it resumes. |
| `c` / `C` / `O` | Set its check / run it now / show what it last printed. |
| `A` | Approve its work. |
| `t` | Its loop, check and trail. |
| `f` | Follow it at work, read-only. |
| `M` | The profile it runs on. |
| `r` | New tasks from a recipe, set going. |

**The new-task form.** `n` opens it in the context, since most tasks
are just that; leave the title empty and a model names the task. Above
the line are `Title:`, `Project:` and `When:` (Tab/Shift-Tab between
them). RET on a field above the line, or C-s anywhere, creates the
task; C-j is RET. C-x C-e edits the whole form in `$EDITOR`. Esc puts
the draft away and the next `n` brings it back. `e` opens the same form
on a task; changing the Project moves it. Prompts take shell editing
keys (C-w, C-u, C-k, Alt-b/f, C-y).

### The project picker

Every project question uses one picker: the form's Project (RET or
typing opens it, Backspace empties it; left empty, it's asked on
create), `m`, and a first start whose project is unclear. It lists
known projects by name and directory, newest first, with the likeliest
picked at the top (the form's, else the selected task's, else the
board's); `m` puts the task's own project last. Typing narrows it; ↑/↓
or C-p/C-n choose, RET picks, Esc goes back. Type a path (`~`, `/` or
`.` first) to browse directories instead: Tab completes, RET takes it.
Tab on a project goes into its directory, and `another directory…`
starts at `~/`.

### The view

`v` opens a pane with two tabs: **Task** (cairn, waiting notes,
context, sessions) and **Project** (the project's cairn). `›` marks
your lines; a yellow `?` marks a line about code that's gone. Board
keys work on the task in view.

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

A half-typed line is never lost: Esc puts it away, and the same key on
the same line brings it back.

### With tmux

Without tmux, RET hands the session the terminal and the board comes
back when the agent exits. With tmux (3.0+), sessions run side by side.
Keep the board in a popup (tmux 3.3+; on 3.2, drop `-B`) and bind
`varde next`:

```sh
# ~/.tmux.conf
bind-key b display-popup -E -B -w 90% -h 85% "varde --popup"  # prefix b: the board
bind-key N run-shell -b 'varde next'                           # prefix N: the next task that needs you
bind-key B run-shell -b 'varde next --board'                   # prefix B: the board
```

Or run the board in its own window (`tmux new -s varde varde`). Each
session gets a window named after its task that rings the bell when
it needs you; prefix `l` goes back to the board. Closed tasks' windows
close once their shell is idle. `run-shell` uses tmux's environment, so
`varde` must be on tmux's PATH.

## Cairns

Cairns are short because every session gets one: at most 200 words,
a Status (30 words), up to 7 Findings, 5 Decisions and 5 Open items
(40 words each). A project cairn has Conventions, Gotchas and How-to
(5, 7 and 5 lines).

**Updates.** A task's cairn is updated when a session ends, before it
compacts, at most every 30 minutes while it runs, and once more on
close. It's a detached agent call (`claude -p` on haiku, or pi's
default) with no tools, reading only the new part of the transcript.
It answers with edits, checked against the caps and filtered for
secrets, URLs and commands. The project cairn is only ever written by
you. The agent can pin a finding with `varde note`.

**Editing.** Lines you write are yours: updates never change or drop
them. Edit in the board's view, in Emacs, or by the ids `varde show`
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

`varde show --history` keeps every earlier version.

**Stale lines.** A line naming a path or identifier that's gone from
the code shows `(stale?)`; the next update fixes or drops it.

**Delivery.** Via the hooks, a fresh session gets the project cairn,
its task's cairn, the task it was deferred from and related lines; a
resumed one gets what changed. About 500 words at most, framed as
facts to check, not instructions.

**Recall.** `varde recall` (`/` on the board) searches cairn lines,
removed lines, notes and project cairns, one hit per task.

**Evaluation.** `varde eval` shows, over 30 days, how much of what
sessions were shown got used, what was repeated or stale, and what
updates cost. `memory.holdout` withholds memory from a share of fresh
sessions, for comparison.

## Configure

Nothing needs configuring. `~/.config/varde/config.toml` (or
`$VARDE_CONFIG`) changes the defaults:

```toml
agent = "claude"                  # or "pi"

[agents.claude]
command = ["claude"]              # e.g. a wrapper that ends in: exec claude "$@"
background = []                   # more arguments for a background session; varde's grant comes after

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

[profiles]                        # an agent, a model and an effort, by name
# deep = { model = "opus", effort = "xhigh" }
# review = { agent = "pi", model = "openai/gpt-5", effort = "high" }
# On Vertex or Bedrock, pin what an alias means in your shell's profile,
# before varde schedule --install: export ANTHROPIC_DEFAULT_SONNET_MODEL=claude-sonnet-4-6
# (a model your project serves; varde doctor says which aliases need it)

[roles]
# work = "build"                  # what a task's sessions run on; default: the agent's own
review = "review"                 # what a check's review runs on

[loops]
parallel = 3                      # background rounds and checks at once
check_minutes = 30                # a check that takes longer is an error

[sandbox]
# on = true                       # every task in a sandbox unless it says off; false: none,
                                  #   whatever a task asks; left out: only tasks that ask
write = []                        # more every sandbox may write, e.g. "~/.m2"
hide = []                         # more logins every sandbox hides

pass = []                         # variables every sandbox keeps that look like a login

# [sandboxes.NAME]                # yours: show, write (shown too), env, pass, deny

[keys]                            # rebind a board key by action
# park = "p"
# quit = "esc"
```

The actions are `start`, `background`, `fresh`, `fork`, `next-needs`,
`prev-needs`, `new`, `edit`, `done`, `park`, `drop`, `reopen`, `accept`, `move`,
`mark`, `view`, `cairn`, `editor`, `diff`, `recall`, `down`, `up`,
`first`, `last`, `next-group`, `prev-group`, `half-down`, `half-up`,
`fold`, `scope`, `refresh`, `quit`, `help`, `loop`, `check`, `check-now`,
`check-output`, `approve`, `trail`, `profile` and `recipes`.

Per-repo settings live in git config, so nothing committed ever runs:

```sh
git config varde.worktree always       # its first runs get a worktree
git config varde.base develop          # the branch worktrees start from and land in
git config varde.setup "pnpm install"  # run in each new worktree
git config varde.check "cargo test"    # what a loop or recipe checks with, unless told
```

**What varde trusts.** It only runs what you set: the agent `command`
in your config, checks you set with `varde check`, `varde loop` or your
recipes, and `varde.setup` and `varde.check` in a repo's local git
config. Memory
is written by agents, so treat it as agent output; model-written lines
are filtered for secrets, URLs, piped commands and orders to sessions.
Your own lines aren't filtered.

**Files.** The store is `~/.local/state/varde/varde.db` (`$VARDE_HOME`
moves it), readable only by you, and it holds your prompts. Sessions
varde launches carry `VARDE_RUN`; its own agent calls carry
`VARDE_INTERNAL=1`, which the hooks ignore.

## Troubleshooting

Start with `varde doctor`.

- **No sessions on the board, or status never changes.** The hooks are
  missing or old: reinstall the plugin.
- **Another `varde` on the PATH.** `doctor` says which.
- **`varde next` does nothing.** It's meant for a tmux key, with
  `varde` on tmux's PATH.
- **A setting is ignored.** Invalid settings are skipped with a notice.

## Upgrading and uninstalling

Upgrade by installing the new binary; the store migrates itself. Keys
that moved since 1.0 are in [`CHANGELOG.md`](../CHANGELOG.md).

To uninstall: `/plugin uninstall varde@varde` (or remove the pi link),
delete the binary and `~/.local/state/varde`, and in each repo remove
leftover `.worktrees/`, the `refs/varde/` refs, and the `.worktrees/`
line in `.git/info/exclude`.

## Clients

`list`, `show`, `new`, `start`, `edit`, `done`, `park`, `drop`,
`recall`, `eval`, `doctor` and `agent` take `--json`; see
[`clients.md`](clients.md) to write your own client.
