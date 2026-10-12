# Changelog

Notable changes to varde and org-varde. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org).

## Unreleased

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
