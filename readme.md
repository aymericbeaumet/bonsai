# bonsai [![release](https://github.com/aymericbeaumet/bonsai/actions/workflows/release.yml/badge.svg)](https://github.com/aymericbeaumet/bonsai/actions/workflows/release.yml)

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

Run `bonsai hq` for terminal and browser interfaces to all your projects:
fuzzy navigation, a worktree constellation, Bonsai commands, and shared
interactive terminals.

## Install

With [mise](https://mise.jdx.dev) (prebuilt binaries for Linux, macOS, and
Windows — amd64 and arm64):

```sh
mise use -g github:aymericbeaumet/bonsai
```

Or build from source:

```sh
cargo install --git https://github.com/aymericbeaumet/bonsai bonsai-cli
```

Then enable the shell integration (auto-cd + completions) in your rc file:

```sh
# zsh — ~/.zshrc
eval "$(bonsai init zsh)"

# bash — ~/.bashrc
eval "$(bonsai init bash)"

# fish — ~/.config/fish/config.fish
bonsai init fish | source
```

Without the wrapper everything still works — cd-capable commands print the
target path, so `cd "$(bonsai add foo)"` composes.

The wrapper captures directory changes only for `add`, `cd`, `remove`/`rm`,
`clean`, and `prune`, and only when stdout is a terminal. Command substitutions,
pipes, and redirections preserve machine output even after initialization.
Other commands stream stdout directly, including `hq`, `start`, and `resume`.
Use `command bonsai` to bypass the wrapper explicitly.

After upgrading Bonsai, an already-running shell may still have an older
wrapper loaded. The binary detects that mismatch and prints the exact
`bonsai init` command to re-evaluate it, or you can restart the shell.

## Commands

| Command | Description |
|---|---|
| `bonsai hq` | Run persistent headquarters with a TUI and browser workspace sharing worktrees and terminal sessions. `--port` selects the web port (default `47831`, `0` chooses an available port); `--no-open` skips opening the browser; `--no-tui` runs headlessly. |
| `bonsai start [claude\|codex\|opencode]` | Start a new coding session in the current worktree. Choose among installed providers when omitted; a single installed provider is selected automatically. `--prompt` supplies an initial prompt. |
| `bonsai add [branch]` | Preserve an existing branch name, or slugify a new name (preserving `/`), fetch the remote, then create a worktree under the bonsai root and cd into it. New branches start from the latest default branch; remote branches get a local tracking branch. No argument opens a fuzzy prompt (type a new name to create it). |
| `bonsai list` (`ls`) | List every Git-registered worktree for the current repo, including external worktrees created by other tools. `--all` lists every Bonsai-managed worktree across projects; `--status` adds a dirty marker. |
| `bonsai cd [query]` | Fuzzy-jump between registered worktrees, including external ones, listed most recently worked-in first with a color-coded last-change age (green = today, yellow = this week, dim = older). Works globally across Bonsai-managed worktrees when run outside a repo. |
| `bonsai resume [query]` | Fuzzy-search one recent-first list of resumable top-level Claude Code, Codex, and OpenCode sessions across every registered worktree of the current project, then resume the selected harness in its original directory. |
| `bonsai workspace` | Refresh and print the repo's `.code-workspace` file, including registered external worktrees: `code "$(bonsai workspace)"`. |
| `bonsai remove [branch-or-path…]` (`rm`) | Remove worktrees (fuzzy multi-pick without arguments). Exact managed paths also support detached worktrees. Keeps the branch unless `-d`; `--force` discards uncommitted changes. Safe to run from inside the worktree being removed. |
| `bonsai clean` | Remove worktrees whose branches are merged or squash-merged into the default branch, then delete those branches. A missing upstream alone never proves integration. Fetches `--prune` first (`--no-fetch` to skip), always shows the plan, `-n`/`--dry-run`, `-y`/`--yes` (alias `-f`/`--force`). Dirty worktrees are never touched. |
| `bonsai prune` | Clean up stale worktree registrations, orphaned directories, and empty dirs. `--all` sweeps the whole root, including worktrees of repos whose clone was deleted. |
| `bonsai init <shell>` | Print the shell wrapper (zsh, bash, fish). |
| `bonsai agents` | Print a usage contract for AI coding agents, ready for `bonsai agents >> AGENTS.md`. |
| `bonsai skill [install]` | Print the bundled Agent Skill, or install it for detected harnesses (Claude Code, Codex, OpenCode, Cursor, Pi). |
| `bonsai completions <shell>` | Print shell completions. |

