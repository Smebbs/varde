# Changelog

Notable changes to varde and org-varde.

## Unreleased

## 1.5.0 - 2026-10-18

### Added

- org-varde opens sessions in ghostel. `org-varde-terminals` says
  which terminals you prefer, in order; the first installed is used,
  and `term`, which can't answer fish 4's startup queries, only when
  none is. ghostel runs through Emacs's PTY, not its native one, so
  org-varde can still read the terminal's tty and whether it is busy.

### Changed

- org-varde prefers ghostel to vterm when both are installed. Set
  `org-varde-terminals` to `'(vterm ghostel)` to keep vterm.

## 1.4.1 - 2026-10-18

### Fixed

- A background run is let off the user's look only if it changed
  nothing: it is told that anything it edited or committed, however
  routine, is for the user to see. A run had judged its own commit not
  worth a look.
- org-varde: going to a vterm shows its latest output, at the cursor; a
  task started in the background had opened at the top its workspace
  was laid out with.
- org-varde: TAB in a task's view, off a section heading, goes to the
  next task that needs you; it had failed with a `wrong-type-argument`.
- org-varde: `org-varde-recall` is autoloaded, as its cookie meant.
- org-varde: in a `term`, any prefix under `C-c` that holds one of
  org-varde's commands is kept from term's escape, not only `C-c v`.
- The board no longer exits when a project picker is opened in a
  terminal one or two rows tall.

## 1.4.0 - 2026-10-17

- One project picker on both boards, for every question about a
  project: known projects first, the likeliest picked, and typed
  directories completed. In org-varde it's a `completing-read`.
- The edit form (`e`) has a `Project:` field; changing it moves the
  task, as `m` does.
- The board's new-task form matches org-varde's: it opens in the
  context and has `Title:`, `Project:` and `When:` fields.
- `varde projects --json` lists the projects varde knows.
- The new-task form no longer binds C-j and C-k; TAB and S-TAB go
  between its fields. On the board, C-j is Enter.

## 1.3.2 - 2026-10-17

- The new-task form opens in the context, not the Title.
- Fixes to the form under Doom/evil, and a task is no longer started
  twice when its key is pressed again.

## 1.3.1 - 2026-10-16

- Fixes to the new-task form under evil-org.

## 1.3.0 - 2026-10-16

### Added

- `agents.<agent>.background` in the config: extra arguments for
  scheduled sessions, e.g. `["--permission-mode", "auto"]`.
- `varde schedule <task> --now` runs the next start at once.
- A repeating task's row says how many runs wait unread (`↻ 3 unread`)
  and when a start is held up (`↻ due`, `↻ late`).
- The view lists every session a schedule ran.

### Changed

- `done` on a repeating task ends the run, not the series; end the
  series with `varde schedule --off`.
- Every unopened background run stays unread until opened.
- Recall leaves out lines you removed or rewrote.
- The project cairn is yours: closing a task no longer promotes lines
  into it.
- org-varde: the new-task form's context is written in org-mode.

## 1.2.0 - 2026-10-15

### Added

- Scheduled and repeating tasks: `varde schedule <task> RULE`, `varde
  new --when`, or `When:` in the forms. `varde schedule --install`
  sets up the timer that runs `varde tick`.
- `org-varde-new-here` copies a heading's `SCHEDULED:` into `When:`.

### Changed

- org-varde writes a task in a form with `Title:` and `Project:`
  fields above the context. `C-u C-c C-c` no longer asks for another
  project: change the Project field.
- The board's form: Tab and Shift-Tab go between all fields.

### Fixed

- On macOS the Emacs board no longer misses updates while a session
  runs.

## 1.1.1 - 2026-10-13

- Fixes: two starts of one task at once, and org-varde on Emacs 29/30
  loads `org-element` itself.

## 1.1.0 - 2026-10-13

### Added

- Install with Homebrew: `brew install smebbs/tap/varde`.
- Cairns shown and edited a line at a time: the board's view (`v`),
  `varde show`, the Emacs view, and `varde edit --add/--set/--rm/--restore`.
- `varde --popup` for the board in a tmux popup, and `varde next` to
  go to the next task that needs you.
- Model-written titles for tasks varde titled itself (`[memory] titles
  = false` turns them off).
- The board keeps to the project it's opened in; `.` shows every one.
- `[keys]` in the config rebinds the terminal board's keys.
- `u` reopens the tasks last closed on a board.
- org-varde: `varde:` links (`org-varde-insert-link`), which show their
  task's status; `org-varde-new-here` makes a task from a heading or
  region; DONE on a heading offers to close its tasks; a `#+BEGIN:
  varde` block lists tasks for a review; `org-varde-next-needs-you`.

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

- Every group is always shown, in the same order, on every board.
- "Drop" is the word for throwing a task away everywhere.
- `X` in the terminal view drops the task (`x` there removes a line).
- `varde new` and `varde set` take `--context`; `--body` still works.
- org-varde: `N` runs the configured agent, not always claude.

### Removed

- **org-varde no longer makes org headings into tasks.** Gone: the
  `VARDE_TASK` property, keyword changes closing tasks, archiving,
  `org-varde-done`, the board's `f`, `l` and `o`, the view's `h`, and
  the options `org-varde-directories`, `org-varde-start-goes`,
  `org-varde-park-keywords`, `org-varde-drop-keywords`,
  `org-varde-settle-delay` and `org-varde-context-words`.
  `org-varde-start` and `org-varde-link` are obsolete aliases.
  **Migrating:** `M-x org-varde-convert-links` (`C-u` for every org
  file) puts a link to each heading's task in its text; save the files
  after.

## 1.0.3 - 2026-10-12

- Tab completes directories in the board's project prompts.
- `e` on a task without a cairn edits it in the new-task form.

## 1.0.2 - 2026-10-12

- C-g in the board's new-task form writes the task in `$EDITOR`.
- `n` on the org-varde board writes the task in a buffer of its own.
- org-varde starts a task without blocking Emacs.

## 1.0.1 - 2026-10-12

- `s` in the board's recall pane starts the picked task.

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
