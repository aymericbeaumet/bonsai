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

Run `bonsai hq` to browse every project in the terminal and open the one
you pick.

## Install

With [mise](https://mise.jdx.dev) (prebuilt binaries for Linux, macOS, and
Windows — amd64 and arm64):

```sh
mise use -g github:aymericbeaumet/bonsai
```

Or with Cargo:

```sh
cargo install --locked bonsai-cli
```

From a clone, `cargo install --path . --locked` builds the same `bonsai`
binary. The `bonsai-cli` package provides that binary. The `bonsai-lib`
package is the Rust library behind it.

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

Inside a repository, commands act on that project; `-a`/`--all` widens them
to every project under the root. Outside a repository every project is in
scope, and commands that need one project (`add`, `start`) prompt for it.
`-C <dir>` (`--cwd`) runs bonsai as if it was started in `<dir>`, like
`git -C`, and composes with `-a`.

| Command | Description |
|---|---|
| `bonsai hq` | Browse the current project, or every project with `-a`, in the terminal. Enter, `o`, and Ctrl-I open the selected project. Ctrl-O and Esc go up. `--json` prints the inventory and exits. |
| `bonsai start [claude\|codex\|opencode]` | Start a new coding session in the current worktree. Choose among installed providers when omitted; a single installed provider is selected automatically. `--prompt` supplies an initial prompt. `-a` picks a worktree from any project first. |
| `bonsai add [branch]` | Preserve an existing branch name, or slugify a new name (preserving `/`), fetch the remote, then create a worktree under the bonsai root and cd into it. New branches start from the latest default branch; remote branches get a local tracking branch. No argument opens a fuzzy prompt (type a new name to create it). `-a` picks the project first. |
| `bonsai list` (`ls`) | List every Git-registered worktree for the current repo, including external worktrees created by other tools. `-a` lists every project, prefixed by repo id; `--status` adds a dirty marker. |
| `bonsai cd [query]` | Fuzzy-jump between registered worktrees, including external ones, listed most recently worked-in first with a color-coded last-change age (green = today, yellow = this week, dim = older). `-a` jumps across every project. |
| `bonsai resume [query]` | Fuzzy-search one recent-first list of resumable top-level Claude Code, Codex, and OpenCode sessions across every registered worktree of the current project, then resume the selected harness in its original directory. `-a` searches every project. |
| `bonsai workspace` | Refresh and print the repo's `.code-workspace` file, including registered external worktrees: `code "$(bonsai workspace)"`. `-a` prints the global file. |
| `bonsai remove [branch-or-path…]` (`rm`) | Remove worktrees (fuzzy multi-pick without arguments). Exact managed paths also support detached worktrees. Keeps the branch unless `-d`; `--force` discards uncommitted changes. Safe to run from inside the worktree being removed. With `-a`, a branch selects its worktree in every project. |
| `bonsai clean` | Remove worktrees whose branches are merged or squash-merged into the default branch, then delete those branches. A missing upstream alone never proves integration. Fetches `--prune` first (`--no-fetch` to skip), always shows the plan, `-n`/`--dry-run`, `-y`/`--yes` (alias `-f`/`--force`). Dirty worktrees are never touched. `-a` confirms one combined plan for every project. |
| `bonsai prune` | Clean up stale worktree registrations, orphaned directories, and empty dirs. `-a` prunes every project and sweeps the whole root, including worktrees of repos whose clone was deleted. |
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
clone_cargo_target = true   # copy-on-write clone Cargo's target/ into new worktrees
post_add = "mise install"   # runs after automatic dependency installation
# untracked files copied into new worktrees; setting this replaces the
# defaults: [".env", ".env.*", ".envrc", ".mcp.json", "CLAUDE.local.md",
#           ".claude/settings.local.json", ".cursor/mcp.json"]
copy = [".env*", ".envrc"]

[clean]
fetch = true                # fetch --prune before computing merged branches
protected = ["release/*"]   # branch globs never cleaned

