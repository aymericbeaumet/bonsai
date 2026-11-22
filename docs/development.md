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

CI and the demo use [mise](https://mise.jdx.dev) 2026.11.12. To reproduce
the environment locally, install that version and run these commands in the
clone:

```sh
mise trust .mise.toml
mise run setup
```

Setup installs only this project's tools, even when your global mise
configuration contains other tools.
`.mise.toml` and `mise.lock` pin Rust, Node.js, Python, uv, actionlint,
ShellCheck, Git, and the GitHub CLI for local development and CI. Every
command is a task; `mise tasks` lists them, and options after `--` reach the
underlying command.

| Task | Runs |
|---|---|
| `mise run setup` | Install the pinned development tools from `mise.lock` |
| `mise run check` | Everything a change needs: `fmt-check`, `lint`, `test`, `package`, `actionlint`, `release-helper-tests` |
| `mise run build` / `release` | Debug build of the workspace / optimized CLI |
| `mise run test -- <filter>` | The workspace tests, optionally filtered |
| `mise run shell-check` | Verify Bash, Zsh, and Fish can launch before shell integration tests |
| `mise run fmt` | Format the code |
| `mise run fmt-check` / `lint` / `package` | Check formatting / Clippy / package both crates |
| `mise run actionlint` / `release-helper-tests` | Validate workflows with pinned ShellCheck / test release automation |
| `mise run run -- <args>` | Run the CLI from the checkout |
| `mise run install` | Install the checkout's `bonsai` |
| `mise run doc` | Build and open the library documentation |
| `mise run smoke` | The slower Linux CI checks: `package-managers`, `terminal`, and a quick benchmark |
| `mise run bench -- <options>` | Benchmarks, for example `--worktrees 1 10 100 --sessions 1000 10000 100000` |
| `mise run bench-trim -- <packages> <files/package> <runs>` | Compare deletion primitives on disposable npm-like trees; defaults to `200 100 3` |
| `mise run demo-setup` / `demo` | Install the pinned container CLI and Buildx / re-record the README demo |

Task runs do not silently install tools. CI explicitly installs its job's
tools with `--locked`, and releases use the same Python, Rust, and GitHub CLI
pins. The lock includes all six CI platforms. Commit it together with
`.mise.toml` when updating versions; refresh it with `mise lock`. Actionlint uses the pinned
ShellCheck and disables optional Pyflakes so ambient installations cannot
change its checks.

Use your host Git to clone the repository. After setup, mise supplies Git
on every supported platform, including MinGit on Windows. Native system
compilers, platform SDKs and OS libraries come from the host. CI pins
runner generations; their OS packages receive upstream updates.

On Linux and macOS, setup installs Bash, Zsh and Fish from conda-forge with
their locked libraries. Every Unix CI runner requires all three shells.
The PTY checks (`terminal`) require Unix. The package-manager smoke tests
use the pinned npm (bundled with Node.js), uv and Python.
Benchmarks use disposable repositories and synthetic
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
Linux builds use the pinned Zig compiler and cargo-zigbuild to compile
native dependencies against musl. Each build checks that `--version`
reports the release version.

Release tasks are `release-version`, `release-target -- <triple>`,
`release-stamp`, `release-musl`, `release-archive`, and `release-publish`. The workflows supply
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

The recorder uses the mise-managed Buildx and container CLI to build bonsai
from the checkout. The compiler image, mise binary, and VHS image are pinned
by digest. The recording uses the same locked Rust, Git, and Zsh as the
development environment. It creates a throwaway repository inside the container.
Start Colima on macOS (`colima start`) or use native Docker Engine on Linux.
