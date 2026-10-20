# Development

## Layout

- `bonsai-cli/` — the `bonsai-cli` package: the `bonsai` binary and its
  integration tests.
- `bonsai-lib/` — the `bonsai-lib` library (imported as `bonsai`) with the
  commands behind the CLI.
- `skills/bonsai/SKILL.md` — the bundled Agent Skill, embedded at build time
  and printed by `bonsai skill` and `bonsai agents`.
- `tests/terminal.py` — release PTY checks; `scripts/benchmark.py` —
  disposable benchmarks.
- `docs/` — this documentation; `docs/demo/` — the README demo.

The CLI is not published to crates.io. Install the latest version from
GitHub with `cargo install --locked --git https://github.com/aymericbeaumet/bonsai`,
or your clone's version with `cargo install --path . --locked`.

## Checks

Install [mise](https://mise.jdx.dev), then run `mise trust .mise.toml` and
`mise install --locked rust node python uv actionlint` in the clone. `.mise.toml` and
`mise.lock` pin Rust, Node.js, Python, uv, and actionlint for local development and CI.
Use `mise exec --` for commands in shells without mise activation. The Linux
CI shell tests also need Bash, Zsh, and Fish from the OS.

```sh
mise exec -- cargo test --locked
mise exec -- cargo clippy --locked --all-targets -- -D warnings
mise exec -- cargo fmt --check
mise exec -- actionlint .github/workflows/ci.yml .github/workflows/release.yml
mise exec -- cargo package --workspace --locked
mise exec -- cargo build --release --locked
mise exec -- python3 tests/terminal.py target/release/bonsai
mise exec -- cargo test --locked --test cli real_package_managers_ -- --ignored
mise exec -- python3 scripts/benchmark.py --worktrees 1 10 100 --sessions 1000 10000 100000
```

The PTY checks require Unix. Package-manager smoke tests require npm, uv, and
Python. Benchmarks use disposable repositories and synthetic session stores,
report first-run and warm median/p95 latency plus Git process counts, and never
access personal sessions.

## Releases

Every push to `main` runs the test suite, then tags and publishes a GitHub
release with binaries for Linux, macOS, and Windows (amd64 and arm64). The
version is derived from Conventional Commits since the last tag: a breaking
change bumps the major version, `feat` the minor, anything else the patch.
The `version` in `Cargo.toml` is a placeholder stamped at release time.

## Demo

`docs/demo/demo.gif` is recorded with [VHS](https://github.com/charmbracelet/vhs)
from `docs/demo/demo.tape`. Re-record it after changing the commands or output
it shows:

```sh
docs/demo/record.sh
```

The script builds bonsai from the checkout inside a container and records in
a throwaway environment (`docs/demo/setup.sh`), so it never touches your
repositories or `~/.bonsai`. It needs Docker; on macOS, run `colima start`
first.
