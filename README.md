# varde

[![CI](https://github.com/Smebbs/varde/actions/workflows/ci.yml/badge.svg)](https://github.com/Smebbs/varde/actions/workflows/ci.yml)

**Task management and memory for AI coding agents.** varde runs Claude
Code and pi sessions as tasks, shows you which one needs you, and keeps
what each task's sessions learned for the next session.

- **Tasks outlive sessions.** A task holds its sessions together:
  resumes, forks and `/clear`s. Starting it goes to its live session,
  resumes the last one, or starts fresh, in its own git worktree if you
  like. The worktree goes once its work has landed.
- **Moving between sessions.** The board groups tasks by what they need
  from you, with live status from the agent's hooks. One key, Tab, goes
  to the next task that needs you; in tmux, every session has a window
  and `varde next` on a tmux key goes there from anywhere.
- **Cairns.** Each task keeps a *cairn*: what its sessions learned,
  handed to the next one. It is short and capped: a Status line, and
  what was found, decided and left open. Each project keeps one too.
  You see it and fix it a line at a time, and `varde recall` searches
  all of them.
- **No daemon.** One binary and one SQLite file. A hook takes a few
  milliseconds.

**varde in a terminal:** the board, with a tmux window per session.

https://github.com/user-attachments/assets/d2f80466-2e66-4ef1-bac3-c51dc6594252

**org-varde in Emacs:** the board, and tasks linked from your org notes.

https://github.com/user-attachments/assets/21ad9ebe-255b-4dff-ab87-76012172c2be

(Both videos show 1.0; the keys have changed since.)

## Install

You need git, and Claude Code or pi. tmux 3.0 or later is recommended.

<!-- TODO: AUR -->

1. **varde.** With Homebrew (macOS or Linux):

   ```sh
   brew install smebbs/tap/varde
   ```

   Or take the binary for your platform from the
   [latest release](https://github.com/Smebbs/varde/releases/latest)
   and put it on your PATH, or build it with Rust 1.88 or later:

   ```sh
   cargo install --locked --git https://github.com/Smebbs/varde varde
   ```

   `cargo install` puts it in `~/.cargo/bin`. If your shell then says
   `varde: command not found`, that directory is not on your PATH (on
   macOS too): add `export PATH="$HOME/.cargo/bin:$PATH"` to
   `~/.zshrc` (or your shell's profile) and open a new terminal. The
   hooks below call `varde` by name, so do this first.

2. **The hooks.** In Claude Code:

   ```
   /plugin marketplace add Smebbs/varde
   /plugin install varde@varde
   ```

   For pi, see [the guide](docs/guide.md#install).

3. **Check it.** `varde doctor` says what is missing and how to fix it.

## Quick start

In a repository:

```sh
tmux new -s varde varde       # outside tmux: the board, in a tmux session of its own
varde                         # inside tmux: the board, in any window (or the popup below)
```

- `n`, type what the agent should do, and Enter: that is a task.
- RET starts its session in a tmux window of its own, and goes there.
  Prefix `l` comes back to the board.
- When the agent's turn ends, the task moves to **Needs you**. Tab goes
  to the next task that needs you, and RET into its session.
- `v` opens the task's cairn under the board. `j`/`k` pick a line, `e`
  fixes it, `a` adds one (a key picks the section), `x` removes it and
  `u` brings it back. What you write is yours, and updates keep it.
- `d` marks the task done once its work is in. `?` shows every key.

Or open the board in a tmux popup from any window (tmux 3.3 or later):
`bind b display-popup -E -B -w 90% -h 85% "varde --popup"` in
`~/.tmux.conf` (on tmux 3.2, without `-B`); going to a session closes it. Bind `varde next` to a
tmux key to go to the next session that needs you without the board
([the guide](docs/guide.md#inside-tmux) has the lines). Outside tmux the board works too, one session at a time.

In Emacs, [org-varde](docs/emacs.md) has the same board keys and the
same cairn view, and links tasks from your org notes: a link shows
where its task stands, a heading becomes a task with one command, and
marking it DONE offers to close its tasks.

From a shell, `varde new Fix the login bug` makes a task and
`varde attach <id>` starts it. [The guide](docs/guide.md) covers every
command, the board, memory and configuration. [The
design](docs/varde-design.md) says why varde is built the way it is.

## Agents and front ends

- **Agents:** [Claude Code](https://code.claude.com) and
  [pi](https://pi.dev); Codex and Gemini CLI are planned. Adding one is
  an adapter in [`cli/crates/varde-agent`](cli/crates/varde-agent).
- **Front ends:** the terminal board, `varde`, and
  [org-varde](docs/emacs.md) for Emacs. Adding one needs no Rust: a
  front end shells out to `varde` and reads its `--json`;
  [`docs/clients.md`](docs/clients.md) is the contract.

varde runs on Linux and macOS. An issue or a PR is welcome for anything
planned.

## Contributing

Bug reports and ideas are welcome as
[issues](https://github.com/Smebbs/varde/issues).
[CONTRIBUTING.md](CONTRIBUTING.md) says how to build, test and release.

## License

[MIT](LICENSE)
