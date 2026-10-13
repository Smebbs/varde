# varde and org-varde

The design and its rationale are in `docs/varde-design.md`. Read it
before changing the architecture. Much of what looks like a missing
feature was left out on purpose: §12 lists what was dropped.

## varde (`cli/`)

The core: a Rust CLI. It owns the store, the status machine, the attach
decision, worktrees and cairns.

```bash
cd cli && cargo test && cargo clippy --all-targets && cargo fmt --check
```

- **Tests use a temp store** (`VARDE_HOME`) and never spawn the real
  agent. Launch and memory tests use a fake `claude` (`VARDE_CLAUDE`)
  that fires real hooks and answers like the model.
- **A hook must always print a JSON object and exit 0.**
- **Budgets:** hook p99 under 5 ms (10 ms for the once-per-session
  hooks that deliver memory), and `varde list --json` under 10 ms at
  1 000 tasks. `cargo test --release -p varde --test bench -- --ignored
  --nocapture` measures them.

## org-varde (the `.el` files)

The Emacs client. It shells out to `varde` and holds no state of its
own. Run ERT in **batch mode**, never through `emacsclient --eval`,
which blocks the user's Emacs:

```bash
emacs --batch -Q -L . -L test -l test-helper -l ert \
  -l org-varde -l org-varde-board \
  -l org-varde-test -l org-varde-board-test \
  -f ert-run-tests-batch-and-exit
emacs --batch -Q -L . -l test/test-helper.el -f batch-byte-compile org-varde.el org-varde-board.el && rm -f *.elc
```

- **Every test runs under `org-varde-test-with-varde`.** That macro
  swaps in a fake `varde` script and records terminals instead of
  opening them.
- **No test touches** the real store, the real org tree, or a terminal.

## Shape

| File | Owns |
|---|---|
| `cli/crates/varde-core` | `store/` (schema, migrations), status, attach decision (`plan.rs`), board, `cairn.rs` (the document and its edits), `memory/` (bullets, update jobs, recall) |
| `cli/crates/varde-agent` | the Claude and pi adapters: hook payloads, launch flags, transcript digest, tool calls |
| `cli/crates/varde` | the CLI, hook, `run` supervisor, `git/`, `job.rs` (memory updates), `deliver.rs` (memory into sessions), `stale.rs`, `eval.rs`, `tui/` |
| `cli/crates/varde/tests` | integration tests; `common/` has the temp store and the fake `claude`, `pi`, `gh` and tmux |
| `cli/plugin` | Claude plugin: only `hooks.json`, every entry calls `varde hook` |
| `org-varde.el` | start/terminals/workspaces, the view (cairn view and edit), `varde:` links, new task from a doc |
| `org-varde-board.el` | renders `varde list --json`; decides nothing |

Rules that keep it reliable:

1. **varde owns every durable fact.** Org holds only `varde:`
   links, which varde never syncs.
2. **Status is derived and never stored.**
3. **Hooks never call Emacs.** Nothing in a render path spawns a
   process.
4. **No LLM runs in an interactive path.** Cairn updates run detached.
5. **Every internal agent call sets `VARDE_INTERNAL=1`.**
