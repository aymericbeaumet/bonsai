# Desktop editors

The bonsai root is structured so GUI tools get worktrees for free — no
extension required.

## VS Code and derivatives

Native, via the multi-root workspace standard (VS Code, Cursor, Windsurf,
and every other VS Code derivative): bonsai maintains two
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

## Folder-based apps

Claude Code desktop, Codex desktop, and other folder-based apps: every
worktree is a plain directory named after its branch under
`~/.bonsai/<host>/<owner>/<repo>/`, so folder pickers and recent-project
lists stay readable. Jump from a terminal with `bonsai cd`.