Every subcommand has detailed `--help` with examples.

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
removals, deleted branches, failures, and a recovery directory after partial
failure; redirected JSON remains a single document.

Note: a freshly added worktree with no commits counts as merged (same
semantics as `git branch --merged`), so `bonsai clean` will offer to remove
it — the confirmation prompt and `--dry-run` are there for a reason.

## Configuration

For each setting, the first value found in this list wins:

1. CLI flags, such as `--root` and `--remote`.
2. `BONSAI_*` environment variables. Use `__` for nested settings, for
   example `BONSAI_CLEAN__FETCH=false`.
3. `git config bonsai.*`. Repository-local values override global Git values.
4. The project's `.bonsai.toml`.
5. `$XDG_CONFIG_HOME/bonsai/config.toml`, or
   `~/.config/bonsai/config.toml` when `XDG_CONFIG_HOME` is unset.
6. Built-in defaults.

Use `.bonsai.toml` for configuration the team should share. Bonsai first
looks in the worktree you are currently using, so a branch can test a config
change before it is merged. If that worktree has no `.bonsai.toml`, Bonsai
uses the file from the main checkout.

Use `git config bonsai.*` for personal or per-clone overrides without editing
the shared file:

```sh
git config bonsai.root ~/src/worktrees        # this clone only
git config --global bonsai.clean.fetch false  # all of your clones
git config --add bonsai.add.copy ".env"       # repeat for list values
```

The complete TOML shape, with its defaults and common overrides, is:

