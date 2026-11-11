# Your first worktree

A worktree gives another branch its own directory. Keep your current editor,
uncommitted changes, and running task where they are while you work on a fix
in a second checkout. Bonsai manages the directories and setup around Git.

## Look around first

[Install Bonsai and enable shell integration](../readme.md#install), then run
these commands from a repository you already use:

```sh
bonsai list --status
bonsai
```

The first command lists the main checkout and every linked worktree registered
with Git. The second opens the terminal dashboard; press `?` for help and `q`
to quit. Existing worktrees need no migration or import. From another
directory, use `bonsai -C ~/src/my-repo list --status`.

## Start a task

```sh
bonsai add fix-parser
```

For a new branch, Bonsai fetches the remote and branches from the latest
default branch, creates a checkout under `~/.bonsai/`, copies matching
untracked local config such as `.env`, and installs dependencies using
supported lockfiles. The shell integration changes into that directory.
Without it, use `cd "$(bonsai add fix-parser)"`.

An existing local or remote branch keeps its history. If it already has a
worktree, `add` returns that checkout without moving it or rerunning setup.
To start a new branch from your current commit instead of the default branch:

```sh
bonsai add parser-followup --base HEAD
```

Your uncommitted source edits stay in the original checkout. Git commits,
branches, and remotes are shared; each worktree has its own files and index.
A branch normally checks out in only one worktree at a time.

Use your usual editor, tests, commits, and pull requests. `bonsai cd` opens a
fuzzy picker to switch checkouts; `bonsai cd -a` searches across known projects.
For editor workspaces and coding sessions, see [editors](editors.md) and
[agents](agents.md).

## Finish a task

After merging the pull request, preview cleanup for that branch:

```sh
bonsai clean fix-parser --dry-run
bonsai clean fix-parser
```

`clean` fetches and checks integration, including squash-merges, then asks
before deleting the eligible checkout and its local branch. It skips dirty,
locked, and busy worktrees. A new branch with no commits also counts as
merged, so inspect the plan. If you are inside the removed checkout, the
shell integration returns you to a surviving directory.

To reclaim build and dependency space while keeping a checkout:

```sh
bonsai trim parser-followup --dry-run
bonsai trim parser-followup
```

Trim keeps tracked files and protected local state; dependencies and builds
it removes must be restored before use. See the [cleanup comparison](commands.md#free-disk-or-remove-worktrees)
for `remove` and `prune`, and [keep rules](configuration.md) for data you want
to preserve even inside build directories.

## Common questions

**Do I need a coding agent?** No. All worktree commands work independently.
The optional skill and session commands help when you use coding agents.

**Does this isolate running apps?** Worktrees separate files, not processes,
ports, databases, or credentials. Configure separate ports or data stores
when running multiple copies of an application. Matching local `.env` files
are copied by default; adjust `[add] copy` if each task needs different values.

**What if dependency setup fails?** The checkout remains available and Bonsai
prints the failing setup command to retry there. Repeating `add` reuses the
checkout without rerunning installation. See [package-manager setup](integrations.md#package-managers)
for supported managers and a mise hook with locked tools.

**Can I use it offline?** Set `BONSAI_ADD__FETCH=false` for an `add` command,
or `[add] fetch = false` in configuration. New branches then use locally
available refs. Dependency installation may still need network access;
`BONSAI_ADD__INSTALL=false` disables that step. Configured hooks can also
access the network.

**What if I stop using Bonsai?** Your branches, commits, and Git-registered
worktrees remain usable by Git and your editor. Remove the shell integration
and uninstall the binary when you no longer need it. Keep checkout directories
until their work is committed or deliberately discarded.
