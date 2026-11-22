# Desktop editors

The bonsai root is structured so GUI tools get worktrees for free — no
extension required.

## VS Code and derivatives

Native, via the multi-root workspace standard (VS Code, Cursor, Windsurf,
and every other VS Code derivative): bonsai maintains two
`.code-workspace` files, kept in sync by add/remove/clean/prune:

- `~/.bonsai/<repo-id>/<repo>.code-workspace` — this repo's main checkout
  plus every Git-registered worktree, including external ones
- `~/.bonsai/bonsai.code-workspace` — every linked worktree of every known
  repo, labelled `repo · branch`, including external checkouts created by
  other tools. Projects are discovered from the Bonsai root, workspace folder
  references, and the current checkout; existing orphan directories under the
  root stay listed too.

Open one and each worktree appears as a root folder in the Explorer's left
tree, with per-root git status in the Source Control panel. Editors watch
the file, so the tree updates live as bonsai creates and removes worktrees:

```sh
code "$(bonsai workspace)"           # this repo's worktrees
cursor "$(bonsai workspace -a)"      # linked worktrees of every known project
```

Disable the file maintenance with `workspace = false`.

Refreshes preserve workspace settings, tasks, launch configuration, comments,
and custom folders. Updates use locking and atomic replacement, and unchanged
files are not rewritten. Customized files are retained after the last worktree
is removed.

## Folder-based apps

Claude Code desktop, Codex desktop, and other folder-based apps open worktrees
as plain directories. Bonsai creates them under
`~/.bonsai/<host>/<owner>/<repo>/`, named after their branch; checkouts created
elsewhere keep their existing paths. Jump to either from a terminal with
`bonsai cd`.
