# bonsai [![ci](https://github.com/aymericbeaumet/bonsai/actions/workflows/ci.yml/badge.svg)](https://github.com/aymericbeaumet/bonsai/actions/workflows/ci.yml)

Ergonomic git worktree manager. Use it from inside any clone; the worktrees it
creates are centralized under a global root (`~/.bonsai/<repo-id>/<branch>/`),
branches are created automatically, and every command falls back to a fuzzy
picker when you omit an argument.

```sh
cd ~/workspace/my-repo
bonsai add fix-parser     # creates branch + worktree, cds into it
# ...hack, commit, open a PR, squash-merge it...
bonsai clean              # removes merged worktrees and their branches
```

## Install

```sh
cargo install --git https://github.com/aymericbeaumet/bonsai bonsai-cli
```

Then enable the shell integration (auto-cd + completions) in your rc file:

```sh
eval "$(bonsai init zsh)"    # or bash
bonsai init fish | source    # fish
```

Without the wrapper everything still works — cd-capable commands print the
target path, so `cd "$(bonsai add foo)"` composes.

## Commands

| Command | Description |
|---|---|
| `bonsai add [branch]` | Create a worktree under the bonsai root and cd into it. The branch is created from the default branch if it doesn't exist, or set up to track its remote counterpart. No argument opens a fuzzy prompt (type a new name to create it). |
| `bonsai list` (`ls`) | List the current repo's worktrees. `--all` lists every bonsai worktree, `--status` adds a dirty marker. |
| `bonsai cd [query]` | Fuzzy-jump between worktrees. Works globally (across all repos) when run outside a repo. |
| `bonsai workspace` | Refresh and print the repo's `.code-workspace` file: `code "$(bonsai workspace)"`. |
| `bonsai remove [branch…]` (`rm`) | Remove worktrees (fuzzy multi-pick without arguments). Keeps the branch unless `-d`; `--force` discards uncommitted changes. Safe to run from inside the worktree being removed. |
| `bonsai clean` | Remove every worktree whose branch is merged into the default branch — including squash-merges and branches whose upstream is gone (the GitHub PR flow). Deletes the branches too. Fetches `--prune` first (`--no-fetch` to skip), always shows the plan, `-n`/`--dry-run`, `-y`/`--yes`. Dirty worktrees are never touched. |
| `bonsai prune` | Clean up stale worktree registrations, orphaned directories, and empty dirs. `--all` sweeps the whole root, including worktrees of repos whose clone was deleted. |
| `bonsai init <shell>` | Print the shell wrapper (zsh, bash, fish). |
| `bonsai agents` | Print a usage contract for AI coding agents, ready for `bonsai agents >> AGENTS.md`. |
| `bonsai skill [install]` | Print the bundled Agent Skill, or install it for detected harnesses (Claude Code, Codex, OpenCode, Cursor). |
| `bonsai completions <shell>` | Print shell completions. |

Every subcommand has detailed `--help` with examples.

Note: a freshly added worktree with no commits counts as merged (same
semantics as `git branch --merged`), so `bonsai clean` will offer to remove
it — the confirmation prompt and `--dry-run` are there for a reason.

## Configuration

Precedence, low to high: defaults < `~/.config/bonsai/config.toml` <
`<repo>/.bonsai.toml` (checked in, team policy) < `git config bonsai.*`
(per-clone personal overrides, like `user.email`) < `BONSAI_*` environment
variables (`__` nests: `BONSAI_CLEAN__FETCH=false`) < CLI flags
(`--root`, `--remote`).

`.bonsai.toml` is read from the worktree you are standing in (your branch's
version wins), falling back to the main worktree. The git-config layer uses
the same keys, e.g.:

```sh
git config bonsai.root ~/src/worktrees          # this clone only
git config --global bonsai.clean.fetch false    # everywhere
git config --add bonsai.add.copy ".env"         # multi-valued
```

```toml
root = "~/.bonsai"          # where worktrees live
remote = "origin"           # optional: defaults to checkout.defaultRemote, then origin
default_branch = "main"     # optional: skip detection
workspace = true            # maintain a .code-workspace file per repo

[add]
fetch = false               # fetch --prune before adding
post_add = "mise install"   # command run inside a new worktree
# untracked files copied into new worktrees; setting this replaces the
# defaults: [".env", ".env.*", ".envrc", ".mcp.json", "CLAUDE.local.md",
#           ".claude/settings.local.json", ".cursor/mcp.json"]
copy = [".env*", ".envrc"]

[clean]
fetch = true                # fetch --prune before computing merged branches
protected = ["release/*"]   # branch globs never cleaned
```

