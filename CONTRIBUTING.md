# Contributing

Thanks for looking. varde is young, so an issue before a large change
saves us both time. [`docs/varde-design.md`](docs/varde-design.md) says
why varde is built the way it is. Much of what looks like a missing
feature was left out on purpose, and §12 lists what was dropped.

## The layout

| Path | What it is |
|---|---|
| `cli/` | varde itself: a Cargo workspace (`varde-core`, `varde-agent`, `varde`) |
| `cli/plugin/` | the Claude Code plugin: only `hooks.json`, every entry calls `varde hook` |
| `cli/pi/` | the pi extension, which forwards pi's events to `varde hook` |
| `*.el`, `test/` | org-varde, the Emacs client, and its tests |
| `docs/` | the guide, the Emacs guide, the client contract and the design |

## varde

```sh
cd cli
cargo test && cargo clippy --all-targets && cargo fmt --check
cargo test --release -p varde --test bench -- --ignored --nocapture   # the latency budgets
```

- **Tests are sandboxed.** They use a temp store through `VARDE_HOME`
  and never spawn the agent. Launch tests use a fake `claude`
  (`VARDE_CLAUDE`) that fires real hooks, and a fake `pi` (`VARDE_PI`)
  that sends what the pi extension sends.
- **Fixtures are real.** The files in
  `cli/crates/varde-agent/fixtures/claude/` are real payloads captured
  from Claude Code, anonymised.
- **A hook always prints a JSON object and exits 0.** A hook that fails
  must never break the agent's session.
- **Budgets:** hook p99 under 5 ms (10 ms for the once-per-session
  hooks that deliver memory), and `varde list --json` under 10 ms at
  1 000 tasks.
- **The schema** is the numbered list in
  `cli/crates/varde-core/src/store/migrations.rs`. A change appends a
  migration; one that has shipped is never edited.

## org-varde

Run ERT in batch mode, never in your running Emacs:

```sh
emacs --batch -Q -L . -L test -l test-helper -l ert \
  -l org-varde -l org-varde-board \
  -l org-varde-test -l org-varde-board-test \
  -f ert-run-tests-batch-and-exit
emacs --batch -Q -L . -l test/test-helper.el -f batch-byte-compile org-varde.el org-varde-board.el && rm -f *.elc
```

Every test runs under `org-varde-test-with-varde`, which swaps in a
fake `varde` and records terminals instead of opening them.

## Pull requests

- Keep a change to one thing, with tests for what it changes.
- CI runs everything above. Run it locally first.
- Start a commit message with what it changes, `varde:`, `org-varde:`
  or `docs:`, then say what is now true: `varde: a dropped task keeps
  its work`.
- Add a line to [`CHANGELOG.md`](CHANGELOG.md) under *Unreleased* when
  a user would notice the change.
- `main` takes one commit per change: pull requests are squash-merged
  (the only merge GitHub allows here), and work landed locally is
  squashed the same way (`git merge --squash`, or a soft reset before
  pushing), so each commit on `main` is a whole change.

## Releasing

Bump `version` in `cli/Cargo.toml`, `cli/plugin/.claude-plugin/plugin.json`,
`.claude-plugin/marketplace.json` and `org-varde.el`, move *Unreleased* in the changelog under the new
version, and push a `vX.Y.Z` tag. The release workflow builds the
binaries and publishes the release. Then it fills the templates in
`packaging/` with the version and checksums (`packaging/render`) and
pushes the formula to
[Smebbs/homebrew-tap](https://github.com/Smebbs/homebrew-tap). That
needs the repository secret `HOMEBREW_TAP_TOKEN`, a token that can push
to the tap, and is skipped without it.
