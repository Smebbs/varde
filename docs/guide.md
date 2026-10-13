# varde

varde runs AI coding-agent sessions (Claude Code or pi) as tasks, in
any terminal. This is the guide to the CLI and the terminal board. The
top-level [`README.md`](../README.md) says what varde is for,
[`emacs.md`](emacs.md) covers the Emacs client, and
[`varde-design.md`](varde-design.md) says why it is built this way.

## In one minute

- A **task** is a piece of work you give an agent. Its **sessions** are
  the agent's conversations about it: starting a task again resumes
  its last one, or goes to it while it runs.
- Its **cairn** is what its sessions learned, handed to the next one: a
  Status line, and short lists of what was found, decided and left
  open. Each project keeps a cairn of its own, for what holds across
  its tasks. You read a cairn, and fix a line of it, on the board.
- The **board** groups tasks by what they need from you:

  | Group | Holds |
  |---|---|
  | Needs you | a live session whose turn ended (**your turn**), or that asks you something mid-turn (**needs input**) |
  | Working | a live session at work |
  | Open | tasks with no session running |
  | Inbox | tasks you did not create: sessions you started yourself, and work an agent left for later at your word |
  | Done | tasks done today, and older ones whose work has not landed yet |
  | Parked | tasks put aside (folded) |
  | Dropped | tasks dropped: not on the board; `varde list --all` lists them |

  A row's glyph says the same at a glance: `●` live (green working,
  yellow your turn, red needs input), `○` nothing running (or your
  turn, once you have looked), `✓` done, `‖` parked, `✕` dropped.
  Every group is always there, in the same order, so nothing moves
  under you: an empty one is a dim line, `Working  0`.
- When you have finished with a task, it is **done** (`d`), **parked**
  for later (`P`), or **dropped** (`x`): the work in its worktree is
  kept under a git ref, and starting it again brings it back. A dropped
  task is found with `varde list --all`, or by recall (`/`, where `s`
  on its hit reopens and starts it); `varde reopen ID` puts it back in
  Open. Inbox tasks are **accepted** (`a`) into Open, or started, done
  or dropped.
- **Moving between sessions** is the board's job. Tab goes to the next
  task that needs you, and RET into its session. Inside tmux each
  session has a window, and `varde next` on a tmux key goes to the next
  one from anywhere.

## Install

varde needs git, and Claude Code or pi. tmux 3.0 or later is
recommended: inside it, each session gets a window of its own.

Install it with Homebrew (macOS or Linux):

```sh
brew install smebbs/tap/varde
```

