# bonsai 🪴

[![release](https://github.com/aymericbeaumet/bonsai/actions/workflows/release.yml/badge.svg)](https://github.com/aymericbeaumet/bonsai/actions/workflows/release.yml)
[![latest release](https://img.shields.io/github/v/release/aymericbeaumet/bonsai)](https://github.com/aymericbeaumet/bonsai/releases/latest)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

**Git worktrees without the bookkeeping.** bonsai creates the branch and the
worktree in one command, keeps every worktree in one predictable place, lets
you fuzzy-jump between them, and cleans them up once they are merged, even
when squash-merged. Built for running several coding agents in parallel, and
just as handy by hand.

![bonsai demo: create a worktree, push a fix, fuzzy-jump to another worktree, and clean up after the squash-merge](docs/demo/demo.gif)

## Why

`git worktree` lets you work on several branches at once, but every task
means choosing a directory, creating the branch, `cd`-ing there, copying your
`.env`, and reinstalling dependencies. Afterwards you remove both the worktree
and the branch, which `git branch --merged` cannot even detect once the pull
request was squash-merged. bonsai does the bookkeeping:

- **One command to start.** `bonsai add fix-login` fetches, branches off the
  latest default branch, creates the worktree, copies untracked local config
  (`.env*`, `.envrc`, `.mcp.json`, ...), installs dependencies from the
  lockfile, and drops you into it.
- **One place for everything.** Worktrees live outside your clone, at
  `~/.bonsai/github.com/<owner>/<repo>/<branch>`, so they never clutter the
  repository and every tool can find them.
- **Fuzzy navigation.** `bonsai cd` jumps between worktrees, most recently
  used first; `-a` jumps across all your projects.
- **Safe cleanup.** `bonsai clean` detects merged *and* squash-merged
  branches, shows the plan before acting, and never touches a worktree with
  uncommitted changes.
- **Free disk, keep your work.** `bonsai trim` deletes what git ignores
  (`node_modules`, `target`, `.venv`, build caches) across worktrees on every
  core, and keeps worktrees, branches, local config, and databases.
- **A headquarters.** `bonsai` (or `bonsai hq`) shows every project with its
  checkouts, running coding agents, uncommitted changes, upstream state, and
  disk size, and runs all of the above from one screen with vim keys.
- **Ready for coding agents.** Idempotent commands, `--json` output, and a
  bundled [Agent Skill](docs/agents.md) for Claude Code, Codex, OpenCode, and
  Cursor. `bonsai ps` shows which agent works in which worktree, removal and
  cleanup never pull a worktree from under a running agent, and
  `bonsai resume` finds agent sessions across all your worktrees.
- **Editor-friendly.** Maintained `.code-workspace` files show every worktree
  in VS Code, Cursor, and other VS Code derivatives.

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
macOS, and Windows on amd64 and arm64; Linux binaries are static and run on
any distribution. You can also download them from
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

```sh
cd ~/src/my-repo
bonsai add fix-parser   # branch + worktree from the latest default branch, cd into it
# ...hack, commit, push, open a pull request...
bonsai cd               # fuzzy-jump between worktrees
bonsai list             # show this repository's worktrees
bonsai clean            # once merged: remove the worktree and delete the branch
bonsai trim             # free node_modules, target, ... in the worktrees you keep
bonsai                  # hq: every worktree with its agents, changes, and size
```

Leave out an argument and commands open a fuzzy picker. Run `bonsai` alone
to see every project and worktree with its agents, changes, and size, and
act on them from one screen.

## Commands

| Command | What it does |
|---|---|
| `bonsai add [branch]` | Create (or reuse) a branch and its worktree, then cd into it |
| `bonsai cd [query]` | Fuzzy-jump to a worktree, most recently used first |
| `bonsai list` | List the repository's worktrees (`--status`, `--json`) |
| `bonsai remove [branch…]` | Remove worktrees; `-d` also deletes their branches |
| `bonsai clean` | Remove merged and squash-merged worktrees and their branches |
| `bonsai trim [branch…]` | Delete ignored files (`node_modules`, `target`, ...); worktrees stay |
| `bonsai prune` | Tidy stale registrations and orphaned directories |
| `bonsai` / `bonsai hq` | See and manage every project, worktree, and agent in a terminal UI |
| `bonsai ps` | List running coding agents and the worktree each works in |
| `bonsai start` / `resume` | Start or resume Claude Code, Codex, or OpenCode sessions |
| `bonsai workspace` | Print the `.code-workspace` file for VS Code-style editors |
| `bonsai skill` / `agents` | Teach your coding agents to use bonsai |

`-a` widens most commands to every project, and every command has a detailed
`--help`; `bonsai --help` groups them by task. `trim`, `clean`, and `prune`
share `-n`/`--dry-run`, `-y`/`--yes`, and `--json`. See the
[command reference](docs/commands.md).

## Documentation

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
