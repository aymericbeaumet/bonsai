# bonsai 🪴

[![release](https://github.com/aymericbeaumet/bonsai/actions/workflows/release.yml/badge.svg)](https://github.com/aymericbeaumet/bonsai/actions/workflows/release.yml)
[![latest release](https://img.shields.io/github/v/release/aymericbeaumet/bonsai)](https://github.com/aymericbeaumet/bonsai/releases/latest)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

**Git worktrees without the bookkeeping.** Keep a feature, a bug fix, and
several coding agents working side by side, each in its own checkout.
One command creates a branch, copies your local config, installs dependencies,
and takes you there. Jump between checkouts and clean up after the merge,
including squash-merges.

A single Rust binary with a CLI and terminal dashboard. Works with your
existing Git worktrees in place; no account, server, or coding agent required.

![bonsai demo: create a worktree, push a fix, fuzzy-jump to another worktree, and clean up after the squash-merge](docs/demo/demo.gif)

## Why

- **Leave unfinished work where it is.** Start another branch without
  stashing, then fuzzy-jump back with `bonsai cd`. New checkouts live under
  `~/.bonsai/<host>/<owner>/<repo>/<branch>`; existing ones stay where they are.
- **See the whole workspace.** `bonsai` opens a terminal dashboard with
  checkouts, running agents, changes, upstream state, and disk usage. Open
  an editor or coding session from the same screen.
- **Finish the cleanup.** `bonsai clean` previews merged and squash-merged
  worktrees before removing them, skipping dirty or busy checkouts.
  `bonsai trim` reclaims ignored builds and dependencies while keeping
  checkouts and protected local state. Both support `--dry-run`.
- **Use your own tools.** Shell integration, VS Code/Cursor workspaces,
  `--json` output, and an optional [Agent Skill](docs/agents.md). Your branches
  and checkouts remain ordinary Git worktrees.

## Install

**Homebrew** (macOS and Linux):

```sh
brew install aymericbeaumet/tap/bonsai
```

**mise** (Linux, macOS, and Windows):

```sh
mise use -g github:aymericbeaumet/bonsai
```

**Cargo** (Rust 1.99.1 or newer):

```sh
cargo install --locked --git https://github.com/aymericbeaumet/bonsai
```

All three install the `bonsai` command. Prebuilt binaries support Linux,
macOS, and Windows on amd64 and arm64; Linux binaries are statically linked.
You can also download them from
[GitHub Releases](https://github.com/aymericbeaumet/bonsai/releases/latest),
verify the archive against `SHA256SUMS`, and put the binary on your `PATH`.

Then enable the shell integration, which lets bonsai `cd` you into worktrees
and adds completions:

```sh
# zsh — ~/.zshrc
eval "$(bonsai init zsh)"

# bash — ~/.bashrc
eval "$(bonsai init bash)"

# fish — ~/.config/fish/config.fish
bonsai init fish | source
```

Without it everything still works: commands that change directory print the
path instead, so `cd "$(bonsai add foo)"` composes.

## Quick start

Start in an existing Git repository. This first command only lists its
checkouts, including any created by other tools:

```sh
cd ~/src/my-repo
bonsai list --status
```

With shell integration enabled:

```sh
bonsai add fix-parser   # branch + worktree from the latest default branch, cd into it
# ...hack, commit, push, open a pull request...
bonsai cd               # fuzzy-jump between worktrees
bonsai clean --dry-run  # after merging: inspect eligible worktrees and branches
bonsai clean            # review the plan, then confirm removal
```

New to worktrees? The [first-worktree walkthrough](docs/getting-started.md)
covers setup, stacked branches, cleanup, and what stays shared between
checkouts. Already have worktrees? `bonsai` opens the dashboard immediately.

## Commands

| Command | What it does |
|---|---|
| `bonsai add [branch]` | Create (or reuse) a branch and its worktree, then cd into it |
| `bonsai cd [query]` | Fuzzy-jump to a worktree, most recently used first |
| `bonsai list` | List the repository's worktrees (`--status`, `--json`) |
| `bonsai remove [target…]` | Delete chosen checkout directories; keep branches unless `-d` |
| `bonsai clean [target…]` | Delete merged or squash-merged checkout directories and their branches |
| `bonsai trim [target…]` | Delete ignored builds/dependencies; keep checkouts and branches |
| `bonsai prune [target…]` | Drop missing checkout registrations and delete orphan directories; keep branches |
| `bonsai` / `bonsai hq` | See and manage every project, worktree, and agent in a terminal UI |
| `bonsai ps` | List running coding agents and the worktree each works in |
| `bonsai start` / `resume` | Start or resume Claude Code, Codex, or OpenCode sessions |
| `bonsai workspace` | Print the `.code-workspace` file for VS Code-style editors |
| `bonsai skill` / `agents` | Teach your coding agents to use bonsai |

`-a` widens most commands to every project, and every command has a detailed
`--help`; `bonsai --help` groups them by task. `trim`, `clean`, and `prune`
share `-n`/`--dry-run`, `-y`/`--yes`, and `--json`. See the
[cleanup comparison and command reference](docs/commands.md#free-disk-or-remove-worktrees).

## Documentation

- [Getting started](docs/getting-started.md): your first worktree, safe previews, and common questions
- [Commands](docs/commands.md): reference, worktree-relative behavior, and safety guarantees
- [Configuration](docs/configuration.md): `.bonsai.toml`, `git config bonsai.*`, and environment variables
- [AI coding agents](docs/agents.md): Agent Skill, AGENTS.md, scripting, and session resume
- [Headquarters](docs/hq.md): the `bonsai hq` terminal UI, its columns, keys, and commands
- [Desktop editors](docs/editors.md): VS Code and Cursor workspaces, folder-based apps
- [Integrations](docs/integrations.md): shells, direnv, package managers, Cargo, and backups
- [How it works](docs/how-it-works.md): layout, repository ids, and the Rust library
- [Development](docs/development.md): building, testing, releases, and the demo

## License

[MIT](LICENSE)