Or take the binary for your platform from the
[latest release](https://github.com/Smebbs/varde/releases/latest) and
put it on your PATH, or build it with Rust 1.88 or later:

```sh
cargo install --locked --git https://github.com/Smebbs/varde varde
```

`cargo install` puts it in `~/.cargo/bin`. If your shell then says
`varde: command not found`, that directory is not on your PATH (on
macOS too): add `export PATH="$HOME/.cargo/bin:$PATH"` to `~/.zshrc`
(or your shell's profile) and open a new terminal. The agent's hooks
call `varde` by name, so it must be on the PATH before they run.

In Claude Code, install the plugin. It contains only hooks, and each
one calls `varde hook <Event>`:

```
/plugin marketplace add Smebbs/varde
/plugin install varde@varde
```

For [pi](https://pi.dev), which has no shell hooks, link the extension
from a clone of this repository instead. It only forwards pi's session
events to `varde hook --agent pi`:

```sh
mkdir -p ~/.pi/agent/extensions    # or $PI_CODING_AGENT_DIR/extensions
ln -s "$PWD/cli/pi/varde.ts" ~/.pi/agent/extensions/varde.ts
```

Then run `varde doctor`. It checks the store, that the `varde` on your
PATH is this one, the config file, the agent new sessions use, each
agent's hooks, pending events, memory updates that failed, and
worktrees whose directory is gone. Each failure says how to fix it.

## Use

```sh
varde                        # the board (outside tmux: tmux new -s varde varde)
varde --popup                # the board in a tmux popup, which closes once you go to a session
varde new Fix the login bug  # a task in the checkout you are in (--context TEXT: what the agent is told with it)
varde new --context "$(cat issue.md)"  # no title: one is cut from the context, and a model names the task
varde attach k3m9qz          # start it here, resume it, or go to where it is running
varde attach k3m9qz --fork   # branch off its conversation instead (--fresh: a new one)
varde next                   # in tmux: the window of the next task that needs you
varde done k3m9qz            # close it; its worktree goes once its work has landed
varde park k3m9qz            # put it aside, worktree kept; starting it again picks it up
varde drop k3m9qz            # drop it; its work is kept, and starting it again brings it back
varde reopen k3m9qz          # back to Open from done, parked or dropped, without starting it
varde accept k3m9qz          # an Inbox task into Open
varde set k3m9qz --project ~/src/api  # move it to another project
varde show k3m9qz            # where it stands, its cairn with each line's id (#12), its sessions
varde edit k3m9qz --add findings The cache key omits the locale.  # a line of your own
varde recall locale cache    # what earlier tasks learned (this project; --all for every one)
varde show --project         # the project cairn: what holds across this project's tasks
varde eval                   # what memory was worth here (--all, --since 30d, --json)
varde list                   # the board as text (--all: old done and dropped tasks too; --project: this project only)
```

Refer to a task by the six characters the board shows; its first four
are enough while no other task shares them. Inside a session varde
runs, `done`, `park` and `show` with no task mean the session's own.

**Which project.** A task belongs to the checkout you make it in: the
repository, or a worktree of your own (`git worktree add`), which is
then a project of its own, so the agent edits that tree and not the
main checkout. A session in a task's worktree that varde made belongs
to the repository it was made from.

**Moving a task.** `varde set <task> --project DIR` (or `m` on the
board) moves a task to the project DIR is in. It is refused while the
task has a live session, an active worktree, or work kept from a drop,
since those belong to the old checkout: end the session, and let the
worktree's work land, first. The task's past runs keep the directories
they ran in and its cairn stays with it. Its next start is a fresh
conversation in the new project, given that project's cairn: a
conversation only continues where it ran.

**What happens when a task starts again.** varde chooses from what it
knows about the task:

- **Running in a terminal varde launched:** inside tmux, varde goes to
  its window. Otherwise it names the window or terminal it runs in, and
  the run `--fork` would branch off, instead of starting a second
  session.
- **Live, but started outside varde:** varde goes to its terminal when
  its hooks told it which one. Otherwise it forks it, since a second
  front end cannot attach to a running session.
- **Finished:** varde resumes it, in the directory it ran in. A session
  started outside varde counts as finished once its process is gone,
  even when its terminal closed without telling anyone.
- **Otherwise:** varde starts it fresh. On a first run in a repository
  it asks whether the run gets a worktree (`Start "Fix the login" in a
  worktree of its own (app)? [Y/n]`; the board asks the same, with
  `Y/n · Esc cancel`, and Emacs with y/n), unless `--worktree always|never` or a setting below says
  so.

Starting a done, parked or dropped task reopens it.

**Worktrees.** A task's worktree is `.worktrees/<slug>` in its repo,
on branch `varde/<slug>`, cut from your local base branch. All three
are settings (below). A task made in a worktree of your own starts from
that worktree's branch, and its work has landed once it is in that
branch; its worktree is still kept in the main checkout's
`.worktrees`, not inside yours. varde makes a worktree ready the way
Claude Code does its own:

- Submodules are checked out when the repo has a `.gitmodules`.
- Ignored files named in `.worktreeinclude` at the checkout's root
  (gitignore syntax, e.g. `.env`) are copied from that checkout. Files
  git would track are never copied.
- `git config varde.setup` runs a command of yours (below).

**When a worktree goes.** `done` asks nothing. The worktree and its
branch are removed once nothing in them can be lost: no uncommitted
changes, every commit in the base or in a PR merged at the branch's tip
(squashed too), and no session of the task still running. Until then
the task stays in Done with its stage shown, as a reminder that its
work has not landed. Removal deletes ignored files, as `git worktree
remove` does. A worktree whose `.worktreeinclude` files differ from the
checkout's is kept until they match or you remove it (`varde done
<task> --remove --force`, which names the files it deletes). `drop`
keeps everything else under `refs/varde/discarded/<slug>`. Outside a
git repository a task simply runs in its directory.

**Sessions you start yourself.** A session started with plain `claude`
or `pi` becomes an Inbox task after its first prompt, titled by that
prompt's first words. Its first memory update that can tell what the
task is about then names it in a few words. A title you set first
(`varde set --title`, `e` on the board) is kept, and so is the title of
a task another client links (`varde link`); `varde show` keeps the
first prompt's words beside the new title. While the session runs it shows under Needs you or Working,
as every live session does, and once it ends it sits in the **Inbox**
until you act on it: `varde accept`, starting, closing, parking or
dropping it. A session started by a script (`claude -p`, the Agent
SDK) is not tracked.

## The board

`varde` with no command opens the board. A `●` marks a task at work,
or one that needs you whose latest turn nobody has looked at yet; `○`
one seen or idle, `✓` a done one and `‖` a parked one. Under Needs you
a row says *needs input* when the session asks you something, and
nothing more when its turn simply ended: the group says it is yours. A
deferred task's row ends with `← <short>` of the task it came from.

Opened inside a repository, the board keeps to that project's tasks,
once it has one on the board. Its title names the project and says how
many tasks are in other projects, and how many of those need you. `.`
switches between this project and every project. A task you make, move
or pick in recall in another project is shown all the same: the board
then shows every project. Opened outside a repository, it shows every
project. `[board] scope = "all"` in the config opens it on every
project instead.

The keys are vi's where vi has one, and the Emacs board's wherever
Emacs allows ([emacs.md](emacs.md) lists the differences). The footer
shows the ones the row at point needs most, as many as fit, and none
that would do nothing to it; `?` shows them all, under what each group
holds and each glyph says. Taller than the screen, `?` scrolls with
`j`/`k` and C-d/C-u, and any other key closes it.

| Key | Does |
|---|---|
| `RET` | Go to the task's session: start it, resume it, or go to where it runs. On a folded group, open it. |
| Tab / S-Tab | The next / previous task that needs you, round to the first. On the board as it opened, the first Tab stays on the first. |
| `s` | Inside tmux, start it in a window of its own and stay on the board. |
| `R` / `F` | Start a fresh conversation / fork the latest one. |
| `n` | New task: the form below. |
| `e` | Edit the task's title and context, in the same form. A task linked elsewhere, which owns its text, opens its cairn in `$EDITOR` instead. |
| `d` / `P` / `x` | Done / park / drop. `x` asks first. `d` and `P` ask only when a session is at work (working, waiting for your answer, or with agents or shells its last turn left running): `y` stops it, `n` leaves it running, Esc cancels. The selection then goes to the open task nearest it, and the message says `u reopens`. |
| `u` | Reopen the tasks last closed on this board with `d`, `P` or `x`, the selection on them. In the view, `u` brings back the line `x` removed, or makes a Status `x` handed back yours again; with neither, it reopens the task last closed there. Each undoes its own, as vi's does per buffer. |
| `a` | Accept an Inbox task. |
| `m` | Move the task to another project: asks for a directory, its project filled in. Tab completes a directory, and more Tabs (or S-Tab) pick from the list. |
| space | Mark the task; `a`, `d`, `P` and `x` then act on every marked one, and `x` asks once, naming how many. Esc unmarks. |
| `v` | The view: the task and its cairn, and on its other tab its project's cairn (below). `v` again closes it. |
| `E` | Edit the whole cairn in `$VISUAL` or `$EDITOR`: the project's on the view's Project tab, else the task's, its context above it. |
| `/` | Recall in this project. In its pane, `j`/`k` pick a hit, RET (or `v`) opens the view on it (on the Project tab for a project's cairn), `s` starts it (asking first to reopen it if closed), and `.` switches between this project and every project. The keys that act on a task (`d`, `P`, `x`, `a`, `e`, `m`, `R`, `F`, `E`, `=`) act on the hit picked, never on the row the pane hides; a project cairn's hit has no task, so its footer leaves them out. RET on it opens the Project tab on a task of that project, or says there is none on the board. A hit says how many more lines of its task matched, and a closed task's state (`(done)`). |
| `=` | The task's diff, new files included. For a task without a worktree, what is uncommitted in the checkout it shares. |
| `j` / `k` | Next / previous task; the arrows too. |
| `gg` / `G` | First / last task; Home and End too. |
| `{` / `}` | Previous / next group. |
| C-d / C-u | Half a page down / up: the open pane, else the list. |
| `z` | Fold the group at point, or open it again on the task you folded it from. Parked starts folded. A folded group is one line the selection stops on. |
| `.` | Keep to the project the board was opened in, or show every project. |
| C-l | Redraw the screen and read the board again. |
| `q` / Esc | Close the pane, else quit. Esc never quits, unless `[keys]` binds it to (`quit = "esc"`, below) or the board runs in a popup (`--popup`, below). |

**The new-task form.** `n` opens it on the title; Tab (or Down) moves
to the context below it. RET on the title, or C-s, creates the task in
the project the board keeps to; showing every project, it asks for the
project first, offering the one of the task at point. With the title
left empty, C-s creates the task from its context alone: its title is
cut from the context's first words, as a session's is from its first
prompt, and a model names it in the background. C-x C-e (or
Alt-e) writes it in `$VISUAL` or `$EDITOR`, the title on the first line
and the context below, and comes back to the form. Esc or C-g puts the
form away with what you wrote: the next `n` (or `e` on the task, for
an edit) brings it back, as often as you put it away. To throw a draft
away, clear it (C-u) and Esc. The new task and each edit keep a draft
of their own, and every draft outlives the board.

**Typing.** Every prompt of the board edits text as a shell does: the
arrows, C-b/C-f, C-a/C-e and Home/End move; Alt-b/Alt-f and
Ctrl-arrows move by word; C-w rubs out back to the last blank, as
readline's does, and Alt-Backspace to the start of the word (both a
directory, in a directory prompt); C-u to the line's start, C-k to its
end and Alt-d the next word; C-y puts back what was last rubbed out.
Esc and C-g leave the prompt. Pasted text only ever goes into a prompt.
On macOS, the Alt keys need the Option key to send Meta: "Use Option
as Meta key" in Terminal (Profiles, Keyboard), or "Left Option key:
Esc+" in iTerm2. Cmd-Backspace sends C-u in many terminals, so it
rubs out to the line's start, as it does elsewhere on a Mac.

Starting a done, parked or dropped task asks first, since it reopens
it. Nothing that fails in the background closes the board: errors and
notices go to its message line.

### The view

`v` opens the view: a pane under the board, the task's title in its
border, which has the keys while it is open; the board dims under it,
as it does under the new-task form and `?`. The pane is as tall as
what it shows, up to most of the screen, so a short cairn leaves the
board in view; on a screen under 30 rows, or in a popup, it takes all
but three rows of the board, around the selection. While there is
more below, its bottom border says
`↓ C-d more`. It has two tabs. **Task** shows where the task stands
(its status, its project, its worktree's branch), its cairn, the notes
waiting for the next update, then its context and its last sessions, a
line each (`7njqys  2m · ended`; `varde show` has the rest). **Project**
shows its project's cairn, the project's name in the border. `[` and
`]` switch them.

Under the tabs, one dim line says where the task stands, then when
the cairn was last updated, how many sessions were handed it, and how
many of its lines are stale. A
section shows how full it is only once it is full or one short
(`Findings 6/7`). Your own lines, the Status too, are marked `›`
instead of `-`, and a line whose code is gone ends in a yellow `?`. A
cairn with nothing in it yet is a line saying how it gets one, and its
sections' headings, dim.

The view takes the keys that work on a line. Every other key does what
it does on the board, for the task in view and never the marked ones
(the footer says how many are marked): RET goes to its session, and
`s`, `d`, `P`, `n`, `/`, `?`, `.` and the rest work as above.

| Key | Does |
|---|---|
| `[` / `]` | The Task / Project tab. |
| `j` / `k`, `gg` / `G` | Pick a line: the Status, a section's heading, or one of its lines. `G` also shows the end of the view. |
| C-d / C-u | Half a page down / up, the picked line kept in view. |
| `e` | Edit the picked line; on the Status, the Status; on a heading, add a line under it, as `o` does. |
| `o` | Add a line to the section at point; on the Status, as `a`. |
| `a` | Add a line: a key picks the section (`f` Findings, `d` Decisions, `o` Open, `s` Status; in a project cairn `c` Conventions, `g` Gotchas, `h` How-to). A full section says so at once. |
| `x` | Remove the picked line, without asking. On a Status you set, hand it back to the updates: its words stay, and the next update may change them; `e` or `u` makes it yours again. |
| `u` | Bring back the line last removed, where it was, or make the Status last handed back yours again. With neither to undo, reopen the task last closed here with `d`, `P` or `X`. |
| `X` | Drop the task in view, asking first, as `x` does on the board. |
| `E` | The whole cairn in `$VISUAL` or `$EDITOR`, for changes to many lines. |
| `J` / `K` | The next / previous task, past the groups' headers, opening a folded group; the view kept open. Tab and S-Tab go to the next task that needs you, the view kept open too. |
| `q` / Esc / `v` | Back to the board. With tasks marked, Esc unmarks them first. |

A line is typed on the prompt line under the board, as a recall or a
directory is (`Findings · new line:`, `Edit line #12:`, `Status:`),
its words counted at the right against what it may have (`12/40`),
while the line it changes stays highlighted in the view. RET saves it.
A line the caps refuse stays open, saying why over it. Esc (or C-g)
puts a line you were typing away, one for each line, saying for which
task. The view shows it, dim, where it goes (`draft: … · o
continues`), and the same key on the same line (`e`, or `a` and its
section) brings it back, as often as you put it away. Cleared (C-u),
Esc drops it. A change to a line that has since been removed comes
back as a line to add to its section. Lines put away outlive the
board: quitting keeps them, and the next board names their tasks and
sections. Two boards on the same store
keep each other's drafts.

From the board, adding a finding to the task at point is `v`, `a`,
`f`, the line, RET, and `q` back; `o` in place of `a f` when the line
picked is in Findings, as it is when the view opens on a cairn with
findings.

### Inside tmux

The board is best kept a key away, in a tmux popup over whatever
window you are in (tmux 3.3 or later):

```sh
# ~/.tmux.conf
bind-key b display-popup -E -B -w 90% -h 85% "varde --popup"  # prefix b: the board
```

`-B` leaves out tmux's border: the board draws its own frame. tmux 3.2
has popups but not `-B`; leave it out there, and the board sits inside
tmux's border. The popup opens in the directory of the pane you are in,
so the board keeps to that project. With `--popup`, a key that takes
you to a session (RET, `R`, `F`, or `s` on a recall hit) quits the
board once it is there, and the popup closes on the session; `s` on a
row, and everything else, keeps the popup open. Esc on the bare board
(nothing marked, no pane or prompt open) quits it too, closing the
popup. Outside tmux `--popup` changes nothing. A board rings windows and closes those of closed tasks only
while it runs, so with only a popup that happens while it is open;
`varde next` (below) needs no board.

Or run the board in a window of its own: `varde` in any tmux window,
or `tmux new -s varde varde` from outside tmux. RET opens the task's
session in a tmux window of its own, named after the task's title (cut
short), with the board's store and `PATH`, and goes to it. RET on a running task goes to its window, and starting it
again after the agent exits reuses that window's shell. When a session
comes to need you, its window rings the bell, which tmux flags in the
status line (`monitor-bell`, on by default; `set -g monitor-bell off`
silences it). Going to the window, from the board or with tmux's own
keys, marks it seen. Once a task is closed, here or anywhere else, the
board closes the windows it opened for it as soon as their shell is
idle. A pane of your own where you ran `varde attach` is never closed.

tmux's own `last-window` (prefix `l`) takes you back to the board from
a session, and to the session again. `varde next` goes to the window
of the next task that needs you, round to the first, and to the board
once nothing else does; `varde next --board` goes to the board. Bound
to keys, you answer one session after another without the board:

```sh
# ~/.tmux.conf
bind-key N run-shell -b 'varde next'          # prefix N: the next task that needs you
bind-key B run-shell -b 'varde next --board'  # prefix B: the board
```

`run-shell` takes tmux's own environment, so `varde` must be on the
`PATH` tmux was started with, and the store the default one or
`VARDE_HOME` set there (`set-environment -g VARDE_HOME …`). What
`varde next` cannot do it says in tmux's status line: a task that needs
you but has no window yet is named there, and RET on the board opens
one.

### Outside tmux

The board runs one session at a time. RET asks on the board what a
first start needs (a worktree, a project), then hands the terminal to
the session, under a line naming the task, and the board comes back
when the agent exits (`/exit`, or C-d), saying how it ended. A session
that fails leaves its last words on screen until a key. A task already
running elsewhere stays on the board, which says where (`in tmux window
main:2`, `in another terminal (pts/7)`). `s` needs tmux, and
`varde next` only says it is for a tmux key, and exits 1. To run
several sessions at once and keep the board in view, use tmux.

## Cairns

A cairn is what a task's sessions learned, handed to the next one, so
a new session does not start from nothing and you do not explain the
same thing twice. It is short on purpose: every session is handed it.

**Its shape.** A task's cairn is at most 200 words: a Status line (30
words at most), and up to 7 Findings, 5 Decisions and 5 Open items, 40
words each. Each line keeps an id, and its exact words, until something
changes it.

**When it is updated.** A session's work goes into its task's cairn
when the session ends, before it compacts, and at most every 30 minutes
while it runs. Closing, parking or dropping a task updates it once
more, to say where it ended and settle what was left open.

**How.** A detached call to the run's own agent (`claude -p` on haiku,
or pi on its default model) reads only the transcript since the last
update, with varde's own system prompt and none of your settings,
`CLAUDE.md` or tools. It answers with edits: add, edit or drop a line
(with a reason), confirm one, or set the Status. varde checks them
before applying any. A reply that is not all edits is asked for again.
An edit that would break the caps, or that holds a URL, a command piped
into a shell, an order to sessions or a secret, is sent back once. A
line no edit names stays as it was.

