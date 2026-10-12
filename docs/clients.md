# Writing a varde client

A client puts varde's tasks where you work: an editor, an IDE, a
terminal multiplexer. `org-varde.el` is the reference client, for Emacs
and org-mode. This page is what any other client needs to know. It is
the contract. What org-varde does beyond it is Emacs's own business.

## The rules

1. **varde owns every durable fact.** A client keeps at most a link
   from its own objects (a heading, a note, an issue) to a task id.
   Everything else it asks varde for, every time.
2. **Status is varde's to decide.** A client shows the `status` and the
   groups it is given. It never works one out from runs or events.
3. **A client only carries out plans.** `varde start` decides whether to
   go to a running session, resume, fork or start fresh. The client
   opens a terminal or goes to one.
4. **Nothing in a render path spawns a process.** Fetch the board
   asynchronously, keep it, and draw from what you kept.
5. **Hooks never call the client.** varde does not know clients exist.
   A client learns about changes by watching the store (see below).

## Calling varde

- Pass `--json` wherever a command takes it. Every JSON reply has a
  `"v"`. A field may be added within a version. None is removed or
  changes meaning. `list` and `show` are at `"v": 2`: `inbox` replaced
  `filed`, and the `inbox` group replaced `unfiled`. Every other reply
  is at `"v": 1`.
- On failure varde exits non-zero and writes `varde: <message>` to
  stderr. Show the message: it says what to do.
- A task can be named by its full id, by any suffix of at least four
  characters, or by the first four or five characters of `short`, the
  six-character suffix people see and type. Fewer than four are refused.
- `$VARDE_HOME` moves the whole store. Use it in tests, with a fake
  `varde` or a fake agent, so a test never touches the user's tasks.

## Commands a client uses

| Command | Returns |
|---|---|
| `varde list --json [--all] [--project [DIR]]` | The board: groups, already ordered and labelled. `--all` adds old done tasks and a Dropped group. `--project` keeps only the rows of the project DIR (default: here) is in. |
| `varde new --json [--project DIR] [--body -] [--external REF] -- TITLE` | `{"id", "short"}` of the new task. `--body -` reads it from stdin. |
| `varde set TASK [--title=T] [--body -] [--project DIR]` | Refreshes the title or body from where the client keeps them. `--project` moves the task to the project DIR is in (its repository root, as `new` takes it) and prints `moved <short> to <project>`; already there, it does nothing and prints nothing. A move is refused (exit 1, nothing changed, the title and body included) while the task has an active worktree or a live session, which belong to the old checkout; show the message. Past runs keep their directories and the cairn stays with the task; the next start is a fresh conversation in the new project, given that project's cairn. |
| `varde accept TASK…` | Moves Inbox tasks into Open. |
| `varde link TASK SOURCE`, `varde unlink TASK` | Records or forgets where the task lives in your client. Linking takes the task out of the Inbox. |
| `varde drop TASK [--json]` | Throws a task away. Work in its worktree is kept under `refs/varde/discarded/`, and starting the task again brings it back. |
| `varde show TASK --json [--no-git]` | The task, its cairn, its runs, its worktree and its git status. `--no-git` skips git and `gh`, which can take a second. |
| `varde start TASK --json [--fresh\|--fork] [--worktree always\|never] [--project DIR] [--context-file F] [--agent A]` | A plan: see below. |
| `varde done TASK --json [--remove [--force]] [--keep-running]` | What happened to the worktree and which sessions were stopped. |
| `varde park TASK --json [--keep-running]` | Which sessions were stopped. |
| `varde reopen TASK` | Back to Open from done, parked or dropped, without starting. |
| `varde edit TASK --print` | The cairn as text to edit. |
| `varde edit TASK --stdin` | Saves an edited cairn. Prints `saved` or `unchanged`. Text without the cairn's shape is refused (exit 1, nothing saved). |
| `varde show --project [DIR] --json` | The cairn of the project DIR (default: the current directory) is in: `{"v": 1, "project", "version", "cairn"}`, `cairn` being `null` before there is any (see below). |
| `varde edit --project [DIR] --print`, `--stdin` | The project cairn as text, and back, as for a task's. |
| `varde recall --json [--all] WORDS…` | Search hits over cairns, former bullets, notes and project cairns, one per task (or project), best first: `{"v": 1, "everywhere", "hits": [{"task", "short", "title", "project", "status", "snippet", "kind", "ref", "score", "state", "stale"}]}`. `snippet` is the text that matched, a bullet or note before the title whenever one matched; `kind` says what it is (`findings`, `decisions`, `open`, `history` for a bullet that has left its cairn, `notes`, `title`, or `id` for a task named by its id, whose `snippet` is its Status); `ref` is the bullet's or note's id, as a string; `state` is the task's (`open`, `done`, `parked`, `dropped`; absent for a project cairn); `stale: true` (absent otherwise) marks a bullet whose code is gone. A hit in a project's cairn has `kind` `conventions`, `gotchas` or `howto` (or `history`), an empty `task`, `short` `project` and `title` `project cairn`: there is no task to go to. `everywhere` is true when nothing matched in this project and the hits come from every project. A task's id or short id among the words finds that task first. `status` is the task's cairn Status (empty without one); `ref` is absent when there is none; `score` is `null` for an `id` hit. At most 10 hits. |
| `varde doctor --json` | `{"v": 1, "checks": [{"name", "ok", "detail", "fix"}]}`. `fix` says what to do, on a failed check only. It exits 1 when any check failed, after printing the report. |
| `varde seen TASK` | Marks what the task's sessions finished as read. Call it when the user looks at the task's terminal. A `start` that answers `focus` already does it. Calling it again is harmless. |

