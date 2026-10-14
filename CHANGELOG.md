# Changelog

Notable changes to varde and org-varde. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org).

## Unreleased

### Changed

- org-varde writes a task in a form, as a mail is written in
  `message-mode`: a `Title:` and a `Project:` field above a line, the
  context below it, where the first line used to be the title. An
  empty Title says varde names the task from the context; `RET` goes to
  the next field, `TAB` on the Project picks its directory, and evil's
  line edits keep to a field. `C-u C-c C-c` no longer asks for another
  project: change the Project field instead.

### Fixed

- A turn that ends while you are in its session, in tmux, is read: the
  session marks it so while its pane is on screen. Only a running board
  did, on its next refresh, and only if the pane was still on screen
  then.
- On macOS the Emacs board no longer misses what hooks write while a
  session runs, which left a finished task showing Working. kqueue
  tells a watch on a directory only of files coming and going, not of
  commits to the store's `-wal` that a running session keeps open;
  org-varde now watches that file too.

## 1.1.1 - 2026-10-13

### Fixed

- Two starts of one task at once no longer plan two runs when one
  records its run just as the other looks for one to take.
- org-varde loads `org-element` itself, which Emacs 29 and 30 do not
  load with org.

## 1.1.0 - 2026-10-13

### Highlights

- **Cairns you see and edit a line at a time, everywhere.** A task's
  cairn, what its sessions learned, handed to the next one, comes first
  in the terminal board's view, `varde show` and the Emacs view, and
  you fix one line in place there, or with `varde edit --add`, `--set`,
  `--rm` and `--restore`.
- **A board that stays put, with one vi-like keymap.** Every group is
  always shown, in the same order, so nothing jumps. The terminal and
  Emacs boards share their keys wherever Emacs allows.
- **Moving between sessions.** Tab goes to the next task that needs
  you. In tmux, `varde next` on a key does it from any window; in
  Emacs, `org-varde-next-needs-you` does, in the task's Doom workspace.