**Why you edit it.** An update can get a fact wrong, keep one that no
longer holds, or miss what you know. A line you write is yours, Status
included: updates never change or drop it, and every later session is
handed it.

**How you edit it.** One line at a time, in the board's view (above),
in Emacs, or from a shell, by the id `varde show` prints after each
line:

```sh
varde edit k3m9qz --add findings The cache key omits the locale.  # findings, decisions, open; status sets the Status
varde edit k3m9qz --set 12 The cache key omits locale and region.  # change line #12
varde edit k3m9qz --rm 12          # remove it: recall still finds it
varde edit k3m9qz --restore 12     # bring it back, where it was
varde edit k3m9qz --rm status      # hand your Status back to the updates
varde edit --project --add gotchas 'launch.rs tests need tmux.'  # the project cairn
```

The text is the rest of the line, or `-` to read it from stdin. Each
change is one transaction, so an update landing meanwhile is never
undone. The caps hold for your lines too: a change past them is
refused, saying why, and so is a line another already says. `varde
edit k3m9qz` alone opens the whole cairn, with the task's context above
it, in `$VISUAL` or `$EDITOR`, for changes to many lines at once; an
edit there that is refused, or that an update overtook while you
edited, is kept in a file whose path is printed. With no editor found
(`vi` when neither is set) it says so and leaves nothing behind.