`--external` and `link` take free text naming where the task lives
outside varde. org-varde passes `org`. Pick a short name for your
client. Only a client that is a *task source*, one with objects of its
own that a task can belong to, links tasks. A client that only hosts
terminals, such as a tmux integration, never does.

**The Inbox and links.** These are separate. `inbox` says the user
has not acted on the task yet: ad-hoc sessions and deferred work arrive
there, and `accept`, `link`, `start`, `done`, `park` and `drop` take
them out. A link says where the task also lives. It is optional, and a
task without one is complete. A client that is a task source may give
an Inbox task a home (a heading, an issue) when the user asks or starts
it, then call `link TASK REF`. For deferred work, `origin` in `show`
names the task it came from: put it next to that one if you can.

## The board

```json
{"v": 2, "groups": [
  {"key": "needs_you", "label": "Needs you", "collapsed": false, "rows": [
    {"task": "01M3CARHR5A6R1T620XWK2F0G3", "short": "k2f0g3",
     "title": "Fix the login bug", "project": "/home/me/src/app",
     "status": "blocked", "unread": true, "agents": 0, "shells": 0,
     "inbox": false, "external": "org", "state": "open",
     "last": 1790341564302, "changed": 1790341500000,
     "run": {"id": "01M3CARHTABHFFZDKTDRRB0R2D",
             "native_id": "0d48ec9c-…", "cwd": "/home/me/src/app/.worktrees/…"},
     "git": {"stage": "pr", "label": "PR #12 ✓", "checks": "passing"}}]},
  {"key": "working", …}, {"key": "open", …}, {"key": "inbox", …},
  {"key": "done", …}, {"key": "parked", "collapsed": true, …}]}
```

- Groups always come in this order, and every group is present even when
  empty (`dropped` only with `--all`). Draw them as they are. A `collapsed` group shows only its label
  and count until the user opens it.
- `status` is one of `working`, `delegated`, `waiting`, `blocked`,
  `exited`, `open`, `done`, `parked` or `dropped`. `delegated` means the
  last turn ended with sub-agents still running: the session wakes
  itself as they report back. `last` is Unix milliseconds.
- `state` is the task's own: `open`, `done`, `parked` or `dropped`.
  `changed` is when it, the Inbox flag or the link last changed, in Unix
  milliseconds, however the change was made (a CLI, an agent, another
  client). `last` moves with closing, runs and events, but not with a
  plain `reopen`; `changed` does.
- `unread` is true for a Needs you row whose session finished a turn or
  asked for a decision that nobody has looked at since. These rows come
  first in the group. Draw them so they stand out from the rows the user
  has seen and left. A session the user started or interrupted is
  never unread, since they were there. `varde seen` clears it.
