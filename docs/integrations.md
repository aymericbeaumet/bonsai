# Integrations

## Git

bonsai respects `checkout.defaultRemote` and `init.defaultBranch`, is
configurable through `git config bonsai.*` (see
[Configuration](configuration.md)), and shells out to your system `git` for
every operation.

### Worktrees created by other tools

Git's worktree registrations are the shared inventory. Bonsai manages any
registered linked checkout in place, whether created by Claude Code, Codex,
Grok CLI, OpenCode, another harness, or `git worktree add`. No path conventions,
import step, or creator allowlist are required. Run from any checkout, or use
`bonsai -C <checkout>`; `list`, `cd`, `workspace`, `hq`, `trim`, `clean`, and
`remove` see the same project registrations. Global discovery still starts
from the Bonsai root, its workspace references, and the current project.
`bonsai add <branch>` also returns an existing linked checkout's path, including
external checkouts, without moving it or rerunning provisioning.

An `external` label means the checkout lives outside the configured Bonsai
root. It has the same lifecycle safeguards: the main checkout is protected,
locked checkouts stay locked, local changes prevent ordinary removal, and
active coding agents block or skip destructive operations. Removal also refuses
a checkout containing another registered worktree. Bonsai does not move these
checkouts or apply new-worktree provisioning to them.

## Shells

`bonsai init zsh|bash|fish` prints a wrapper that adds auto-cd and
completions. The wrapper uses a plain `cd`, so existing directory-change hooks
pick up worktree jumps automatically.

Without the wrapper everything still works — cd-capable commands print the
target path, so `cd "$(bonsai add foo)"` composes.

The wrapper captures directory changes only for `add`, `cd`, `remove`/`rm`,
`clean`, `prune`, and `hq`, and only when stdout is a terminal. `hq` draws its
interface on stderr so it can hand back the checkout you open. Command
substitutions, pipes, and redirections preserve machine output even after
initialization. Other commands stream stdout directly, including `start` and
`resume`. Use `command bonsai` to bypass the wrapper explicitly.

After upgrading Bonsai, an already-running shell may still have an older
wrapper loaded. The binary detects that mismatch and prints the exact
`bonsai init` command to re-evaluate it, or you can restart the shell.

## direnv

`.envrc` files copied by bonsai from your own worktree are `direnv allow`ed
automatically; tracked ones stay gated by direnv as usual.

## Package managers

New worktrees get their dependencies installed automatically for pnpm, npm,
yarn, bun, cargo, and uv, keyed on the lockfile (and `package.json`'s
`packageManager` field). A declared manager must have its matching lockfile.
Installs preserve lockfiles; lifecycle scripts can still generate files. uv
uses `sync --locked`, checking that the manifest and lockfile agree. Before
installing, bonsai inspects supported repository, inherited, and environment
settings and prints an interactive `WARNING` label with a yellow background
(plain text when redirected) plus an official setup link when a faster shared
worktree layout is available. This check still runs with
`[add] install = false`.

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

## Backups, sync, and indexing

The bonsai root holds disposable checkouts and build output, so tools that
walk your home directory should ignore it, the same way they ignore build
directories. Exclude it from Spotlight (System Settings → Spotlight → Search
Privacy) and other desktop indexers, keep `root` outside cloud-synced folders
such as iCloud Drive, Dropbox, or OneDrive, and exclude it from backups such
as Time Machine (`tmutil addexclusion ~/.bonsai`) once your work is pushed
regularly, because uncommitted changes in an excluded worktree are not backed
up.
