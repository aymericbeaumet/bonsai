# Commands

Every subcommand has detailed `--help` with examples; this page is the
overview.

## Scope

Inside a repository, commands act on that project; `-a`/`--all` widens them
to every project under the root. Outside a repository every project is in
scope, and commands that need one project (`add`, `start`) prompt for it.
`-C <dir>` (`--cwd`) runs bonsai as if it was started in `<dir>`, like
`git -C`, and composes with `-a`.

`bonsai --help` groups the commands by task: working with worktrees and
agents, freeing disk or removing worktrees, and setup. Run `bonsai` alone on a
terminal to open [hq](hq.md); without a terminal it prints that help.

The bulk commands `trim`, `clean`, and `prune` share preview and reporting
flags: `-n`/`--dry-run` previews, `-y`/`--yes` (alias `-f`/`--force`)
skips confirmation, and `--json` reports the plan and result. `remove` acts
on chosen targets; its `--force` can discard uncommitted work.

| I want to… | Run |
|---|---|
| start on a branch | `bonsai add <branch>` |
| see what exists, what changed, what weighs what | `bonsai` (hq), or `bonsai list --status` and `bonsai trim -n` |
| see which coding agents run where | `bonsai ps` (`-a` for every project) |
| free disk or delete worktrees | Choose the [cleanup operation](#free-disk-or-remove-worktrees) below |

## Free disk or remove worktrees

A worktree is a checkout directory registered with Git; its branch is a
separate Git reference. Deleting one does not have to delete the other.

| Command | Use when | Deletes | Keeps |
|---|---|---|---|
| `trim [target…]` | Keep a worktree but reclaim its build/dependency space | Ignored files such as `node_modules`, `target`, and caches, subject to keep rules | Checkout, branch, tracked files, non-ignored untracked files, protected local state |
| `remove <target…>` | Stop using chosen worktrees, regardless of merge status | Entire checkout directories, including ignored files | Branches and commits; add `-d` to delete the branches too |
| `clean [target…]` | Finish work already merged into the default branch | Eligible merged or squash-merged checkout directories **and their local branches** | Dirty, locked, busy, and otherwise protected worktrees |
| `prune [target…]` | Repair leftovers, including HQ's `directory deleted` entries | Missing checkout registrations, orphan directories under the Bonsai root, and empty directories | All branches and existing registered checkouts |

`remove` corresponds to `git worktree remove`; `-d` also deletes its local
branch. `trim` has the ignored-only intent of `git clean -X`, with Bonsai's
keep rules and worktree protections. Deleted dependencies and builds need
to be restored before using them again; review `bonsai trim -n` first.

`clean` selects merged branches and removes both their worktrees and branches;
it does not mean `git clean`. `prune` extends `git worktree prune` with
orphan-directory deletion, rather than Git object pruning. Missing registrations
are metadata and are dropped without a CLI prompt; orphan directories can
contain uncommitted files and require confirmation. Preview them with
`bonsai prune -n`. Orphan directories containing nested checkouts are
preserved, including checkouts from other repositories. HQ confirms every
destructive action inside the UI.

## Reference

| Command | Description |
|---|---|
| `bonsai add [branch]` | Preserve an existing branch name, or slugify a new name (preserving `/`), fetch the remote, then create a worktree under the bonsai root and cd into it. New branches start from the latest default branch; remote branches get a local tracking branch. No argument opens a fuzzy prompt (type a new name to create it). If the worktree's directory was deleted by hand, `add` recreates it. `-a` picks the project first. |
| `bonsai cd [query]` | Fuzzy-jump between registered worktrees, including external ones, listed most recently worked-in first with a color-coded last-change age (green = today, yellow = this week, dim = older). `-a` jumps across every project. |
| `bonsai list` (`ls`) | List every Git-registered worktree for the current repo, including external worktrees created by other tools. `-a` lists every project, prefixed by repo id; `--status` adds a dirty marker. |
| `bonsai remove [branch-or-path…]` (`rm`) | Remove worktrees (fuzzy multi-pick without arguments). Exact checkout paths also support detached worktrees, including external ones. Keeps the branch unless `-d`; `--force` discards uncommitted changes and removes a worktree another coding agent is working in, which is otherwise refused. Ignored files are deleted across cores before Git removes the rest. Safe to run from inside the worktree being removed. With `-a`, a branch selects its worktree in every project. |
| `bonsai clean [branch-or-path…]` | Remove worktrees whose branches are merged or squash-merged into the default branch, then delete those branches. A missing upstream alone never proves integration. Fetches `--prune` first (`--no-fetch` to skip), always shows the plan, `-n`/`--dry-run`, `-y`/`--yes` (alias `-f`/`--force`), `--json`. Dirty worktrees and worktrees another coding agent is working in are never touched, and worktrees whose directory was deleted are skipped (drop their registration with `prune`). Named branches or exact checkout paths restrict both preview and execution. `-a` confirms one combined plan for every project. |
| `bonsai trim [branch-or-path…]` | Delete what git ignores (`node_modules`, `target`, `.venv`, build output, caches) from all registered linked worktrees to free disk; worktrees, branches, tracked files, and non-ignored untracked files stay. Kept even when ignored: nested repositories, local state no rebuild restores (`.env*`, `.envrc`, `*.local*`, databases, Terraform state, keys, IDE settings), and `[add] copy` and [`[trim] keep`](configuration.md) globs, at any depth for globs without `/`. Never touches the main checkout, locked worktrees, or worktrees another agent works in. Within ignored dependency/build directories, only explicit `[trim] keep` globs and registered nested worktrees are protected; package fixtures and dependency checkouts are regenerable. Terminal deletion progress shows entries, throughput, and completed paths. The plan shows sizes; `-n` lists every path. `-a` trims every project. |
| `bonsai prune [branches-or-paths…]` | Drop missing, unlocked linked worktree registrations (including external worktrees) without a prompt. Confirm before deleting orphan directories under the Bonsai root, which may contain uncommitted files; remove empty directories too. Branches and existing registered checkouts are preserved. Optional branches or exact checkout paths restrict the cleanup, including missing paths. `-n`/`--dry-run` lists the exact registrations and orphan directories without changing anything; `--json` reports them. `-a` prunes every project and sweeps the whole root, including worktrees of repos whose clone was deleted. |
| `bonsai` / `bonsai hq` | Browse the current project, or every project with `-a`, in the terminal: checkouts, running coding agents, uncommitted changes, upstream state, and disk size, with vim keys. Add, remove, clean, trim, prune, fetch, open an editor or a coding agent, or cd into a checkout without leaving it. `--json` prints the inventory and exits. See [Headquarters](hq.md). |
| `bonsai ps` | List running coding agents (Claude Code, Codex, OpenCode, Cursor Agent, Gemini CLI, Copilot, Amp, Aider, Goose, Crush, Qwen Code, Pi) with the checkout each works in, its pid, and its uptime. `-a` lists every agent on the machine; `--json` for scripts. See [AI coding agents](agents.md#running-agents). |
| `bonsai start [claude\|codex\|opencode]` | Start a new coding session in the current worktree. Choose among installed providers when omitted; a single installed provider is selected automatically. `--prompt` supplies an initial prompt. `-a` picks a worktree from any project first. |
| `bonsai resume [query]` | Fuzzy-search one recent-first list of resumable top-level Claude Code, Codex, and OpenCode sessions across every registered worktree of the current project, then resume the selected harness in its original directory. `-a` searches every project. See [AI coding agents](agents.md#resume-sessions). |
| `bonsai workspace` | Refresh and print the repo's `.code-workspace` file, including registered external worktrees: `code "$(bonsai workspace)"`. `-a` prints the global file with linked worktrees of every known project, including external checkouts. See [Desktop editors](editors.md). |
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

Every Git-registered linked worktree is supported in place, including those
created by Claude Code, Codex, Grok, OpenCode, another harness, or Git itself.
No Bonsai skill or creation step is required. Listing, switching, resuming,
trimming, removal, cleanup, and HQ use the same protections for every checkout.
The `external` label only means a checkout lives outside the Bonsai root.
`bonsai add <branch>` reuses an existing checkout wherever it lives.

New worktrees created through Bonsai use its configured directory and setup
helpers. Existing checkouts are never relocated; `add --path` constrains only
new checkout destinations. Global discovery starts with the current project,
projects under the Bonsai root, and editor workspace references; pass `-C <dir>`
to manage a project that has not been discovered yet.

## Trim output and progress

Trim groups its plan by project, aligns worktree sizes, and puts the largest
ignored paths on separate lines. Ordinary previews fit the terminal width;
`--dry-run` retains complete paths and `--json` includes every entry.
Colors follow `NO_COLOR`. Confirmation shows a selected count instead of
repeating the complete list.

Deletion on a terminal shows a live bar with filesystem entries removed,
entries per second, and completed top-level paths. Entry counts include files,
links, and directories; they avoid a second size-measuring walk while deleting.
Concurrent file changes can change the denominator. HQ streams the same
progress into its output pane. Ordinary redirected output and JSON remain
free of animated progress. To compare deletion strategies on disposable
trees, run [`mise run bench-trim`](development.md).

## Concurrency and safety

Independent work is bounded and parallel by default: `clean` analyzes
worktrees concurrently, `list --status` probes checkouts concurrently,
`resume` scans provider stores and transcripts concurrently, global `cd` and
workspace generation probe checkouts concurrently, `prune` deletes confirmed
independent orphans concurrently, and `add` installs independent ecosystems
together. File trees are measured in parallel; Unix deletion uses directory
descriptors and relative unlinks without a metadata lookup per file. On macOS,
four deletion workers limit APFS contention; other platforms use the default
worker pool. Each pool handles independent directories. Removal deletes a
worktree's ignored files (usually most of it) before Git removes the rest.
Output remains deterministic and identifies concurrent contexts with labels such as
`[branch]`, `[npm]`, or `[1/3]`. Operations that mutate shared Git
metadata—unregistering worktrees and deleting branches—remain sequential.

Coding agents are processes bonsai can see: `remove` refuses, and `clean` and
`trim` skip, a worktree another agent is working in (`bonsai ps` lists
them). An agent that runs bonsai itself never blocks its own request, so it
can clean up after its finished task.

Concurrent mutations in the same repository are guarded by an OS lock; a busy
operation reports that you should retry. Global prune excludes concurrent
mutations throughout the root. Cleanup revalidates worktree identity and branch
tips, and uncertain or unreadable state is preserved. `list --status --json`
uses `dirty: null` when status cannot be read. Cleanup JSON includes completed
removals, deleted branches, skipped worktrees, failures, and a recovery
directory after partial failure; redirected JSON remains a single document.
