# varde

[![CI](https://github.com/Smebbs/varde/actions/workflows/ci.yml/badge.svg)](https://github.com/Smebbs/varde/actions/workflows/ci.yml)

**Task management and memory for AI coding agents.** varde turns agent
sessions into tasks you can start, park, resume and close, each in its
own git worktree if you like. It keeps what the sessions learned, and hands that
to the next session.

**varde in a terminal:** the board, with a tmux window per session.

https://github.com/user-attachments/assets/d2f80466-2e66-4ef1-bac3-c51dc6594252

**org-varde in Emacs:** org headings as tasks.

https://github.com/user-attachments/assets/21ad9ebe-255b-4dff-ab87-76012172c2be

## Features

- **Tasks outlive sessions.** A task holds its sessions together:
  resumes, forks and `/clear`s. Starting a task goes to its live
  session, resumes the last one, or starts fresh.
- **A board of what needs you.** Tasks are grouped as Needs you,
  Working, Open, Inbox, Done and Parked, with live status from the
  agent's hooks. Inside tmux, each session gets a window of its own.
- **Worktrees that clean up after themselves.** A task's worktree goes
  once its work has landed, by a local merge or a merged PR (squashed
  ones too). Work you throw away is kept under a git ref.
- **Memory that stays true.** Each task keeps a *cairn*, a short record
  of what its sessions found and decided. Each project keeps one too.
  Both are handed to every new session. A bullet that names code is
  marked when the code is gone. `varde recall` searches all of it, and
  `varde eval` measures what it was worth.
- **No daemon.** One binary and one SQLite file. A hook takes a few
  milliseconds.

## Agents

- **Supported:** [Claude Code](https://code.claude.com) and
  [pi](https://pi.dev).
- **Planned:** Codex and Gemini CLI.
- **Adding one** is an adapter in
  [`cli/crates/varde-agent`](cli/crates/varde-agent): it maps the
  agent's hook events to varde's, and knows how to launch and resume
  it. The Claude and pi adapters are the examples.

## Front ends

- **Supported:**
  - the terminal board, `varde`, in any terminal; inside tmux, each
    session gets a window of its own;
  - [org-varde](docs/emacs.md) for Emacs, with org headings as tasks.
- **Planned:** a desktop or web board, a VS Code extension, and
  Markdown task files, as org-varde does for org.
- **Adding one** needs no Rust: a front end shells out to `varde` and
  reads its `--json`. [`docs/clients.md`](docs/clients.md) is the
  contract, and org-varde is the example.

varde runs on Linux and macOS. An issue or a PR is welcome for anything
planned.

## Install

You need git, and Claude Code or pi. tmux 3.0 or later is optional.

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
varde new Fix the login bug   # a task in this repository
varde                         # the board: RET starts the task, ? shows the keys
```

[The guide](docs/guide.md) covers every command, the board, memory and
configuration. [The design](docs/varde-design.md) says why varde is
built the way it is.

## Contributing

Bug reports and ideas are welcome as
[issues](https://github.com/Smebbs/varde/issues).
[CONTRIBUTING.md](CONTRIBUTING.md) says how to build, test and release.

## License

[MIT](LICENSE)