## Working from inside a worktree

Every command anchors on the main worktree via git itself, so bonsai behaves
identically whether you run it from the original clone or from any worktree —
`bonsai add` from inside one worktree creates a sibling for the same repo.
A few things are deliberately relative to *where you stand*:

- `--base` refs resolve in your current worktree: `bonsai add fixup --base
  HEAD` from inside `feat-a` stacks the new branch on `feat-a`'s HEAD.
- `[add] copy` files are taken from your current worktree first (they carry
  your freshest local `.env` tweaks), then the main worktree.
- `.bonsai.toml` is read from your current checkout first.

New branches are created with `--no-track` (no phantom upstream on
`origin/main`), so `git push` with `push.autoSetupRemote` does the right
thing and `bonsai clean` can detect squash-merges reliably.

## AI agents (Claude Code, Cursor, Codex, OpenCode, ...)

bonsai is built to work the same across coding harnesses, so switching tools
mid-project costs nothing.

**Install the skill.** The repo ships an [Agent Skill](https://agentskills.io)
(`skills/bonsai/SKILL.md`) teaching agents the workflow, its invariants, and
the destructive-command policy. It is embedded in the binary:

```sh
bonsai skill install        # detects Claude Code, Codex, OpenCode, Cursor
bonsai skill install --all  # or install for every harness unconditionally
bonsai skill                # or print it and pipe it wherever you want
```

Claude Code users can track releases through the plugin marketplace instead:

```
/plugin marketplace add aymericbeaumet/bonsai
/plugin install bonsai@bonsai
```

**Or drop a snippet in your instructions file**: `bonsai agents >> AGENTS.md`
prints a shorter usage contract for the cross-harness instructions file.

**Everything is scriptable**: `path=$(bonsai add feat-x)` prints the worktree
path and is idempotent; `bonsai list --json` and `bonsai clean --dry-run
--json` give structured output for a reliable inspect-then-execute loop;
`clean --yes` / `prune --yes` never prompt; fuzzy pickers only ever appear on
a real terminal.

**New worktrees come pre-provisioned**: untracked local config — `.env*`,
`.envrc`, `.mcp.json`, `CLAUDE.local.md`, `.claude/settings.local.json`,
`.cursor/mcp.json` — is copied over by default, so whichever harness (or
human) created the worktree, the others find their setup in place.

## Desktop editors

The bonsai root is structured so GUI tools get worktrees for free:

- **VS Code / Cursor / Windsurf** (and other VS Code derivatives): bonsai
  maintains a multi-root workspace file per repo at
  `~/.bonsai/<repo-id>/<repo>.code-workspace` — the main checkout plus every
  worktree, labelled by branch, kept in sync by add/remove/clean/prune.
  Open everything in one window: `code "$(bonsai workspace)"` or
  `cursor "$(bonsai workspace)"`. Disable with `workspace = false`.
- **Claude Code desktop / Codex desktop** (folder-based apps): every
  worktree is a plain directory named after its branch under
  `~/.bonsai/<host>/<owner>/<repo>/`, so folder pickers and recent-project
  lists stay readable. Jump from a terminal with `bonsai cd`.

## Integrations

- **git**: respects `checkout.defaultRemote` and `init.defaultBranch`;
  configurable through `git config bonsai.*`; all operations shell out to
  your system `git`.
- **zsh/bash/fish**: wrapper + completions via `bonsai init`. The wrapper
  uses a plain `cd`, so `chpwd`-based tools (zoxide, direnv, starship) pick
  up worktree jumps automatically.
- **direnv**: `.envrc` files copied by bonsai from your own worktree are
  `direnv allow`ed automatically; tracked ones stay gated by direnv as usual.

## How it works

- Worktrees live at `<root>/<repo-id>/<branch>`, where the repo-id is derived
  from the remote URL (`github.com/owner/repo`, nested groups preserved) or a
  hash of the repo path when there is no remote. Branch names map verbatim to
  nested directories (`feature/login` → `feature/login/`).
- The shell wrapper captures stdout and watches for a sentinel line to cd;
  prompts render on stderr, so fuzzy pickers work even inside `$(...)`.

## License

[MIT](./LICENSE)