- `agents` and `shells` count what the session's last turn left running
  in the background. They are 0 unless the row is `delegated` or
  `waiting`. Shells never make a row `delegated`.
- `git` is where the task's git work stands, as varde last checked it.
  Show `label` as it is. It is empty when there is nothing to do, and
  `git` is `null` before the first check, outside a repository, and
  for a task varde does not track: one never started, or one closed or
  parked without a worktree. `stage` is as in `show` (below), and
  `checks` is the open PR's (`passing`, `failing` or `pending`).
  `list` without `--all` keeps it current by itself; asking for the
  board is enough.
- `done` holds tasks closed today, and older ones whose worktree is
  still there because their work has not landed.
- `run` is the task's latest run, or `null` for a task that never ran.
- `origin` is `{"task", "short", "title"}` of the task deferred work
  came from, or `null`.
- `inbox` holds tasks that arrived without the user creating them:
  sessions started outside any client, and work deferred from a
  session. They stay until the user accepts, links, starts, closes,
  parks or drops them. Offer `accept` and your own way to link them.
  Dropped tasks are not on the board.
- `--all` adds old done tasks, which the board otherwise leaves out,
  and a seventh group, `dropped` (collapsed), with the dropped tasks.
  Nothing else leaves the board by itself.

## Starting a task

`varde start TASK --json` returns one of three plans.

**Run it:**

```json
{"v": 1, "action": "run", "run": "01M3…", "cwd": "/home/me/src/app/.worktrees/fix-the-login-bug-k2f0g3",
 "command": "varde run 01M3…"}
```

Open a terminal in `cwd` running the user's shell, and type `command`
into it. Remember which terminal belongs to which task. The agent
exits back into that shell, so the user can read what it left and start
again in the same place. `varde run` must be the agent's parent: that
is how varde knows the session is alive. Never run the agent yourself.
A run launches once. Running the same `command` a second time fails.

**Go to it:**

```json
{"v": 1, "action": "focus", "run": "01M3…", "tty": "/dev/pts/7"}
```

The task's session is already running. Go to the terminal you opened
for it. If you did not open it, `tty` (when not `null`) is the terminal
the agent runs on, for a session someone started by typing the agent's
name into a shell. Find your terminal on that tty if you can. Otherwise
say where it runs. `--fork` starts a second session beside it. A done,
parked or dropped task is reopened first, as any start reopens it.

**Ask the user:**

```json
{"v": 1, "action": "ask", "ask": "worktree"}
{"v": 1, "action": "ask", "ask": "project", "choices": ["/home/me/src/app", "/home/me/src/api"]}
```

A first run needs an answer only the user can give. Nothing was started
or changed. Ask, then call `start` again with the answer added.

- `worktree`: whether a task's first run in a git repository gets a
  worktree of its own. Call again with `--worktree always` or
  `--worktree never`. It is not asked when the user's settings or the
  repository's `git config varde.worktree` already say, nor outside a
  repository or in one with no commits yet.
- `project`: the first run of deferred work, or of a task with no
  project, since the session it came from may have been anywhere.
  `choices` are the likely directories, the task's own guess first. Call
  again with `--project DIR`. It comes before the worktree question.
  `start --project` is for a first run only: a started task moves with
  `varde set TASK --project DIR`.

`start` writes anything else the user should know to stderr, one line
each, starting `varde: `: a worktree restored, dropped work brought
back, a `.worktreeinclude` file it could not copy, another of the
task's sessions live in the same directory. Show those lines; the
plan still follows on stdout.

`--context-file F` adds text to a fresh run's first prompt: the
surroundings the task has in your client, such as the outline around an
org heading. Keep it short (org-varde caps it at about 150 words).

## Closing and parking

`done` returns:

```json
{"v": 1, "task": "01M3…", "stopped": ["01M3…"], "left": [],
 "worktree": {"path": "…", "branch": "varde/fix-the-login-bug-k2f0g3", "base": "main",
              "ahead": 2, "dirty": false, "included": [], "action": "kept", "session": false,
              "stranded": [], "pr": null, "saved": null}}
```

