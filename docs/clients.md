# Writing a varde client

A client puts varde's tasks where you work: an editor, an IDE, a
terminal multiplexer. `org-varde.el` is the reference client. This page
is the contract any other client works to.

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
  `"v"`. A field may be added within a version; none is removed or
  changes meaning. `list` and `show` are at `"v": 2`, every other reply
  at `"v": 1`.
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
| `varde new --json [--project DIR] [--context -] [--external REF] [--when RULE [--worktree always\|never]] -- TITLE` | `{"id", "short"}` of the new task. `--when` schedules it, as `varde schedule` does (below); a rule varde cannot read, or a first start that would ask about a worktree with no `--worktree`, is refused before anything is made. `--context -` reads its context (the task's `body`) from stdin; `--body` is an alias. With no TITLE (nothing after `--`), the title is cut from the context's first words and a detached model call names the task, which the next `list --json` shows; with neither, it exits 1 (`a task needs a title, or --context TEXT to cut one from`). |
| `varde schedule TASK [--worktree always\|never] -- RULE`, `varde schedule TASK --off` | Gives the task a schedule, or changes or removes it, and says so (`k2f0g3 repeats (every weekday 09:00); first Thu 09:00`). RULE is `at [DAY\|DATE] [HH:MM]`, `every day\|weekday\|weekend\|mon,thu [HH:MM]`, `every 30m\|4h\|2d\|1w`, or an org timestamp (`<2026-10-19 Mon 09:00 ++1w>`). Nobody is there when it fires, so where a first start would ask about a worktree, it exits 1 without `--worktree`: ask the user and call again. A task whose project is unsure is refused until `set --project`. See "Scheduled tasks". |
| `varde set TASK [--title=T] [--context -] [--project DIR]` | Sets the title or context (`body`; `--body` works too), and says what changed: `<short>: title and context changed` (or `title changed`, `context changed`); the same text again prints nothing. A title set this way is the user's, and no model renames it; an empty `--title=` cuts it from the context again and has a model name the task. `--project` moves the task to the project DIR is in (its repository root) and prints `moved <short> to <project>` (then `; title changed` and the like); already there, it prints nothing. A move is refused (exit 1, nothing changed) while the task has an active worktree or a live session; show the message. The cairn stays with the task, and the next start is a fresh conversation in the new project. |
| `varde accept TASK…` | Moves Inbox tasks into Open, printing `accepted <short>` for each. When none of the tasks named is in the Inbox it exits 1 (`<short> is not in the Inbox: accept is for Inbox tasks`, or for several `none of them is in the Inbox: …`); beside ones that are, the others are named on a last line (`not in the Inbox, so left as they are: …`). |
| `varde link TASK SOURCE`, `varde unlink TASK` | Records or forgets where the task lives in your client. Linking takes the task out of the Inbox. |
| `varde drop TASK [--json]` | Drops a task. Work in its worktree is kept under `refs/varde/discarded/`, and starting the task again brings it back. |
| `varde show TASK --json [--no-git]` | The task, its cairn, its runs, its worktree and its git status. `--no-git` skips git and `gh`, which can take a second. `prompt_title`, present only on a task a model has named, is the title varde cut from its first prompt or context (or the agent wrote, for deferred work). `schedule` is `{"rule", "due", "error"}` or `null`: the rule as typed, when it is next due (Unix ms), and why its last start failed. `conversations` lists its sessions, newest first, one per conversation (a run and those that resumed or forked it): `{"run", "started", "status", "unread", "quiet", "launched"}`, `status` a live run's or `null`, `unread` while a background run of it is not opened, `quiet` when one ended with nothing to report. Show them under the cairn; RET on one is `start --run`. |
| `varde projects --json` | `{"v": 1, "projects": [DIR…]}`: the projects of varde's tasks whose checkout is still there, newest first, for a project picker. |
| `varde agent --json` | `{"v": 1, "agent", "command"}`: the agent a new conversation uses (`$VARDE_AGENT`, else `agent` in the config, else `claude`) and the argv that runs it (`$VARDE_CLAUDE` or `$VARDE_PI`, else `[agents.NAME] command`, else its name). Run `command` for a session of the user's own (an Inbox task once prompted); never hardcode `claude`. Exit 1 for an agent varde does not know. Without `--json`, the command line, quoted for a shell. |
| `varde start TASK --json [--fresh\|--fork\|--run RUN] [--worktree always\|never] [--project DIR] [--context-file F] [--agent A]` | A plan: see below. `--run` continues that run's conversation (a `conversations` entry's `run`, from `show`) rather than the newest. |
| `varde done TASK --json [--remove [--force]] [--keep-running]` | What happened to the worktree and which sessions were stopped. On a repeating task it ends the run, not the series, and the task stays open: the reply has `repeats` (the rule), `next` (Unix ms), `stopped`, `left` and `read` (how many unread runs it read), and no `worktree`. With nothing to end it exits 1, and `--remove` is refused. A client offers no undo for it. |
| `varde park TASK --json [--keep-running]` | Which sessions were stopped. |
| `varde reopen TASK` | Back to Open from done, parked or dropped, without starting. |
| `varde edit TASK --add SECTION TEXT… [--json]` | Adds a line to the cairn, as the user's: `findings`, `decisions` or `open`; `status` replaces the Status. See below. |
| `varde edit TASK --set ID TEXT… [--json]`, `--rm ID`, `--restore ID` | Changes the line with that id, removes it, or brings back one removed with `--rm`. `--rm status` hands a Status the user set back to the updates. |
| `varde edit TASK --print` | The cairn as text to edit, for bulk changes. |
| `varde edit TASK --stdin` | Saves an edited cairn. Prints `saved` or `unchanged`. Text without the cairn's shape is refused (exit 1, nothing saved). |
| `varde show --project [DIR] --json` | The cairn of the project DIR (default: the current directory) is in: `{"v": 1, "project", "version", "cairn", "usage", "limits"}`, `cairn` being `null` before there is any (see below). |
| `varde edit --project [DIR] --add SECTION TEXT…`, `--set`, `--rm`, `--restore`, `--print`, `--stdin` | The project cairn, as for a task's; its sections are `conventions`, `gotchas` and `howto`. |
| `varde recall --json [--all \| --project DIR] WORDS…` | Search hits over cairns, former bullets, notes and project cairns, one per task (or project), best first: `{"v": 1, "everywhere", "hits": [{"task", "short", "title", "project", "status", "snippet", "kind", "ref", "score", "state", "stale", "more"}]}`. `snippet` is the text that matched, preferring a bullet or note to the title; `kind` says what it is (`findings`, `decisions`, `open`, `history` for a bullet that has left its cairn, `notes`, `title`, or `id` for a task named by its id, whose `snippet` is its Status); `ref` is the bullet's or note's id, as a string; `state` is the task's (`open`, `done`, `parked`, `dropped`; absent for a project cairn); `stale: true` (absent otherwise) marks a bullet whose code is gone. `more` (absent when 0) counts the task's (or project cairn's) other matching lines among the best rows. A hit in a project's cairn has `kind` `conventions`, `gotchas` or `howto` (or `history`), an empty `task`, `short` `project` and `title` `project cairn`. `everywhere` is true when nothing matched in this project and the hits come from every project. `--project DIR` searches DIR's project first, not the current directory's. A task's id or short id among the words finds that task first. `status` is the task's cairn Status (empty without one); `ref` is absent when there is none; `score` is `null` for an `id` hit. At most 10 hits. |
| `varde doctor --json` | `{"v": 1, "checks": [{"name", "ok", "detail", "fix"}]}`. `fix` says what to do, on a failed check only. It exits 1 when any check failed, after printing the report. |
| `varde seen TASK` | Marks what the task's sessions finished as read. Call it when the user looks at the task's terminal; a `start` that answers `focus` already does. It does not read a background run's result: only opening that run (`start`) does. |

`--external` and `link` take free text naming where the task lives
outside varde. Only a client with objects of its own that a task can
belong to (a heading, an issue) links tasks; one that only hosts
terminals never does. org-varde links nothing: notes link to tasks
instead (`[[varde:SHORT]]`).

**The Inbox and links** are separate. `inbox` says the user has not
acted on the task yet: ad-hoc sessions and deferred work arrive there,
and `accept`, `link`, `start`, `done`, `park` and `drop` take them out.
A link is optional. For deferred work, `origin` in `show` names the
task it came from: put it next to that one if you can.

## The board

```json
{"v": 2, "groups": [
  {"key": "needs_you", "label": "Needs you", "collapsed": false, "rows": [
    {"task": "01M3CARHR5A6R1T620XWK2F0G3", "short": "k2f0g3",
     "title": "Fix the login bug", "project": "/home/me/src/app",
     "status": "blocked", "unread": true, "busy": true, "agents": 0, "shells": 0,
     "inbox": false, "external": "org", "state": "open",
     "last": 1790341564302, "changed": 1790341500000,
     "run": {"id": "01M3CARHTABHFFZDKTDRRB0R2D",
             "native_id": "0d48ec9c-…", "cwd": "/home/me/src/app/.worktrees/…"},
     "git": {"stage": "pr", "label": "PR #12 ✓", "checks": "passing"}}]},
  {"key": "working", …}, {"key": "open", …}, {"key": "scheduled", …}, {"key": "inbox", …},
  {"key": "done", …}, {"key": "parked", "collapsed": true, …}]}
```

- Groups always come in this order, every one present even when empty
  (`dropped` only with `--all`). A `collapsed` group shows only its
  label and count until the user opens it.
- `status` is one of `working`, `delegated`, `waiting`, `blocked`,
  `exited`, `open`, `done`, `parked` or `dropped`. `delegated`: the
  last turn ended with sub-agents still running. `last` is Unix milliseconds.
- `state` is the task's own: `open`, `done`, `parked` or `dropped`.
  `closed` is when it was done, parked or dropped, in Unix
  milliseconds, and `null` while it is open. With `--all`, a row whose
  task has a cairn Status has it as `cairn_status`, for a review; the
  board leaves it out, to stay small.
  `changed` is when it, the Inbox flag or the link last changed, in Unix
  milliseconds, by anyone. `last` moves with closing, runs and events, but not with a
  plain `reopen`; `changed` does.
- `unread` is true for a Needs you row whose session finished a turn or
  asked for a decision that nobody has looked at since. These rows come
  first in the group; make them stand out. A session the user started
  or interrupted is never unread. `varde seen` clears it.
- `busy` is true while a session of the task is at work: mid-turn
  (`working`, `delegated` or `blocked`), or `waiting` with sub-agents or
  background shells its last turn left running (`agents` or `shells`
  above 0), in any of its live sessions, not only the one the row shows.
  Closing a busy task stops that work, so ask first (the board asks
  whether to stop it and close, or close and leave it running).
- `agents` and `shells` count what the session's last turn left running
  in the background. They are 0 unless the row is `delegated` or
  `waiting`. Shells never make a row `delegated`.
- `git` is where the task's git work stands, as varde last checked it.
  Show `label` as it is. It is empty when there is nothing to do, and
  `git` is `null` before the first check, outside a repository, and
  for a task varde does not track: one never started, or one closed or
  parked without a worktree. `stage` is as in `show` (below), and
  `checks` is the open PR's (`passing`, `failing` or `pending`).
  `list` without `--all` keeps it current by itself.
- `done` holds tasks closed today, and older ones whose worktree is
  still there because their work has not landed.
- `run` is the task's latest run, or `null` for a task that never ran.
- `schedule` is `null`, or for an open task with a schedule
  `{"rule", "due", "label", "repeats", "late", "error"}`. Such a task is
  in `scheduled`, soonest first, unless a session of it is in Needs you
  or Working. Show `label` (`↻ Mon 09:00`, `◷ Thu 06:00`, with `late`
  when it should have started by now) where a row shows its age; in
  Needs you or Working, a repeating one shows a bare `↻` beside its age,
  since `done` then ends its run, not the task.
- `origin` is `{"task", "short", "title"}` of the task deferred work
  came from, or `null`.
- `inbox` holds tasks that arrived without the user creating them:
  sessions started outside any client, and work deferred from a
  session. They stay until the user acts on them. Offer `accept`.
- `--all` adds old done tasks and an eighth group, `dropped`
  (collapsed). Nothing else leaves the board by itself.

## Starting a task

`varde start TASK --json` returns one of three plans.

**Run it:**

```json
{"v": 1, "action": "run", "run": "01M3…", "cwd": "/home/me/src/app/.worktrees/fix-the-login-bug-k2f0g3",
 "command": "varde run x7q2kd"}
```

Open a terminal in `cwd` running the user's shell, and type `command`
into it: it names the run by the six characters `varde show` prints
(the full id, in the rare store where they name two runs). Remember which terminal belongs to which task. The agent
exits back into that shell. `varde run` must be the agent's parent:
that is how varde knows the session is alive, so never run the agent
yourself. A run launches once; the same `command` again fails.

**Go to it:**

```json
{"v": 1, "action": "focus", "run": "01M3…", "tty": "/dev/pts/7"}
```

The task's session is already running. Go to the terminal you opened
for it. If you did not open it, `tty` (when not `null`) is the terminal
the agent runs on, for a session someone started by typing the agent's
name into a shell: find your terminal on it, else say where it runs. `--fork` starts a second session beside it. A done,
parked or dropped task is reopened first, as any start reopens it.

**Ask the user:**

```json
{"v": 1, "action": "ask", "ask": "worktree"}
{"v": 1, "action": "ask", "ask": "project", "choices": ["/home/me/src/app", "/home/me/src/api"]}
{"v": 1, "action": "ask", "ask": "takeover", "run": "01M3…"}
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
- `takeover`: the task's session is at work in the background, started
  by its schedule, with no terminal to go to. Ask whether to take it
  over; yes calls again with `--take-over`, which stops it and answers
  `run`, resuming its conversation in your terminal. No leaves it be.

`start` writes anything else the user should know to stderr, one line
each, starting `varde: `: a worktree restored, dropped work brought
back, a `.worktreeinclude` file it could not copy, another of the
task's sessions live in the same directory. Show those lines; the
plan still follows on stdout.

`--context-file F` adds text to a fresh run's first prompt: the
surroundings the task has in your client. Keep it short.

## Scheduled tasks

A schedule makes varde start a task in the background: `start --fresh`
and `varde run` with no terminal, the agent in its print mode. It is
tracked as any session, with its hooks, status and row. Once it has
ended, its row is in Needs you, unread, until that run is opened, unless
it ran `varde done` itself (it found nothing to report); every such run
counts, not only the newest, and the row's `run` is the newest of them.
`start` opens that one. While it works, `start` asks `takeover`
(above). In `show`, its runs have `"background": true`, and `done`
(Unix ms) when the run's own session ran `varde done`. A repeating task
starts a fresh conversation each time, given its cairn.

Nothing runs in the background but `varde tick`, which the heartbeat
(`varde schedule --install`: a systemd user timer or a LaunchAgent)
runs every minute, and `list` starts when a row it returns is due. A
client does nothing to make schedules fire. `park` and `drop` stop a
schedule, since it fires only for open tasks; `reopen` lets it fire
again, at once if it was due meanwhile. `done` on a repeating task ends
its run and leaves it open; the series ends with `schedule --off`.

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

Where your client has its own idea of done (a keyword, a checked
box), keep the two in step from your side: call `done`, `park` or
`reopen` when the user changes it, and read the board (or `state` from
`show`) to catch changes made elsewhere. When they disagree, the more
recent change wins: compare a row's `changed` with when your object
last changed. Never stop a busy session to make them agree. A task
dropped elsewhere is on the board only with `--all`. If a user can
step through several states quickly, act on where the state rests,
not on each step.

## Memory reaches the session by itself

A client does nothing to hand a session its memory. The agent's hooks
do it as the session starts: the project's cairn, the task's cairn,
the task it was deferred from and related work, as fits how it started
(a fresh start, a resume, a compaction), within about 500 words. A
`start` plan's first prompt is only the task's title, its body and the
`--context-file`.

## Showing and editing the cairn

`show TASK --json` (`"v": 2`) has the task's `id`, `short`, `title`,
`context` (also as `body`), `project`, `state`, `inbox`, `external`, `origin` (a task id
here), `created`, `closed` and `changed`; its `cairn`, its `usage` and
pending `notes`; its `worktree` (`{path, repo, branch, base, state}`, or
`null`); its `runs`; and its `git` (below). `--history` adds every
earlier version of the cairn, with why it changed.

Each run has its `id`, `native_id`, `relation` (`fresh`, `resume` or
`fork`), `parent_run`, `launch` (`managed` when varde launched it),
`cwd`, `started`, `status` and `exit_code`, and a `why`: the facts its status comes from.
`event` and `at` are its last status event, `idle_at` and `seen_at` are
an idle notice and a `varde seen` after that event, or `null`, and
`alive` says whether its agent process is running, or is `null` when
varde does not know the process.
A run `start` prepared that nothing launched has `launched: false`, and
is listed only while it is the latest. A run
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
the same while it is in the cairn; `varde show` prints it after the
line, as `#12`, which `--set`, `--rm` and `--restore` take with or
without the `#`. `anchor`, when present, is the path
or identifier the bullet is about, and `stale: true` (absent otherwise)
says that anchor is no longer found in the project's code; `varde show`
marks such a bullet `(stale?)`. A bullet that leaves the cairn is kept,
and `recall` still finds it, unless the user removed or rewrote it. `notes` lists the texts pinned
with `varde note` that no update has taken in yet.

`usage` says how the cairn is used, for a client to show beside it:

```json
"usage": {"updated": 1760000000000, "author": "job", "run": "01K…", "sessions": 3,
          "handed": 1760000100000, "notes": 1, "stale": 1}
```

`updated` and `author` (`job` for an update, `user` for an edit) are its
last change, or `null`; `run` is the run whose update made it (`null`
for the user's); `sessions` is how many sessions were handed it whole in
the last 30 days, and `handed` the last time, or `null`; `notes` and
`stale` count the notes waiting and the bullets whose code is gone. A
project's `usage` has `task`, the task whose promotion last changed it
(from before promotion was dropped), and `notes` 0. `varde show` words these the way the board does:
`updated 20m ago by session k3m9qz · handed to 3 sessions, last 9m ago · 1 stale`.

`limits` are the cairn's caps, for a client to show beside a line
being typed (`12/40 words`, `Open 4/5`) rather than hard-code them:

```json
"limits": {"status": 30, "line": 40, "words": 200,
           "sections": {"findings": 7, "decisions": 5, "open": 5}}
```

`status` and `line` are the most words in the Status and in a line,
`words` the most in the whole cairn, and `sections` the most lines in
each. A project's has no `status`, and its sections are `conventions`
(5), `gotchas` (7) and `howto` (5).

**One line at a time.** The normal edit. Each is one transaction and
ids are stable, so a concurrent update never loses the user's line.

```
varde edit TASK --add findings The cache key omits the locale.   # or open, decisions; status
varde edit TASK --set 12 The cache key omits the locale and region.
varde edit TASK --rm 12
varde edit TASK --restore 12
```

TEXT is the rest of the line, its words joined by one space, or `-` to
read it from stdin (for text that starts with `-`). The line written is
the user's (`mine`), and so is a Status set with `--add status`. `--rm`
moves the line to history with the reason `removed by you`, where
`recall` does not find it, and `--restore ID` brings it back where it was.
The old words of a line changed with `--set` go to history as `edited by
you`, which `recall` does not find either.
`--set` with the words a line already has makes it the user's and
changes nothing else. `--rm status` hands a Status the user set back to
the updates (`status_mine` in `show --json` goes), keeping its words
for the next update to change; on a Status that is not the user's it
is refused: `the Status is already the updates'`. ID is the number `varde show` prints after the
line, `#12` or `12`. Each prints what it did (`added line #23`,
`changed line #12`, `Status set, yours now: …`, `Status handed back: …`,
`removed line #12; …`, `brought back line #12`, or `unchanged`); with `--json`, the cairn after it:

```json
{"v": 1, "version": 4, "id": 23, "changed": true,
 "cairn": {"status": "Cause found.", "status_mine": true, "findings": [{"id": 23, "text": "…", "mine": true}], …}}
```

`id` is the line added, changed, removed or brought back (`null` for the
Status), and `cairn` is as `show --json` has it. A change is refused
(exit 1, `varde: not saved: <why>` on stderr, nothing written) when a
line is over 40 words or a Status over 30, when the section is full
(`not saved: Open is full (5 of 5): change or remove one of its lines
first`), when the cairn would pass its 200 words, when another line
already says the same (`a line already says this (line #12)`), or when
there is no line with that id in this cairn (`no line #12 in this
cairn: varde show lists the current ids`; one already removed says
`line #12 is already removed; --restore 12 brings it back`, and one a
whole-document edit changed names the line it became: `line #12 was
changed since: it is #15 now`). An unknown section is refused before anything is read
(`no section "x": a task's cairn has status, findings, decisions and
open`), and a project cairn has no Status. A client keeps what the user
typed when a change is refused, and shows the message.

**The whole document.** For bulk changes, get the text from
`varde edit TASK --print`, let the user change it, and send the whole
text back to `varde edit TASK --stdin`:

```
#: version 3
Status: Cause found; fix pending.
Findings:
- The cache key omits the locale.
Decisions:
Open:
- [mine] Add the locale to the key.
```

Lines starting `#: ` are varde's notes and are never saved. A line prefixed `[mine]` is the user's. varde compares what comes back
with its own text, and any line that is new or changed becomes the
user's, which later updates must keep. So edit the printed text rather
than rendering the cairn yourself: a rendering that differs by a space
makes every line the user's. A Status the user changed is theirs too:
the cairn's JSON then has `"status_mine": true`, and updates leave it.

An edit is refused (exit 1, nothing saved) when it is not in the
cairn's shape, or past the caps (`limits`), the user's lines included.
The message says what is over, and the last line of stderr is
`what you wrote is kept in PATH`, so a client can offer the text back.
Pass the `#: version N` line back as it is: an edit is also refused
when the document changed since that `--print`. Print it again, and
offer the user what they wrote.

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
  creates and deletes `-wal` and `-shm` files, so a client that
  refreshes on those refreshes forever.
- Debounce, with a ceiling: a working session writes several events a
  second. org-varde waits 0.2 s after the last change but at most 1 s
  after the first, and never runs two listings at once.
- Refresh after your own commands too, and on a slow timer: an event
  a hook wrote while the store was busy waits in `pending.jsonl` until
  the next varde command, and file watching is unreliable on some
  platforms.