[hq]
scrolloff = 10              # rows kept above and below the cursor, as in vim
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
bonsai hq          # browse every worktree
bonsai hq --json   # print the same inventory and exit
```

`hq` lists every project under the bonsai root, plus editor workspace folders
and the project you start it from. A checkout with no bonsai worktrees yet
still appears. Each project's main checkout, bonsai worktrees, and external
worktrees are shown together.

The home list is one row per project, in alphabetical order by host, owner,
and repository. `s` cycles between that and most recent. Each row
shows the host, the repository owner, and the name. The branch is not on
that row. A heading names each visible column. An open tree and an open
project use a ruled frame; the home list stays a table. `t`
opens and closes the tree, grouping checkouts by host, then owner, then
repository. Host, owner, and project each have their own colour, in the table and in the tree. Enter,
`o`, and Ctrl-I open the project and list every checkout by branch. Ctrl-O
and Esc go up one step, clearing a search or leaving the project. `j`/`k`
or the arrow keys move one row, as do Ctrl-E and Ctrl-Y. Ctrl-D and Ctrl-U
move half a page. `gg` jumps to the top and `G` to the bottom. The cursor
moves inside the window, and the list scrolls when it comes within
`hq.scrolloff` rows of the top or bottom (default 10). `/` fuzzy-finds by host, owner, project, branch, path,
stack, or harness and lists every match. The `/` line and the `:` command line take the usual
readline keys: Ctrl-B/F move, Ctrl-A/E jump, Ctrl-W deletes a word, Ctrl-U
deletes back to the start, and Ctrl-K deletes to the end. `user:<name>` and
`organisation:<name>` (also `org:` and
`organization:`) match the owner. A GitHub owner is classified in the
background; until that is known, or on any other host, either prefix matches
the owner text. `:` runs add, remove, clean, and prune for the selected
project, with the same confirmation rules as the CLI. `r` refreshes. `q`
quits. The checkout path stays on the status line.

Each row shows the host and owner on the home list, or the branch inside a project
and in search results, plus age, flags, checkout size, project stack,
dependency-directory sizes, whether those directories are local or shared,
and which of claude, codex, grok, or opencode has a session there. Sizes
appear on the same row once measured. Shared dependencies are symlinks,
hardlinked files, or external stores such as Go modules. Copy-on-write clones
report their full logical size as local. A checkout whose directory is gone
is marked missing and is not passed to git. Warnings are appended to
`~/.local/state/bonsai/hq.log`. Git status fills in after the list appears.
`--json` waits for status and sizes and includes `footprint` on each worktree.

`start` and `resume` stay separate commands. Run them in a worktree.

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
cursor "$(bonsai workspace -a)"      # everything under the bonsai root
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
  | Cargo | Downloads are shared through `CARGO_HOME`. Build output is per checkout, so `add` copy-on-write clones `target/` from your current worktree (then the main one). See [Cargo build output](#cargo-build-output). |
  | npm, Yarn Classic, uv | Their default global download/source caches already give Bonsai's frozen install commands the appropriate sharing. uv warns if its cache is explicitly disabled or placed inside the worktree. |

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
- **backups, sync, and indexing**: the bonsai root holds disposable checkouts
  and build output, so tools that walk your home directory should ignore it,
  the same way they ignore build directories. Exclude it from Spotlight
  (System Settings → Spotlight → Search Privacy) and other desktop indexers,
  keep `root` outside cloud-synced folders such as iCloud Drive, Dropbox, or
  OneDrive, and exclude it from backups such as Time Machine
  (`tmutil addexclusion ~/.bonsai`) once your work is pushed regularly,
  because uncommitted changes in an excluded worktree are not backed up.

### Cargo build output

Cargo shares downloaded sources in `CARGO_HOME`, but compiles into a
`target/` directory inside each checkout, so every worktree would otherwise
rebuild and store every dependency again. When a new worktree has a
`Cargo.toml`, `bonsai add` asks Cargo where its target and build directories
are and clones them from your current worktree, then the main checkout, with
copy-on-write (`clonefile` on APFS, `FICLONE` on Btrfs, XFS, and other
reflink-capable Linux filesystems). The clone takes seconds even for
multi-gigabyte targets, shares disk blocks until either side rebuilds them,
and leaves only the workspace's own crates to recompile. Tools such as `du`
count shared blocks in every clone; free space reflects the real usage.

Bonsai never falls back to a full copy: on filesystems without copy-on-write,
across volumes, and on Windows, it reports that `target/` was not seeded and
Cargo builds as usual. Target directories outside the worktree, such as a
shared `CARGO_TARGET_DIR` or `build.target-dir`, are left alone. Disable
cloning with `[add] clone_cargo_target = false`. Removing a worktree with
`remove` or `clean` frees the blocks only it was using.

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

## Use as a Rust library

Worktree discovery and the commands behind the CLI are packaged as
`bonsai-lib`, which the CLI builds on and which is imported as `bonsai`.
For the 0.1.1 release, add:

```toml
[dependencies]
bonsai = { package = "bonsai-lib", version = "0.1" }
```

```rust
use bonsai::repo::repo_id_from_url;

let id = repo_id_from_url("https://github.com/aymericbeaumet/bonsai.git");
assert_eq!(id.as_deref(), Some("github.com/aymericbeaumet/bonsai"));
```

API documentation is available on [docs.rs](https://docs.rs/bonsai-lib).
In a clone, run `cargo doc --open -p bonsai-lib`.

## Development checks

Install [mise](https://mise.jdx.dev), then run `mise trust .mise.toml` and
`mise install --locked rust node python uv` in the clone. `.mise.toml` and
`mise.lock` pin Rust, Node.js, Python, and uv for local development and CI.
Use `mise exec --` for commands in shells without mise activation. The Linux
CI shell tests also need Bash, Zsh, and Fish from the OS.

```sh
mise exec -- cargo test --locked
mise exec -- cargo clippy --locked --all-targets -- -D warnings
mise exec -- cargo fmt --check
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

## License

[MIT](./LICENSE)
