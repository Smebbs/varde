# varde

[![CI](https://github.com/Smebbs/varde/actions/workflows/ci.yml/badge.svg)](https://github.com/Smebbs/varde/actions/workflows/ci.yml)

Task management and memory for AI coding agents.

varde runs your coding agents as tasks. A board shows which one needs
you, each task can have its own git worktree, and what its sessions
learn is kept in a short *cairn* for the next one. Tasks can also be
scheduled: varde starts them in the background, once or on a repeat,
and they wait for you only when there is something to see. It is one
binary and one SQLite file, with no daemon.

https://github.com/user-attachments/assets/db6de5d4-adc9-4ffa-a14a-04334c62c4bc

## Install

You need git and a supported agent: Claude Code or pi. tmux is
recommended.

```sh
brew install smebbs/tap/varde
# or: cargo install --locked --git https://github.com/Smebbs/varde varde
```

Binaries are on the [releases page](https://github.com/Smebbs/varde/releases/latest).
Then add the hooks in Claude Code (for pi, see [the guide](docs/guide.md#install)):

```
/plugin marketplace add Smebbs/varde
/plugin install varde@varde
```

`varde doctor` checks the setup.

## Usage

```sh
varde                         # the board (outside tmux: tmux new -s varde varde)
varde new Fix the login bug   # a task in this checkout
varde attach k3m9             # start it, resume it, or go to it
varde recall login            # what earlier tasks learned
varde schedule k3m9 every weekday 09:00   # varde starts it then
```

On the board: `n` makes a task, `RET` goes to its session, `Tab` goes
to the next task that needs you, `v` shows its cairn, `/` recalls, `d`
marks it done. `?` lists every key.

## Emacs

[org-varde](docs/emacs.md) is the Emacs client: the same board, and
tasks linked from your org notes.

https://github.com/user-attachments/assets/1d66f737-63ad-49cb-9a39-8b9580a7a86d

## Documentation

- [Guide](docs/guide.md): commands, the board, cairns, configuration
- [org-varde](docs/emacs.md): the Emacs client
- [Writing a client](docs/clients.md): the `--json` contract
- [Design](docs/varde-design.md): why varde is built this way
- [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md)

## License

[MIT](LICENSE)