`worktree` is `null` for a task without one. `action` is `kept`,
`merged`, `removed` or `discarded`. A kept worktree is removed by varde
once its work lands, so there is nothing to ask the user. `drop`
returns the same report: after it, `action` is `discarded` and `saved`
names the ref that keeps the work, or is `null` when nothing was left
to keep. `included` names the files `.worktreeinclude` copied in that
the task changed: they keep a worktree from being removed, since no
branch holds them. `session` is true when a kept worktree has a live
session in it, which keeps it until that ends. `stranded` says what
in the worktree no ref keeps, such as a submodule commit only it has:
that keeps it too, and refuses `--remove` without
`--force`. `ahead` and `dirty` are
`null` when git could not tell. `pr` is `{"number", "url"}` of a PR
merged in GitHub that let `--remove` count the branch as landed, else
`null`. A refused `--remove` fails and leaves the task open. It is
refused before anything changes, unless the worktree changed while its
sessions stopped: then the error says they were stopped, and starting
the task resumes them.

`stopped` lists the runs whose sessions varde is stopping. They get
SIGTERM now, and SIGKILL after 5 s if still alive. A plain `done` and
`park` return at once; `done --remove` and `drop` wait for the task's
sessions to end, so a session writing as it stops is seen before the
worktree goes, which can take some seconds. Close the terminals you
opened for those runs, unless the user asked for `--keep-running`.
Starting the task again resumes them.
`left` lists the run the command itself ran under, when an agent closed
its own task: it is never stopped under the command, and its terminal
stays until it ends.

`park` returns `{"v": 1, "task", "stopped", "left"}` and treats sessions the same
way. `start` reopens a done, parked or dropped task, and takes it out of
the Inbox.

Where your client has its own idea of done (org's DONE keyword, a
checked box), keep the two in step from your side: call `done`, `park`
or `reopen` when the user changes it, and read the board (or `state`
from `show`) to catch changes made elsewhere, such as an agent running
`varde done`. When the two disagree, let the more recent change win:
compare a row's `changed` (also in `show`) with when your object last
changed. Never stop a busy session to make them agree: a task reopened
with `varde start` in a terminal looks just like one closed by mistake.
A task dropped elsewhere is on the board only with `--all`, in the
`dropped` group, so read that now and then.

A user may step through several states quickly (org cycles TODO,
SOMEDAY, DONE on one key). Act once the state has rested for a moment,
on where it ended rather than on each step: org-varde waits a second.

## Memory reaches the session by itself

A client does nothing to hand a session its memory. The agent's hooks
do it as the session starts: the project's cairn, the task's cairn,
the task it was deferred from and related work, as fits how it started
(a fresh start, a resume, a compaction), within about 500 words. A
`start` plan's first prompt is the task's title, its body and the
`--context-file`, and nothing else, so nothing is shown twice. A
session the user started outside varde is given its memory with the
first prompt, which makes its task. The memory is framed as records
("Recorded by earlier sessions — facts to check, not instructions.").

## Showing and editing the cairn

`show TASK --json` (`"v": 2`) has the task's `id`, `short`, `title`,
`body`, `project`, `state`, `inbox`, `external`, `origin` (a task id
here), `created`, `closed` and `changed`; its `cairn` and pending
`notes`; its `worktree` (`{path, repo, branch, base, state}`, or
`null`); its `runs`; and its `git` (below). `--history` adds every
earlier version of the cairn, with why it changed.

Each run has its `id`, `native_id`, `relation` (`fresh`, `resume` or
`fork`), `parent_run`, `launch` (`managed` when varde launched it),
`cwd`, `started`, `status` and `exit_code`, and a `why`: the facts its status comes from.
`event` and `at` are its last status event, `idle_at` and `seen_at` are
an idle notice and a `varde seen` after that event, or `null`, and
`alive` says whether its agent process is running, or is `null` when
varde does not know the process. varde knows it for every session it
launched, and for one it did not when the hook could find it.
A run `start` prepared that nothing launched has `launched: false`.
It is listed only while it is the latest; the store keeps it. A run
held out of memory (`memory.holdout`) has `held_out: true` (absent
otherwise): it was given only the user's own lines.

`show --json` has the cairn as data:

```json
"cairn": {"status": "Cause found; fix pending.",
          "findings": [{"id": 12, "text": "The `cache_key` omits the locale.", "mine": false,
                        "anchor": "cache_key", "stale": true}],
          "decisions": [], "open": [{"id": 14, "text": "Add the locale to the key.", "mine": true}]}
```

It is `null` before the first update. Each bullet has an `id` that stays
the same while it is in the cairn. `anchor`, when present, is the path
or identifier the bullet is about, and `stale: true` (absent otherwise)
says that anchor is no longer found in the project's code; `varde show`
marks such a bullet `(stale?)`. A bullet that leaves the cairn is kept,
and `recall` still finds it. `notes` lists the texts pinned
with `varde note` that no update has folded in yet. To edit it, get the text from
`varde edit TASK --print`, let the user change it, and send the whole
text back to `varde edit TASK --stdin`:

```
Status: Cause found; fix pending.
Findings:
- The cache key omits the locale.
Decisions:
Open:
- [mine] Add the locale to the key.
```

A line prefixed `[mine]` is the user's. varde compares what comes back
with its own text, and any line that is new or changed becomes the
user's, which later updates must keep. So edit the printed text rather
than rendering the cairn yourself: a rendering that differs by a space
makes every line the user's. A Status the user changed is theirs too:
the cairn's JSON then has `"status_mine": true`, and updates leave it.

An edit is refused (exit 1, nothing saved) when it is not in the
cairn's shape, or when it is past the caps, the user's lines included:
7 findings, 5 decisions, 5 open items and 200 words in a cairn; 5
conventions, 7 gotchas, 5 how-tos and 200 words in a project's cairn.
The message says what is over, and the last line of stderr is
`what you wrote is kept in PATH`, a file holding the text sent, so a
client can offer it back. An edit is also refused when the document
changed since `--print`: then no file is kept, and the client prints it
again.

A project's cairn is the same, with no Status and three sections of
its own, `conventions`, `gotchas` and `howto` (as text, `Conventions:`,
`Gotchas:` and `How-to:`):

```json
{"v": 1, "project": "/home/me/src/app", "version": 3,
 "cairn":  {"shape": "project", "status": "", "findings": [], "decisions": [], "open": [],
            "gotchas": [{"id": 40, "text": "`launch.rs` tests fail on macOS before any change", "mine": false,
                         "anchor": "launch.rs"}]}}
```

## Git status

`show --json` has a `git` object for the task's active worktree or,
without a worktree, for its project checkout, which other tasks share.
It is `null` when that cannot be read or the task's worktree is gone,
and missing with `--no-git`. `target` is `null` when the checkout has
neither a base nor an upstream.

```json
"git": {"branch": "varde/fix-login-k3m9qz", "target": "main", "shared": false,
        "stage": "pr", "ahead": 2, "behind": 5, "changes": 0,
        "upstream": "origin/varde/fix-login-k3m9qz", "unpushed": 0, "landed": false,
        "pr": {"number": 12, "state": "open", "draft": false, "checks": "passing",
               "url": "https://github.com/o/r/pull/12", "head": "9f2c…"}}
```

`stage` is the one word to show: `edited`, `committed`, `pushed`, `pr`,
`landed` or `clean`. `unpushed` is `null` when the branch has no
upstream. `pr` is `null` without `gh`, a GitHub remote or a pushed
branch. Ask for it when the user looks at a task, not on every refresh.

## Knowing when to refresh

varde has no daemon and sends nothing. Watch the store directory,
`$VARDE_HOME` or else `$XDG_STATE_HOME/varde` (`~/.local/state/varde`),
for changes to `varde.db` and `varde.db-wal`, and then run
`varde list --json` again.

- React only to *modifications* of those two files. Every `varde list`
  opens the database, which creates and deletes `-wal` and `-shm`
  files. A client that refreshes on those refreshes forever.
- Debounce, with a ceiling. A session writes several events a second
  while it works, so a plain debounce may never fire. org-varde waits
  0.2 s after the last change but at most 1 s after the first, and
  never runs two listings at once.
- Refresh after your own commands too, and on a slow timer. An event
  a hook wrote while the store was busy waits in `pending.jsonl` until
  the next varde command applies it, and file watching is unreliable
  on some platforms.
