# bonsai

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
| `bonsai remove [branch…]` (`rm`) | Remove worktrees (fuzzy multi-pick without arguments). Keeps the branch unless `-d`; `--force` discards uncommitted changes. Safe to run from inside the worktree being removed. |
| `bonsai clean` | Remove every worktree whose branch is merged into the default branch — including squash-merges and branches whose upstream is gone (the GitHub PR flow). Deletes the branches too. Fetches `--prune` first (`--no-fetch` to skip), always shows the plan, `-n`/`--dry-run`, `-y`/`--yes`. Dirty worktrees are never touched. |
| `bonsai prune` | Clean up stale worktree registrations, orphaned directories, and empty dirs. `--all` sweeps the whole root, including worktrees of repos whose clone was deleted. |
| `bonsai init <shell>` | Print the shell wrapper (zsh, bash, fish). |
| `bonsai completions <shell>` | Print shell completions. |

Note: a freshly added worktree with no commits counts as merged (same
semantics as `git branch --merged`), so `bonsai clean` will offer to remove
it — the confirmation prompt and `--dry-run` are there for a reason.

## Configuration

Precedence, low to high: defaults < `~/.config/bonsai/config.toml` <
`<repo>/.bonsai.toml` (checked in, team policy) < `BONSAI_*` environment
variables (`__` nests: `BONSAI_CLEAN__FETCH=false`) < CLI flags
(`--root`, `--remote`).

```toml
root = "~/.bonsai"          # where worktrees live
remote = "origin"           # remote used for tracking/fetching/repo identity
default_branch = "main"     # optional: skip detection

[add]
fetch = false               # fetch --prune before adding
copy = [".env*", ".envrc"]  # untracked files copied into new worktrees
post_add = "mise install"   # command run inside a new worktree

[clean]
fetch = true                # fetch --prune before computing merged branches
protected = ["release/*"]   # branch globs never cleaned
```

## How it works

- Worktrees live at `<root>/<repo-id>/<branch>`, where the repo-id is derived
  from the remote URL (`github.com/owner/repo`, nested groups preserved) or a
  hash of the repo path when there is no remote. Branch names map verbatim to
  nested directories (`feature/login` → `feature/login/`).
- Every command anchors on the main worktree via git itself, so bonsai behaves
  identically whether you run it from the original clone or from any worktree.
- The shell wrapper captures stdout and watches for a sentinel line to cd;
  prompts render on stderr, so fuzzy pickers work even inside `$(...)`.
- All git operations shell out to your system `git`.

## License

[MIT](./LICENSE)
