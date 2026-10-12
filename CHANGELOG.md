# Changelog

Notable changes to varde and org-varde. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org).

## Unreleased

### Added

- The board keeps to the project it is opened in: the TUI (in tmux or
  not) from its directory, org-varde from the buffer's. Its title or
  header line says how many tasks are in other projects, and how many
  of those need you. `.` switches to every project and back. A project
  with no task on the board yet, or a directory outside a repository,
  shows every project. `[board] scope = "all"` in the config, or
  `org-varde-board-scope`, opens every project instead.
- `varde list --project [DIR]` lists only that project's tasks.

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
