# Development

## Layout

- `bonsai-cli/` — the `bonsai-cli` package: the `bonsai` binary and its
  integration tests.
- `bonsai-lib/` — the `bonsai-lib` library (imported as `bonsai`) with the
  commands behind the CLI.
- `skills/bonsai/SKILL.md` — the bundled Agent Skill, embedded at build time
  and printed by `bonsai skill` and `bonsai agents`.
- `tests/terminal.py` — release PTY checks; `scripts/benchmark.py` —
  disposable benchmarks; `bonsai-lib/examples/trim-benchmark.rs` —
  deletion benchmarks.
- `docs/` — this documentation; `docs/demo/` — the README demo.

The CLI is not published to crates.io. Install the latest version from
GitHub with `cargo install --locked --git https://github.com/aymericbeaumet/bonsai`,
or your clone's version with `mise run install` after the setup below.

## Tasks

mise is the single development environment. Install
[mise](https://mise.jdx.dev) 2026.10.17 or newer, then in the clone run
`mise trust .mise.toml` and `mise run setup`. This installs only this project's
tools, even if your global mise configuration contains other tools.
`.mise.toml` and `mise.lock` pin Rust, Node.js, Python, uv, actionlint,
ShellCheck, and the GitHub CLI for local development and CI. Every
command is a task; `mise tasks` lists them, and options after `--` reach the
underlying command.

| Task | Runs |
|---|---|
| `mise run setup` | Install the pinned development tools from `mise.lock` |
| `mise run check` | Everything a change needs: `fmt-check`, `lint`, `test`, `package`, `actionlint`, `release-helper-tests` |
| `mise run build` / `release` | Debug build of the workspace / optimized CLI |
| `mise run test -- <filter>` | The workspace tests, optionally filtered |
| `mise run fmt` | Format the code |
| `mise run fmt-check` / `lint` / `package` | Check formatting / Clippy / package both crates |
| `mise run actionlint` / `release-helper-tests` | Validate workflows with pinned ShellCheck / test release automation |
| `mise run run -- <args>` | Run the CLI from the checkout |
| `mise run install` | Install the checkout's `bonsai` |
| `mise run doc` | Build and open the library documentation |
| `mise run smoke` | The slower Linux CI checks: `package-managers`, `terminal`, and a quick benchmark |
| `mise run bench -- <options>` | Benchmarks, for example `--worktrees 1 10 100 --sessions 1000 10000 100000` |
| `mise run bench-trim -- <packages> <files/package> <runs>` | Compare deletion primitives on disposable npm-like trees; defaults to `200 100 3` |
| `mise run demo-setup` / `demo` | Install the pinned container CLI / re-record the README demo |
| `mise run review-setup` / `review -- <args>` | Install the locked optional Greptile tool / run a review (requires a Greptile account) |

Task runs do not silently install tools. CI explicitly installs its job's
tools with `--locked`, and releases use the same Python, Rust, and GitHub CLI
pins. The lock includes all six CI platforms and the optional review tool's
npm dependency graph under `.mise/locks/`. Commit those files together when
updating versions; refresh them with `mise lock`. Actionlint uses the pinned
ShellCheck and disables optional Pyflakes so ambient installations cannot
change its checks.

The host's Git, native compiler/linker, and OS libraries remain platform
prerequisites. CI pins runner generations; their OS packages receive upstream
updates. Tool and dependency pins make the development environment repeatable,
but do not promise byte-identical binaries across operating systems.

The PTY checks (`terminal`) require Unix. Package-manager smoke tests
require npm, uv, and Python. The Linux CI shell tests also need Bash, Zsh,
and Fish from the OS. Benchmarks use disposable repositories and synthetic
session stores, report first-run and warm median/p95 latency plus Git
process counts, and never access personal sessions.

`bench-trim` creates fresh package trees in a temporary directory and compares
the former metadata-based progress walker, the standard library remover,
and Bonsai's progress remover with 1, 2, 4, and default worker counts.
Creation and compilation are excluded from
the reported deletion times. It accepts only tree dimensions and run count,
so it cannot target an existing checkout. Tree deletion fans out the first
directory levels inside the caller's worker pool. On macOS, CLI deletion
shares a four-worker pool to reduce APFS unlink contention.

## Releases

Every push to `main` runs the test suite, then tags and publishes a GitHub
release with binaries for Linux, macOS, and Windows (amd64 and arm64). The
version is derived from Conventional Commits since the last tag: a breaking
change bumps the major version, `feat` the minor, anything else the patch.
The `version` in `Cargo.toml` is a placeholder stamped at release time.
Linux binaries link musl statically; each build checks that `--version`
reports the release version.

Release tasks are `release-version`, `release-target -- <triple>`,
`release-stamp`, `release-archive`, and `release-publish`. The workflows supply
`VERSION`, `TARGET`, and `ARCHIVE` where needed. `release-helper -- homebrew`
and `release-helper -- crate` run the post-release distribution steps. These
tasks use the same locked tools locally and in CI; publishing needs the
corresponding tokens.

After the GitHub release, two jobs run `.github/scripts/release.py`:

- `homebrew` regenerates `Formula/bonsai.rb` in
  [aymericbeaumet/homebrew-tap](https://github.com/aymericbeaumet/homebrew-tap)
  from the release's `SHA256SUMS`. It needs the `HOMEBREW_TAP_TOKEN` secret,
  with write access to the tap.
- `crate` publishes `bonsai-lib` at the release version. It needs the
  `CARGO_REGISTRY_TOKEN` secret. The `bonsai-cli` package has
  `publish = false`: that name on crates.io belongs to another project, so
  the CLI installs from GitHub, Homebrew, or mise.

Both are idempotent: rerunning a failed job skips what is already done. Test
the helper with `mise run release-helper-tests`.

## Demo

`docs/demo/demo.gif` is recorded with [VHS](https://github.com/charmbracelet/vhs)
from `docs/demo/demo.tape`. Re-record it after changing the commands or output
it shows:

```sh
mise run demo-setup
mise run demo
```

It runs `docs/demo/record.sh`, which installs the locked Rust toolchain through
mise inside a digest-pinned container, builds bonsai from the checkout, and records in
a throwaway environment (`docs/demo/setup.sh`), so it never touches your
repositories or `~/.bonsai`. It needs native Docker Engine on Linux or Colima on macOS (`colima start`).