```toml
root = "~/.bonsai"          # where worktrees live
remote = "origin"           # optional: defaults to checkout.defaultRemote, then origin
default_branch = "main"     # optional: skip detection
workspace = true            # maintain a .code-workspace file per repo

[add]
fetch = true                # fetch --prune before creating (set false for offline use)
install = true              # auto-install deps (pnpm/npm/yarn/bun/cargo/uv)
post_add = "mise install"   # runs after automatic dependency installation
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

Worktrees registered with Git but located outside the configured global
Bonsai root are treated as external. This makes an existing setup from another
worktree manager immediately usable: `list`, `cd`, `resume`, status checks,
and the per-project editor workspace include it. External worktrees are
clearly labelled and remain read-only to Bonsai—`add` will not adopt or move
one, and `remove`/`clean` never delete one. Registered worktrees anywhere under
the global root remain Bonsai-managed even if the main checkout moves or the
repository ID changes. New worktrees still use the current project's directory,
and `bonsai add --path` is limited to that directory.

## AI agents (Claude Code, Cursor, Codex, OpenCode, ...)

bonsai is built to work the same across coding harnesses, so switching tools
mid-project costs nothing. It sticks to the two cross-harness standards —
no per-harness plugins to install or maintain.

**The [Agent Skill](https://agentskills.io)** (`skills/bonsai/SKILL.md`)
teaches agents the workflow, its invariants, and how to complete authorized
operations while preserving cleanup scope and uncommitted work. It is
embedded in the binary and installs into each harness's
standard skill directory:

```sh
bonsai skill install        # detects Claude Code, Codex, OpenCode, Cursor, Pi
bonsai skill install --all  # or install for every harness unconditionally
bonsai skill                # or print it and pipe it wherever you want
```

**Or the AGENTS.md standard**: `bonsai agents >> AGENTS.md` appends the same
skill body without YAML frontmatter to the cross-harness instructions file.
Both commands use `skills/bonsai/SKILL.md` embedded at build time, so their
guidance stays in sync. After upgrading bonsai, rerun `bonsai skill install`
to refresh installed copies, or replace the existing bonsai section in
`AGENTS.md` with fresh `bonsai agents` output.

**Everything is scriptable**: `path=$(bonsai add feat-x)` prints the worktree
path and is idempotent; `bonsai list --json` and `bonsai clean --dry-run
--json` give structured output for a reliable inspect-then-execute loop;
`clean --yes` / `prune --yes` (alias `-f`/`--force`) never prompt; fuzzy pickers only ever appear on
a real terminal.

**New worktrees come pre-provisioned**: untracked local config — `.env*`,
`.envrc`, `.mcp.json`, `CLAUDE.local.md`, `.claude/settings.local.json`,
`.cursor/mcp.json` — is copied over by default, and dependencies are
installed automatically from the lockfile, so whichever harness (or human)
created the worktree, the others find their setup in place.

## Resume AI sessions

Run `bonsai resume` from any checkout or registered worktree of a project to
search its resumable top-level Claude Code, Codex, and OpenCode history in one
picker. The UI shares `bonsai cd`'s keyboard navigation, type-to-filter
behavior, relative ages, freshness colors, and recent-first ordering. Search
runs asynchronously as you type; arrow keys and Ctrl-P/N navigate, while
standard editing keys such as Ctrl-W/A/E/U edit the filter. Session titles,
providers, IDs, branches, and directories are all searchable. Both pickers
render inline below the current shell prompt and clear themselves on exit.

```sh
bonsai resume                 # newest session is selected by default
bonsai resume parser         # unique matches open; otherwise pre-filter
bonsai resume <session-id>    # exact IDs resume immediately
```

The selected harness starts in the session's original directory. If an old
worktree has been removed, bonsai warns and starts it from the main checkout.
Standard location overrides are respected: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`CODEX_SQLITE_HOME`, `XDG_DATA_HOME`, and `OPENCODE_DB`.

## Headquarters

```sh
bonsai hq                              # TUI and browser, shared sessions
bonsai hq --no-open --port 47831       # TUI with web server on a chosen port
bonsai hq --no-tui --no-open --port 0  # headless server, print private link
```

`hq` is a persistent process running both interfaces. `start` opens a new coding session in
the current worktree, and `resume` finds and restores a past coding session.
Both session commands are available from headquarters.

The TUI specializes in keyboard navigation; the browser specializes in
visual exploration. They share inventory and terminal sessions, so a shell
started in either can be attached from the other. HQ enables the TUI when
stdin and stdout are terminals; redirected or service use stays headless.
Use Ctrl+] to detach an attached terminal back to the TUI without ending
the session.

The graph groups every discovered project with its main checkout and all
Git-registered worktrees, including external, locked, detached, and stale
worktrees. Discovery scans the configured Bonsai root and its editor
workspace files; the project you start from is also included, even before
its first `bonsai add`. Git status refreshes automatically.

Bonsai tracks worktrees independently of terminal tools. Each checkout shows
its HQ terminal and tmux activity, including panes in inactive tmux windows
and worktree subdirectories. Select a checkout to see and attach its sessions;
worktrees with nothing running stay visible. Tmux is optional and keeps its
normal shell configuration.

Use the command palette to fuzzy-find projects and branches, switch between
the graph and list, and select a checkout to inspect its status or open a
terminal. Worktree forms cover common operations; the command runner exposes
the CLI arguments and runs commands interactively, including cleanup
confirmations and `bonsai resume`'s session picker. External worktrees retain
the CLI's protection against removal and adoption.

Terminal tabs are real pseudoterminals: use your shell, terminal editor,
Git tools, or coding assistants with normal input, colors, resizing, and
interactive prompts. Tabs and recent output survive browser reconnection
while the server runs. With tmux installed, create a persistent session for
a checkout or attach an existing session. Closing its browser terminal
detaches the tmux client; the tmux session continues running.

The server binds to `127.0.0.1` (port `47831` by default). Its private launch
link grants access to the local shell; use that link to connect another
browser tab. Quitting HQ stops Bonsai and its ordinary terminals. Tmux sessions
survive a server restart. Browser assets are embedded in the binary, so
Node.js and an internet connection are unnecessary at runtime.

See [browser architecture and development](docs/browser-workspace.md) for
the API, terminal lifecycle, and frontend build commands.

## Desktop editors

The bonsai root is structured so GUI tools get worktrees for free — no
extension required.

**Native, via the multi-root workspace standard** (VS Code, Cursor,
Windsurf, and every other VS Code derivative): bonsai maintains two
`.code-workspace` files, kept in sync by add/remove/clean/prune:

- `~/.bonsai/<repo-id>/<repo>.code-workspace` — this repo's main checkout
  plus every Git-registered worktree, including external ones
- `~/.bonsai/bonsai.code-workspace` — every worktree of every repo,
  labelled `repo · branch` (Bonsai-managed paths only, because this global
  view has no current repository from which to discover external worktrees)

Open one and each worktree appears as a root folder in the Explorer's left
tree, with per-root git status in the Source Control panel. Editors watch
the file, so the tree updates live as bonsai creates and removes worktrees:

```sh
code "$(bonsai workspace)"           # this repo's worktrees
cursor "$(bonsai workspace --all)"   # everything under the bonsai root
```

Disable the file maintenance with `workspace = false`.

Refreshes preserve workspace settings, tasks, launch configuration, comments,
and custom folders. Updates use locking and atomic replacement, and unchanged
files are not rewritten. Customized files are retained after the last worktree
is removed.

**Folder-based apps** (Claude Code desktop, Codex desktop): every worktree
is a plain directory named after its branch under
`~/.bonsai/<host>/<owner>/<repo>/`, so folder pickers and recent-project
lists stay readable. Jump from a terminal with `bonsai cd`.

## Integrations

- **git**: respects `checkout.defaultRemote` and `init.defaultBranch`;
  configurable through `git config bonsai.*`; all operations shell out to
  your system `git`.
- **zsh/bash/fish**: wrapper + completions via `bonsai init`. The wrapper
  uses a plain `cd`, so existing directory-change hooks pick up worktree
  jumps automatically. Commands outside the auto-cd allowlist, including
  `hq`, `start`, and `resume`, inherit stdout directly.
- **direnv**: `.envrc` files copied by bonsai from your own worktree are
  `direnv allow`ed automatically; tracked ones stay gated by direnv as usual.
- **package managers** (pnpm, npm, yarn, bun, cargo, uv): new worktrees get
  their dependencies installed automatically, keyed on the lockfile (and
  `package.json`'s `packageManager` field). A declared manager must have its
  matching lockfile. Installs preserve lockfiles; lifecycle scripts can still
  generate files. uv uses `sync --locked`, checking that the manifest and
  lockfile agree. Before installing, bonsai inspects supported repository,
  inherited, and environment settings and prints an interactive
  `WARNING` label with a yellow background (plain text when redirected) plus
  an official setup link when a faster shared worktree layout is available.
  This check still runs with `[add] install = false`.

  | Manager | Worktree-friendly behavior |
  |---|---|
  | pnpm | Warns until `virtualStoreType: global` (or the legacy `enableGlobalVirtualStore: true`) is set in `pnpm-workspace.yaml`; see [pnpm's worktree guide](https://pnpm.io/git-worktrees). |
  | Bun | Warns until `[install]` uses `linker = "isolated"` and `globalStore = true`; see [Bun's global virtual store](https://bun.sh/docs/pm/global-store). |
  | Yarn Berry | Yarn 4's global cache + PnP defaults are optimized; Yarn 2/3 needs `enableGlobalCache: true`, and `node-modules` needs `nmMode: hardlinks-global`. See [Yarn settings](https://yarnpkg.com/configuration/yarnrc/). |
  | npm, Yarn Classic, Cargo, uv | Their default global download/source caches already give Bonsai's frozen install commands the appropriate sharing. uv warns if its cache is explicitly disabled or placed inside the worktree. |

  Missing tools and failed installs produce an incomplete-setup summary with
  the command to retry from the new worktree. Installer and hook output streams
  on stderr, labelled by tool; cancellation keeps the worktree. Re-adding an
  existing worktree returns its path without rerunning setup.

  Tool selection uses PATH and its installed shims; `packageManager` selects
  the manager, while version enforcement belongs to tools such as Corepack.
  If your project needs mise activation before installation, use the existing
  hook to control the sequence:

  ```toml
  [add]
  install = false
  post_add = "mise install && mise exec -- pnpm install --frozen-lockfile"
  ```

  Storage warnings are suggestions based on recognized settings, not a complete
  package-manager configuration evaluator. Bonsai does not change linker or
  toolchain configuration.

## How it works

- Worktrees live at `<root>/<repo-id>/<branch>`, where the repo-id is derived
  from the remote URL (`github.com/owner/repo`, nested groups preserved) or a
  hash of the repo path when there is no remote. Inputs are slugified per `/`
  segment, and the resulting branch maps to nested directories (`Fix API/Login`
  → branch and directory `fix-api/login`).
- Git-registered worktrees elsewhere under the configured global root remain
  managed, so moving the main checkout or changing its repo ID does not orphan
  them. Registered worktrees outside that root are external and merged into the
  project's read-only views; Bonsai never creates there.
- For interactive cd-capable commands, the shell wrapper watches for a sentinel
  line to cd. Other commands and captured output pass through directly. Prompts
  render on stderr, so fuzzy pickers work even inside `$(...)`.

## Development checks

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
cargo build --release --locked
python3 tests/terminal.py target/release/bonsai
cargo test --locked --test cli real_package_managers_ -- --ignored
python3 scripts/benchmark.py --worktrees 1 10 100 --sessions 1000 10000 100000
```

The PTY checks require Unix. Package-manager smoke tests require npm, uv, and
Python. CI installs Bash/Zsh/Fish coverage explicitly on Linux. Benchmarks use
disposable repositories and synthetic session stores, report first-run and warm
median/p95 latency plus Git process counts, and never access personal sessions.

## License

[MIT](./LICENSE)
