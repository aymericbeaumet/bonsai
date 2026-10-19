# Commands

Every subcommand has detailed `--help` with examples; this page is the
overview.

## Scope

Inside a repository, commands act on that project; `-a`/`--all` widens them
to every project under the root. Outside a repository every project is in
scope, and commands that need one project (`add`, `start`) prompt for it.
`-C <dir>` (`--cwd`) runs bonsai as if it was started in `<dir>`, like
`git -C`, and composes with `-a`.

## Reference

| Command | Description |
|---|---|
| `bonsai add [branch]` | Preserve an existing branch name, or slugify a new name (preserving `/`), fetch the remote, then create a worktree under the bonsai root and cd into it. New branches start from the latest default branch; remote branches get a local tracking branch. No argument opens a fuzzy prompt (type a new name to create it). If the worktree's directory was deleted by hand, `add` recreates it. `-a` picks the project first. |
| `bonsai cd [query]` | Fuzzy-jump between registered worktrees, including external ones, listed most recently worked-in first with a color-coded last-change age (green = today, yellow = this week, dim = older). `-a` jumps across every project. |
| `bonsai list` (`ls`) | List every Git-registered worktree for the current repo, including external worktrees created by other tools. `-a` lists every project, prefixed by repo id; `--status` adds a dirty marker. |
| `bonsai remove [branch-or-path…]` (`rm`) | Remove worktrees (fuzzy multi-pick without arguments). Exact managed paths also support detached worktrees. Keeps the branch unless `-d`; `--force` discards uncommitted changes. Safe to run from inside the worktree being removed. With `-a`, a branch selects its worktree in every project. |
| `bonsai clean` | Remove worktrees whose branches are merged or squash-merged into the default branch, then delete those branches. A missing upstream alone never proves integration. Fetches `--prune` first (`--no-fetch` to skip), always shows the plan, `-n`/`--dry-run`, `-y`/`--yes` (alias `-f`/`--force`). Dirty worktrees are never touched, and worktrees whose directory was deleted are skipped (drop them with `remove`). `-a` confirms one combined plan for every project. |
| `bonsai prune` | Clean up stale worktree registrations, orphaned directories, and empty dirs. `-a` prunes every project and sweeps the whole root, including worktrees of repos whose clone was deleted. |
| `bonsai hq` | Browse the current project, or every project with `-a`, in the terminal. Enter, `o`, and Ctrl-I open the selected project. Ctrl-O and Esc go up. `--json` prints the inventory and exits. See [Headquarters](hq.md). |
| `bonsai start [claude\|codex\|opencode]` | Start a new coding session in the current worktree. Choose among installed providers when omitted; a single installed provider is selected automatically. `--prompt` supplies an initial prompt. `-a` picks a worktree from any project first. |
| `bonsai resume [query]` | Fuzzy-search one recent-first list of resumable top-level Claude Code, Codex, and OpenCode sessions across every registered worktree of the current project, then resume the selected harness in its original directory. `-a` searches every project. See [AI coding agents](agents.md#resume-sessions). |
| `bonsai workspace` | Refresh and print the repo's `.code-workspace` file, including registered external worktrees: `code "$(bonsai workspace)"`. `-a` prints the global file. See [Desktop editors](editors.md). |
| `bonsai init <shell>` | Print the shell wrapper (zsh, bash, fish). See [Shell integration](integrations.md#shells). |
| `bonsai agents` | Print a usage contract for AI coding agents, ready for `bonsai agents >> AGENTS.md`. |
| `bonsai skill [install]` | Print the bundled Agent Skill, or install it for detected harnesses (Claude Code, Codex, OpenCode, Cursor, Pi). |
| `bonsai completions <shell>` | Print shell completions. |

Note: a freshly added worktree with no commits counts as merged (same
semantics as `git branch --merged`), so `bonsai clean` will offer to remove
it — the confirmation prompt and `--dry-run` are there for a reason.

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

## External worktrees

Worktrees registered with Git but located outside the configured global
Bonsai root are treated as external. This makes an existing setup from another
worktree manager immediately usable: `list`, `cd`, `resume`, status checks,
and the per-project editor workspace include it. External worktrees are
clearly labelled and remain read-only to Bonsai—`add` will not adopt or move
one, and `remove`/`clean` never delete one. Registered worktrees anywhere under
the global root remain Bonsai-managed even if the main checkout moves or the
repository ID changes. New worktrees still use the current project's directory,
and `bonsai add --path` is limited to that directory.

## Concurrency and safety

Independent work is bounded and parallel by default: `clean` analyzes
worktrees concurrently, `list --status` probes checkouts concurrently,
`resume` scans provider stores and transcripts concurrently, global `cd` and
workspace generation probe checkouts concurrently, `prune` deletes confirmed
independent orphans concurrently, and `add` installs independent ecosystems
together. Output remains deterministic and identifies concurrent contexts with
labels such as `[branch]`, `[npm]`, or `[1/3]`. Operations that mutate shared
Git metadata—worktree removal and branch deletion—remain sequential.

Concurrent mutations in the same repository are guarded by an OS lock; a busy
operation reports that you should retry. Global prune excludes concurrent
mutations throughout the root. Cleanup revalidates worktree identity and branch
tips, and uncertain or unreadable state is preserved. `list --status --json`
uses `dirty: null` when status cannot be read. Cleanup JSON includes completed
removals, deleted branches, skipped worktrees, failures, and a recovery
directory after partial failure; redirected JSON remains a single document.
