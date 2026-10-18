# Contributing

Issues and pull requests are welcome. For anything big, open an issue
first. [`docs/varde-design.md`](docs/varde-design.md) explains why
things are the way they are, and §11 lists what was left out on
purpose.

## Running the tests

```sh
cd cli && cargo test && cargo clippy --all-targets && cargo fmt --check
```

```sh
emacs --batch -Q -L . -L test -l test-helper -l ert \
  -l org-varde -l org-varde-board \
  -l org-varde-test -l org-varde-board-test \
  -f ert-run-tests-batch-and-exit
```

Run the Emacs tests in batch like this, not in your running Emacs. CI
runs both.

## Releasing

Bump `version` in `cli/Cargo.toml`, `cli/plugin/.claude-plugin/plugin.json`,
`.claude-plugin/marketplace.json` and `org-varde.el`, move *Unreleased*
in the changelog under the new version, and push a `vX.Y.Z` tag. The
release workflow builds the binaries, publishes the release and, if
the `HOMEBREW_TAP_TOKEN` secret is set, pushes the formula to
[Smebbs/homebrew-tap](https://github.com/Smebbs/homebrew-tap).