- **Titles a model wrote.** A task whose title varde wrote, not you,
  is named in a few words by a small model call as it is made: one
  made from its context alone, a session you start yourself, deferred
  work, and a task whose title you cleared. The call runs detached
  (for a session, from the board's next refresh), so nothing waits on
  it, and the board shows the new title on its next refresh.
- **The board a key away in tmux.** `varde --popup` in a tmux popup
  closes it once you go to a session.
- **Org through links.** In Emacs, tasks live on the board and your
  notes link to them: `org-varde-insert-link` puts in
  `[[varde:k2f0g3][Fix login]]` for a task you pick by its title, and
  the link shows where the task stands (`● working`, `✓ done`).
  `org-varde-new-here` makes a task from a heading or a region and
  links it there, a heading marked DONE offers to close its tasks, and
  a `#+BEGIN: varde` block lists tasks for a review. Org headings are
  no longer kept in step with tasks: see Removed.

### Added

- varde installs with Homebrew (`brew install smebbs/tap/varde`). Each
  release updates the formula.
- `varde edit TASK --add SECTION TEXT`, `--set ID TEXT`, `--rm ID` and
  `--restore ID` (and the same with `--project`) change one line of a
  cairn, as yours, in one transaction. The caps hold; a removed line
  goes to history as `removed by you`, where recall finds it, and
  `--restore` brings it back where it was. `--json` prints the cairn
  after the change.
- `varde show` shows a cairn as the view does: what it is, when it was
  last updated and by what, how many sessions were handed it, what is
  stale, the notes waiting, and each line's id (`#12`) for `varde edit`.
- **The view.** `v` on the terminal board opens a pane with two tabs:
  Task (the task's cairn, then its context and sessions) and Project
  (its project's cairn); `[` and `]` switch them, and the board dims
  under it. `j`/`k` pick a line, `e` edits it on the prompt line, `o`
  and `a` add one, `x` removes it and `u` brings it back, `J`/`K` go to
  the next task. A line you were typing is never lost: Esc puts it
  away, and it outlives the board.
- `varde --popup`, for the board in a tmux popup (`bind b display-popup
  -E -B -w 90% -h 85% "varde --popup"` on tmux 3.3, the guide's
  recommended setup): going to a session quits the board, so the popup
  closes on it, and so does Esc on the bare board.
- A task can be made from its context alone (`varde new --context`, the
  board's form or Emacs's new-task buffer with no title): its title is
  cut from the context, and a model then names the task.
- `?` opens with a line for each group saying what it holds, on both
  boards.
- `varde next` goes to the tmux window of the next task that needs
  you, and to the board once nothing else does (`--board`: the board).
  Bound to a tmux key, you answer one session after another.
- The board keeps to the project it is opened in, in the terminal and
  in Emacs; `.` shows every project, and the header says how many tasks
  are elsewhere and how many of those need you. `[board] scope = "all"`
  or `org-varde-board-scope` opens every project instead.
- `varde list --project [DIR]` and `varde recall --project DIR`.
- `[keys]` in the config binds the terminal board's keys by action;
  `quit = "esc"` makes Esc quit once nothing is marked or open.
- Every prompt of the terminal board edits text as a shell does: word
  moves, C-w (back to a blank, as readline's) and Alt-Backspace (back
  to a word's start), C-u, C-k, Alt-d, and C-y to put back what was
  rubbed out.
- `[memory] titles = false` keeps the title varde cut from a first
  prompt or a context. A title you set, or one from org, is never
  renamed; a task is asked about once, and a failed call keeps the cut
  title; `varde show` keeps the cut words. The cairn update no longer
  names the task: its reply is edits only.
- The board and `varde attach` say when the agent's hooks are missing;
  an empty board says how to fill it.
- For clients: `varde agent --json` (the agent of a new conversation
  and the command that runs it); `busy` on `list --json` rows; `limits`
  `usage` and `context` in `show --json`; `more` on recall hits, which `varde
  recall` shows as `(+2 more in this task)`.
- org-varde: the task view edits the cairn a line at a time, with the
  terminal view's keys; `org-varde-next-needs-you`; `org-varde-board`
  returns to the board's own Doom workspace; an org link
  `[[varde:k2f0g3]]` opens a task's view.
- org-varde: `org-varde-insert-link` (bind it, say, to `C-c v i`)
  inserts a link to a task, or a project's cairn
  (`varde:project:DIR`), picked by its title with completion: open
  tasks first, then closed ones, then project cairns, each with where
  it stands, its project and its age; any part of a title, or a short
  id, matches. In Markdown it inserts `[Title](varde:SHORT)`, elsewhere
  `varde:SHORT`. `y` on the board or in a view copies a link to the task
  at point, and `C-c l` stores one. Exported, a link is its description
  as plain text.
- org-varde: a `varde:` link shows its task's status after itself, as
  the board has it, and follows it while `org-varde-mode` is on.
  Drawing runs nothing.
- org-varde: `org-varde-new-here` on a heading or a region writes a new
  task in the board's new-task buffer, with the heading's title and
  text and a `From:` line linking back; once created, its link goes into
  the heading's text, or below the region (below a table, block, drawer
  or list, never inside), unsaved. On a heading that links an open task
  it offers to go to that instead. The view shows the From line, and
  `RET` on it goes there.
- org-varde: marking a heading DONE offers to close the open tasks its
  text links (`org-varde-close-with-heading`: `ask`, `always`,
  `never`). Nothing is written back to org.
- org-varde: `#+BEGIN: varde :project DIR :state open :closed 7d`, a
  dynamic block listing tasks with their cairn's Status and the day
  they closed (`:closed` takes `24h`, `7d`, `2w`).
- org-varde: `M-x org-varde-convert-links` turns the `VARDE_TASK`
  headings of an earlier org-varde into headings with a link.
- For clients: `list --json` rows say when the task `closed`, and with
  `--all` its cairn's Status (`cairn_status`).
- org-varde: `R` and `F` start a fresh conversation and fork. The
  buffers you write in start in evil's insert state, where `:w` and
  `ZZ` save and `ZQ` cancels.
- A Status you set can be handed back to the updates: `varde edit TASK
  --rm status`, or `x` on it in either view. Its words stay, and the
  next update may change them.
- `u` on either board reopens the tasks last closed there with `d`, `P`
  or `x`, as the message after them says (`Done "…"; u reopens`).
- org-varde: `x` on a region drops every task in it, asking once
  ("Drop 3 tasks?").
- The terminal view shows a cairn line you put away, dim, where it
  goes, and the next board names the task and section of each one kept.

### Changed

- **Keys that moved.**

  | What | 1.0 | 1.1 |
  |---|---|---|
  | Park (terminal) | `p` | `P`, as in Emacs |
  | Project cairn (terminal) | `V` | `v`, then `]`: the view's Project tab |
  | Edit a cairn in `$EDITOR` (terminal) | `e`, `E` for the project's | `E`: the task's, or the project's on the Project tab |
  | Edit title and context (terminal) | `e`, on a task without a cairn | `e`, always |
  | Write a new task in `$EDITOR` (terminal) | C-g in the form | C-x C-e or Alt-e; C-g puts the form away |
  | Refresh (terminal) | `g` | C-l |
  | First task (terminal) | Home | `gg`, and Home |
  | Fold Parked (terminal) | Tab | `z`, on any group |
  | Tab (both boards) | fold | the next task that needs you |
  | RET (Emacs board) | start in the background | go to the session; `s` starts in the background |
  | View a task (Emacs) | `s` | `v`, or `K` under evil |
  | Project cairn (Emacs) | `S` | `]` in the task view: its Project tab |
  | Edit the whole cairn (Emacs) | `e` in the view, `E` for the project's | `E`; `e` edits the line at point |
  | Fold a group (Emacs) | TAB | `z` (`za` under evil); TAB on a group's heading |
  | First / last task (Emacs, evil) | none | `gg` / `G` |

- **One look on every board.** The terminal board, `varde list` and the
  Emacs board draw a row the same way: a glyph, the title, then a live
  session's status, git, project and age. Statuses are three words,
  *working*, *your turn* and *needs input*, shown only while a session
  is live. A board kept to one project leaves out the project column.
- **A stable board.** Every group is always shown, in the same order,
  in the terminal board, `varde list` and Emacs; an empty one is a dim
  `Working  0`. Parked starts folded, and a folded group is one line
  the selection stops on.
- The terminal board's footer shows the keys most needed for the row,
  as many as fit (`Tab needs you`); `?` groups them all, says what each
  group holds and each glyph means, and what a cairn is. Esc is one
  word in every footer: `Esc later` where it keeps what you typed.
- **Recall's pane acts on the hit you pick.** RET (or `v`) opens the
  view on it, on the Project tab for a project's cairn; `s` starts it;
  `d`, `P`, `x`, `a`, `e`, `m`, `R` and `F` act on it, never on the row
  the pane hides. A project's cairn is named by its project (`app ·
  project cairn`), in `varde recall` too, paths there start with `~`,
  and a long hit wraps under its own words.
- **One kind of pane.** The view, recall, the new-task form and `?` are
  rounded boxes the board dims under. The view and recall are as tall
  as what they show, up to most of the screen, and say `↓ C-d more`
  while there is more below; `G` in the view goes to its very end.
- The view lists a task's last sessions a line each (`7njqys  2m ·
  ended`); `varde show` keeps the detail. An empty cairn is its hint
  and its sections' headings, dim. The Status's `›` sits where a line's
  does, and `e` on a heading adds under it, as `o` does. The Project
  tab names the project in its border.
- Needs you rows no longer repeat *your turn*, which the group says;
  *needs input* stays, on the terminal board and in `varde list`.
- C-d and C-u move half a page, in the open pane or else the list.
- `x` drops every marked task, as `a`, `d` and `P` act on them,
  asking once and naming how many.
- Done and park of a busy task ask in words that say why, briefly:
  `Done "Fix the login"? It has 3 agents and a shell running. y stop ·
  n leave running · Esc cancel` ("is working", "is waiting for your
  answer"); Emacs asks the same. A session whose last turn left agents
  or shells running counts as busy, in `list --json` too.
- After done, park or drop, the selection goes to the nearest task
  still open; after accept, it follows the task into Open, and the
  message names it (`accepted k3m9qz`, or `accepted 3 tasks`).
- The worktree question names the task and its project, in the same
  words on the board, in `varde attach` and in Emacs: `Start "Fix the
  login" in a worktree of its own (app)?`, the title cut to fit the
  width.
- `X` in the terminal view drops the task in view, asking first, as
  `x` does on the board (`x` there removes a line).
- The recall pane shows a closed hit's state (`(done)`, `(parked)`,
  `(dropped)`), as `varde recall` does; its footer leaves out `s` and
  `d` on a project cairn's hit, which has no task. With nothing found,
  recall says what it searches: what sessions learned, not a task's
  title or context.
- `varde show --history` says when each version was written (`2026-10-12
  14:03 (3h ago)`) and by whom (`by you`, `by an update`), marks your
  lines `(yours)` as `varde show` does, and gives the reason as `why:`.
- A title cut from a context or a first prompt ends with `…`.
- `varde eval` speaks of lines and updates, and leaves out a measure
  with nothing to count.
- The drop question mentions `refs/varde/discarded/` only for a task
  with a worktree, whose work it keeps.
- `varde new` with neither a title nor a context says `a task needs a
  title, or --context TEXT to cut one from`.
- `done` and `varde show` say `1 commit` and `2 commits`, and a session
  that ended says `ended` without `process gone`.
- The guide's Quick start: `tmux new -s varde varde` outside tmux,
  `varde` (or the popup) inside it; the guide lists the Dropped group
  and its `✕`, and how to get a dropped task back.
- **Outside tmux,** RET asks what a first start needs on the board,
  then hands the terminal over under a line naming the task, says once
  that tmux runs sessions side by side (once for the store), and the board comes back saying
  how the session ended. A session that fails leaves its last words on
  screen until a key.
- **Nothing typed is lost to one key.** Esc or C-g puts the new-task
  form away and `n` brings it back; to drop it, clear it (C-u) and Esc.
  `e` keeps each task's changes the same way, and a cairn line one for
  each line; a failed create leaves the form open; drafts survive
  quitting and an upgrade, and two boards on one store keep each
  other's. In Emacs, `C-c C-k`
  puts a new task away, and the cairn buffer asks before dropping
  changes.
- "Drop" is the word for throwing a task away, in the CLI, both boards
  and their questions.
- On a board kept to a project, `n` makes the task there without asking
  (`C-u C-c C-c` asks in Emacs). A task made, moved or picked in recall
  in another project is shown, the board then showing every project.
- Questions name tasks by title. `done`, `park` and `drop` say which
  task they acted on, and `eval` on an empty store says there is
  nothing to measure.
- `varde --help` lists the commands in the order you need them;
  `start`, which only clients use, is left out.
- `varde new` and `varde set` take `--context` for a task's context;
  `--body` still works.
- `varde accept` exits 1 when none of the tasks named is in the Inbox.
- `varde set` says what it changed (`k3m9qz: title changed`).
- org-varde: the task view shows the cairn first, then the context,
  sessions and git; recall searches the board's project first.
- org-varde: the Emacs view, recall and prompts read as the terminal's:
  the view opens on the first finding, the facts line has the row's
  details, recall names a project cairn hit `app · project cairn`, takes
  `j`/`k` and `.` for every project, and says "(+N more in this task)";
  done and park name what they do when a session is busy; edits say what
  they did ("added to Findings, as yours"). `=` opens magit (else
  vc-dir) where the task's work is.
- org-varde: `N` runs the agent varde is set to start, not always
  claude; `org-varde-agent-command` overrides it and is unset by
  default.
- org-varde: closing a task closes its Doom workspace but keeps the
  buffers you opened there.
- org-varde: the board redraws only when it is in sight and changed (a
  refresh of 1 000 rows: 72 ms to 6 ms).
- org-varde: `org-varde-next-needs-you` goes first to what you have not
  seen, pressed again walks on from where it went, and passes over a
  task running outside Emacs once it has said so. The worktree question
  reads `Start "…" in a worktree of its own (app)?`. `a` on a region
  accepts the Inbox tasks in it and says how many it left.
- Related work stays quick as memory grows: each cairn update's tidy-up
  merges recall's index a bounded step at a time (a thousand reindexed
  tasks had left it in 22 pieces, each lookup three times slower), and
  the hook reads the index's row count instead of counting 13 000 rows.

- A session is told its run by the six characters `varde show`
  prints, which `note --run` and `new --from` now take, instead of the
  26-character id three times; and after its cairns, to note a
  correction to a line that is wrong or done.
- The terminal view on a screen under 30 rows, or in a popup, takes
  all but three rows of the board; where the task stands and how its
  cairn is used are one line. `X` left the view's footer (`?` has it).
- `?` in the terminal fits 80 columns and a popup without its columns
  running into each other, and scrolls with `j`/`k` when taller than
  the screen. What a cairn is, and its `›` and `?` marks, is a line
  under Groups, not the border.
- A tmux window the board opens is named by the task's title alone,
  cut to 14 columns: panes are found by their tag, not the name.
- The command a session's terminal is given names its run by six
  characters, `varde run x7q2kd`, which `varde run` takes; the full id
  only where those name two runs.
- `u` in the terminal view also undoes the last Status handed back,
  making it yours again; with nothing of the cairn's to undo, it
  reopens the task last closed there with `d`, `P` or `X`, so a close
  in the view says `u reopens` too.
- `varde edit --set`, `--rm` or `--restore` with the id of a line a
  whole-document edit changed names the line it became (`line #2 was
  changed since: it is #5 now`); an unknown id points to `varde show`.
- Related work shows a task by a line you wrote before one an update
  wrote, and by a cairn line before a note, when they match as well.
- org-varde: `u` in the view also undoes a Status handed back (`Status
  handed back: …; u undoes`), and with nothing there to undo reopens
  the task `d`, `P` or `X` closed in it, so its `u reopens` holds.
  `org-varde-new-here` titles a region without a list item's bullet,
  number or checkbox, and one starting on a heading as org shows it.
  `org-varde-insert-link` matches titles in any case, and `SPC` types a
  space. The board opened from an org note keeps to the `VARDE_PROJECT`
  of the entry at point. A review block cuts a long Status to 50
  columns.

### Removed

- **org-varde no longer makes org headings into tasks** (the design's
  Q31). Gone: starting a task from its heading (`org-varde-start`, now
  obsolete, goes to the open task the heading links, else runs
  `org-varde-new-here`), the `VARDE_TASK` property and `org-varde-link`
  (an obsolete alias of `org-varde-insert-link`), keyword changes closing, parking and reopening
  tasks and tasks setting keywords back, archiving closing a task,
  reconciling, `org-varde-done`, the board's `f` (file), `l` (link) and
  `o` (heading), the view's `h` and recall's `o`, `e` sending a headed
  task to its heading, `m` writing `VARDE_PROJECT`, and the agenda's
  status column (`org-varde-agenda-status` now returns nothing). The
  options `org-varde-directories`, `org-varde-start-goes`,
  `org-varde-park-keywords`, `org-varde-drop-keywords`,
  `org-varde-settle-delay` and `org-varde-context-words` are gone;
  setting them does nothing. **Migrating:** `M-x
  org-varde-convert-links` (`C-u` for every org file) puts a link to
  each heading's task in its text, drops the property, and unlinks the
  task in varde; save the files after. `varde link`, `unlink` and
  `--external` stay, for other clients.

### Fixed

- The first Tab on a board just opened skipped the first task that
  needs you.
- `varde show` said a session whose conversation had ended was
  `running`.
- `varde doctor` checks the agent command `varde run` starts: with a
  mistake in the config, it had checked the defaults instead of the
  settings varde still uses.
- A cairn edit saved after an update landed undid the update; one from
  Emacs, open for minutes, always could. `varde edit --print` now
  stamps the version (`#: version N`), and an edit of an older one is
  refused and kept in a file. A failing editor keeps its text too.
- A typo in the config threw the whole file away, turning memory
  updates back on; now only the setting it is in is left out.
- An empty `$VISUAL` or `$EDITOR` counts as unset, as git takes it, and
  one that is not installed is named (`vi not found: set $VISUAL or
  $EDITOR`), leaving no temporary file behind.
- `done` says when a worktree whose work has landed is kept until the
  session in it has ended.
- org-varde sent org's log lines and clocks under a heading to the
  agent as its context, and link markup and `[1/3]` cookies in its
  title.
- Dropping a task no longer holds Emacs up.
- org-varde: `org-varde-next-needs-you` stuck on the task whose terminal
  it was pressed in; going to a task whose terminal was already in a
  window showed it in a second one; coming back to the board from a
  task's terminal or view set its project scope back, asked varde for
  the project, and, between Doom workspaces, lost its place; a hit in
  recall opened its view in a new window under the hits; after the view
  closed, a board that fits its window stayed scrolled; `a` on a task
  not in the Inbox showed varde's raw error.
- A key typed just before Emacs asks whether to stop a busy session no
  longer answers it.
- org-varde: `org-varde-next-needs-you` passed over the task of the
  board line or link at point, as if you were in it; `C-u M-x
  org-varde-convert-links` left every org file it looked at open.
- The terminal board's cursor no longer slips on wide characters.

## 1.0.3 - 2026-10-12

### Added

- Tab completes directories in the board's project prompts (a new
  task's project, and `m`); more Tabs and Shift-Tab pick from the list.
- C-w and Alt-Backspace in those prompts rub out the last directory.

### Changed

- `e` on a task without a cairn edits it in the new-task form, not
  `$EDITOR`; C-g in the form still opens the editor.

## 1.0.2 - 2026-10-12

### Added

- C-g in the board's new-task form writes the task in `$VISUAL` or
  `$EDITOR`, and C-w and C-u rub out a word and a line.
- `n` on the org-varde board writes the task in a buffer of its own,
  the title on the first line and the context below, where it asked
  for one line of each in the minibuffer. C-c C-c creates it.

### Fixed

- org-varde starts a task without blocking Emacs, which waited while
  varde made the worktree (minutes, in a repository with submodules).

## 1.0.1 - 2026-10-12

### Added

- `s` in the board's recall pane starts the picked hit's task, asking
  first to reopen it if it is closed.

## 1.0.0 - 2026-10-12

The first public release.

- **Tasks** over Claude Code and pi sessions: start, resume, fork,
  park, close and throw away, with live status from the agents' hooks.
- **The board** (`varde`): a terminal UI that groups tasks by what they
  need from you, with a window per session inside tmux.
- **Projects:** a task belongs to a repository, and can move to
  another (`varde set --project`, or `m` on either board).
- **Worktrees** per task that are removed once their work has landed,
  and kept under `refs/varde/discarded/` when thrown away.
- **Memory:** a cairn per task and a project cairn, updated in the
  background, handed to every session, searchable with `varde recall`,
  and measured with `varde eval`.
- **org-varde**, the Emacs client: org headings as varde tasks, and a
  board where RET on a region starts every task in it.