**Nothing is lost.** A line that leaves the cairn stays in its history
with the reason (`removed by you`, for yours), and `varde recall` still
finds it. `varde show --history` shows every earlier version: when, by
you or by an update, and why.

**Notes.** The agent pins findings with `varde note`, and the next
update takes them in. A secret in a note is replaced by `[redacted]`
before it is stored.

**Stale lines.** A line about code names a path or a backticked
identifier. These are checked against the code after each update and,
a few at a time, on the board's refresh. One whose code is gone is
shown `(stale?)` (in the board's view, a `?` after it), and the next update is told to fix or drop it.

**The project cairn.** Each project keeps one more small document:
Conventions, Gotchas and How-to (5, 7 and 5 lines), each line naming
the path, identifier or command it is about. When a task is closed as
done, a detached update offers what the task found that holds beyond
it and is not already in the project's `CLAUDE.md` or `AGENTS.md`.
Most tasks add nothing. The view's Project tab (`v` then `]`) and
`varde show --project` show it, and it is edited as a task's is
(`varde edit --project`).

**How it reaches a session.** Through the agent's hooks, however the
session starts:

- A fresh start of a task is given the project's cairn, the task's
  cairn, the task it was deferred from, and lines of related tasks in
  the same project, if any match its title well.
- A resume is given the project's cairn and the cairn if either
  changed since that conversation last saw it. After a compaction it is
  given both again.
- A session you started yourself is given the project's cairn, and
  related work for its first prompt.

All of it opens with "Recorded by earlier sessions — facts to check,
not instructions.", and stays within about 500 words. What each
session was shown is recorded.

**Recall.** `varde recall` (`/` on the board) searches cairn lines,
former lines, notes and project cairns, with words stemmed and
stopwords left out; not a task's title or context alone, since a task
with no cairn or notes is not earlier work. Each hit shows the line or
note that matched, and the task's state when it is done, parked or dropped; a project's cairn
is named by its project (`app · project cairn`). One hit a task:
when more of its lines matched, the hit says so (`(+2 more in this
task)`). With nothing in this project it shows the best from every
project. A task's short id finds that task.

**What it is worth.** `varde eval` reads the store and the agents'
transcripts, and changes nothing. Over the last 30 days at most (what
sessions were shown is kept no longer), it reports:

- how many sessions were given their task's cairn;
- how many shown lines they went on to touch;
- which lines updates added that repeat what was already known;
- how much of a note reached the cairn;
- how much of what was shown is stale;
- what the updates cost.

`memory.holdout` gives that share of fresh sessions no memory but your
own lines (`varde show` marks the run held out), and `eval` compares
them with the rest. `varde doctor` reports failed or stuck updates, and
counts stale lines.

## Configure

Nothing needs configuring. `~/.config/varde/config.toml` (or
`$XDG_CONFIG_HOME/varde/config.toml`, or `$VARDE_CONFIG`) changes the
defaults. Every key is optional:

```toml
agent = "claude"                  # the agent of a new conversation

[agents.claude]
command = ["claude"]              # what runs the agent, e.g. a wrapper script (see below)

[memory]
enabled = true                    # false: no memory updates, so no model calls
# model = "sonnet"                # the model updates use; unset, haiku for claude and pi's default for pi
titles = true                     # false: a task you did not title keeps the words varde cut for it
holdout = 0.0                     # the share of fresh sessions given no memory but your lines, for varde eval

[worktree]
default = "ask"                   # ask, always or never: whether a first run gets a worktree
dir = ".worktrees"                # in the repo, or e.g. "~/worktrees/{repo}"
branch_prefix = "varde/"

[board]
scope = "project"                 # project or all: whether the board opens kept to the project you are in

[keys]                            # the board's keys, by action
# park = "p"
# start = "o"
```

A file with a key varde does not know, or a value it cannot use, is
read without that setting, with a notice, and `varde doctor` reports
it.

**Keys.** `[keys]` binds a board key by the action's name: `start`,
`background`, `fresh`, `fork`, `next-needs`, `prev-needs`, `new`,
`edit`, `done`, `park`, `drop`, `accept`, `move`, `mark`, `view`,
`cairn`, `editor`, `diff`, `recall`, `down`, `up`, `first`, `last`,
`next-group`, `prev-group`, `half-down`, `half-up`, `fold`, `scope`,
`refresh`, `quit` and `help`. A key is one character, or `space`,
`tab`, `backtab`, `enter`, `esc`, `home`, `end`, `up` or `down`; `first`
bound to `g` is pressed twice, as `gg` is. `esc` can only be bound to
`quit`: with `quit = "esc"`, Esc still unmarks and closes a pane first,
and quits once there is nothing left to close. The key you bind does that action
in place of whatever it did; every other key keeps its default, and
the footer and `?` show the keys as bound. `cairn` has no key of its
own: bound, it opens the view on its Project tab. The Ctrl keys (C-d,
C-u, C-l) and the view's own keys cannot be bound. In Emacs, bind keys
in `org-varde-board-mode-map` as in any mode.

**Titles.** A task whose title you did not type, a session you start
yourself, one made from its context alone, deferred work, or one whose
title you cleared, is first titled with the first words it was started
with, then named by one small model call, made in the background with
the model the memory updates use: as the task is made, or, for a
session, at the board's next refresh. The board shows the new title on
its next refresh. A title you type is never renamed, a task is asked
about once, and a failed call keeps the cut words; `varde show` keeps
them too. `memory.titles = false` (or `memory.enabled = false`) keeps
the cut words as the title and makes no call.

**Which agent.** A new conversation uses `--agent claude|pi` on
`varde attach`, else `$VARDE_AGENT`, else `agent` in the config, else
`claude`. A resume or fork is by the agent whose conversation it
continues; asking for another is refused.

**The command runs memory updates too.** An update runs `command`
with the update's own flags after its arguments (`-p`, its system
prompt, no tools, and for claude `--model` with `memory.model` or
haiku), so a flag of yours that the update also sets is overridden,
and any other reaches it. Set the update's model with `memory.model`.

