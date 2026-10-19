# Headquarters

```sh
bonsai hq          # browse every worktree
bonsai hq --json   # print the same inventory and exit
```

`hq` lists every project under the bonsai root, plus editor workspace folders
and the project you start it from. A checkout with no bonsai worktrees yet
still appears. Each project's main checkout, bonsai worktrees, and external
worktrees are shown together.

## Navigation

The home list is one row per project, in alphabetical order by host, owner,
and repository. `s` cycles between that and most recent. Each row
shows the host, the repository owner, and the name. The branch is not on
that row. A heading names each visible column. An open tree and an open
project use a ruled frame; the home list stays a table. `t`
opens and closes the tree, grouping checkouts by host, then owner, then
repository. Host, owner, and project each have their own colour, in the table
and in the tree.

| Keys | Action |
|---|---|
| Enter, `o`, Ctrl-I | Open the project and list every checkout by branch |
| Ctrl-O, Esc | Go up one step, clearing a search or leaving the project |
| `j`/`k`, arrows, Ctrl-E/Ctrl-Y | Move one row |
| Ctrl-D/Ctrl-U | Move half a page |
| `gg` / `G` | Jump to the top / bottom |
| `/` | Fuzzy-find by host, owner, project, branch, path, stack, or harness |
| `:` | Run add, remove, clean, or prune for the selected project |
| `s` | Cycle between alphabetical and most recent order |
| `t` | Toggle the tree view |
| `r` | Refresh |
| `q` | Quit |

The cursor moves inside the window, and the list scrolls when it comes within
`hq.scrolloff` rows of the top or bottom (default 10). The `/` line and the
`:` command line take the usual readline keys: Ctrl-B/F move, Ctrl-A/E jump,
Ctrl-W deletes a word, Ctrl-U deletes back to the start, and Ctrl-K deletes to
the end. `:` commands use the same confirmation rules as the CLI.

`user:<name>` and `organisation:<name>` (also `org:` and `organization:`)
match the owner. A GitHub owner is classified in the background; until that
is known, or on any other host, either prefix matches the owner text.

## Columns

Each row shows the host and owner on the home list, or the branch inside a
project and in search results, plus age, flags, checkout size, project stack,
dependency-directory sizes, whether those directories are local or shared,
and which of claude, codex, grok, or opencode has a session there. Sizes
appear on the same row once measured. Shared dependencies are symlinks,
hardlinked files, or external stores such as Go modules. Copy-on-write clones
report their full logical size as local. A checkout whose directory is gone
is marked missing and is not passed to git. The checkout path stays on the
status line.

Git status fills in after the list appears. `--json` waits for status and
sizes and includes `footprint` on each worktree. Warnings are appended to
`~/.local/state/bonsai/hq.log`.

`start` and `resume` stay separate commands. Run them in a worktree.