**A wrapper as the command** should end with `exec claude "$@"`, so the
agent takes its place. varde also finds an agent the wrapper runs as a
child, from the session's first hook, but until then it knows only the
wrapper, and it waits for the wrapper to exit. `$VARDE_CLAUDE` (or
`$VARDE_PI`), when set, wins over `command`.

**A repository's own settings** live in its git config, so nothing is
matched by path and nothing committed is ever run:

```sh
git config varde.worktree always       # this repo's first runs get a worktree
git config varde.base develop          # the branch worktrees start from and land in
git config varde.setup "pnpm install"  # run in each new worktree before the agent starts
```

The setup runs in the terminal, at the worktree's root, where you see
its output and ^C stops it. It runs once when the worktree is made, and
again only when the worktree is brought back. A failing setup is
reported, and the session starts anyway. A `varde.worktree` other than
`ask`, `always` or `never` is ignored, with a notice.

**What varde trusts.** It runs only what you set yourself: the agent
`command` in your config, and `varde.setup` in a repository's local git
config. Both run as you. Memory is written by agents and handed to
later sessions, so treat it as you would the agent's own output: what a
model writes into a cairn, or a note handed to another session, is kept
free of secrets, URLs, commands piped into a shell and orders to
sessions, but your own lines are never filtered. The store, under
`~/.local/state/varde`, holds your prompts and is readable by you alone.

**Where things are.** The store is `~/.local/state/varde/varde.db`
(`$XDG_STATE_HOME/varde`, or `$VARDE_HOME` for the whole directory).
Sessions varde launches carry `VARDE_RUN`, and every process varde
starts for itself carries `VARDE_INTERNAL=1`, which its hooks ignore.

## When something is wrong

`varde doctor` checks everything below and says how to fix what fails.

- **The board shows no sessions, or their status never changes.** The
  agent's hooks are not installed, or are older than this varde: the
  board and `varde attach` say so. Install the plugin again (above).
- **`varde` on your PATH is another copy.** `doctor` says which; a
  `cargo install` and a package can both be there.
- **`varde next` does nothing.** It acts only from a tmux key, and says
  what it cannot do in tmux's status line. Check that `varde` is on the
  `PATH` tmux was started with (see Inside tmux).
- **A session landed in the wrong project.** `m` on the board, or
  `varde set <task> --project DIR`, moves the task (once its session
  has ended and its worktree's work has landed).
- **The config seems ignored.** A setting varde cannot use is left out,
  and only that one; the board and `doctor` say which.
- **A cairn edit was refused.** The message says why. A whole-cairn
  edit is kept in a file whose path is printed: edit again and copy it
  in.

## Upgrading

Install the new binary the way you installed the old one. The store
upgrades itself the first time the new varde opens it, and a board that
is running starts again as the new varde, keeping what you were typing.
The plugin holds only hooks that call `varde hook`, so it needs
updating only when `doctor` says its events are out of date. From 1.0,
some board keys moved: [`CHANGELOG.md`](../CHANGELOG.md) has the table.

## Uninstalling

- In Claude Code: `/plugin uninstall varde@varde`; for pi, remove the
  `varde.ts` link from its extensions.
- Remove the `varde` binary, and the store, `~/.local/state/varde`.
- In each repository varde worked in, `.worktrees/` holds any
  worktree still there (`git worktree remove` each first),
  `refs/varde/discarded/` the work of dropped tasks (`git for-each-ref
  refs/varde` lists it), and `.git/info/exclude` a `.worktrees/` line
  varde added.

## For other front ends

`list`, `show`, `new`, `start`, `edit` (one line), `done`, `park`,
`drop`, `recall`, `eval`, `doctor` and `agent` take `--json`; the other
commands print nothing a client needs. `start`, `link`, `unlink`,
`seen`, `note`, `agent` and `run`, which clients and the agent use, are
left out of `--help`; `varde help <command>` describes each.
[`clients.md`](clients.md) is the contract a client for another editor
builds on.
