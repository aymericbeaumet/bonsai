# Headquarters

```sh
bonsai hq          # this project, or every project outside a repository
bonsai hq -a       # every project under the bonsai root
bonsai hq --json   # print the same inventory and exit
```

`hq` lists every project under the bonsai root, plus editor workspace folders
and the project you start it from. A checkout with no bonsai worktrees yet
still appears. Each project's main checkout, bonsai worktrees, and external
worktrees are shown together, and you can create, remove, clean, and open
them without leaving the list. Press `?` inside hq for every key.

## Views

The **home list** has one row per project, in alphabetical order by host,
owner, and repository:

| Column | Shows |
|---|---|
| host, owner, project | Where the repository lives, parsed from its remote |
| checkouts | The main checkout plus every worktree |
| age | Time since the most recent activity in any checkout |
| changes | How many checkouts have uncommitted changes |
| size | Disk used by all checkouts together |
| stack | Languages detected from project files |
| agents | Coding agents with sessions in any checkout |
| notes | Checkouts whose directory was deleted by hand, or `unavailable` |

**Open a project** (Enter) to list each checkout:

| Column | Shows |
|---|---|
| branch | The checked-out branch; the main checkout is cyan |
| age | Time since the checkout was last worked in |
| changes | `clean`, `2 modified, 1 untracked`, `directory deleted`, … |
| upstream | `up to date`, `3 ahead`, `2 behind`, `no upstream`, `upstream gone`, … |
| size | Disk used by the checkout, including its dependencies |
| deps | Dependency and build directories, such as `target 31 GB` or `node_modules 1 GB shared` |
| agents | Coding agents with sessions in this checkout |
| notes | `main checkout`, `external`, `locked`, `prunable` |

The header above the list names the remote, the main checkout, and the
project's totals, and suggests `P` when checkouts were deleted by hand.
`upstream gone` usually means the pull request was merged and its branch
deleted; `C` cleans those.

`t` shows the **tree**: host, then owner, then repository, then checkouts.
Every group sums what it contains. A **search** (`/`) lists matching
checkouts from every project. Columns that have nothing to show are hidden,
and on a narrow terminal the least useful ones go first.

The status line describes the selection in full: its path, how it relates to
its upstream, and what its size is made of.

## Sizes

Sizes are measured in the background, once the list is on screen, and stream
in while large directories are walked. The last measurement of each checkout
is remembered in `~/.local/state/bonsai/footprints.json`, so the next `hq`
shows every total at once and updates it as checkouts are measured again; a
size that is still being measured is dimmed. After a command, only the
affected project is measured again; `r` measures everything.

A checkout's size is the logical size of its files. Shared dependencies are
symlinks, hardlinked files, or external stores such as Go modules; their
store is reported once, not added to every checkout. Copy-on-write clones
report their full logical size as local, so the total can exceed the disk
space actually used.

## Keys

The motion keys work the same in every view: the home list, an open project,
the tree, search results, and also the `?` help, confirmation dialogs, and
the full-screen output (`O`), which they scroll like a pager.

| Keys | Action |
|---|---|
| `j` `k`, arrows, Ctrl-N/P | Move one row; a count repeats (`5j`) |
| `gg` `G`, Home End | First / last row; `3G` goes to the third |
| Ctrl-D/U, Ctrl-F/B, Page Down/Up | Half a page / a page down and up |
| Ctrl-E/Y | Scroll, keeping the cursor where it is |
| `H` `M` `L` | Top / middle / bottom of the screen |
| `zz` `zt` `zb` | Put the cursor row at the center / top / bottom |
| `{` `}` | Previous / next project |
| Enter, `o` | Open a project; on a checkout, quit and change into it |
| `l`, →, Tab | Open a project |
| `h`, ←, Esc, Ctrl-O | Back: close the output, clear a search, leave the project |
| `/` | Fuzzy-find by host, owner, project, branch, path, stack, or agent |
| `t` | Toggle the tree |
| `s` | Sort by name, recent activity, or size |
| `r` | Refresh |
| `O` | Show the latest command's output full screen |
| `?` | List every key |
| `q` | Quit, or close help and the output view |

The cursor moves inside the window, and the list scrolls when it comes within
`hq.scrolloff` rows of the top or bottom (default 10).

`/` searches as you type; Enter keeps the results and returns to the motion
keys, and Esc clears the search. While typing, ↑ ↓ and Ctrl-N/P still move
through the matches, and the `/` line and the `:` command line take the usual
readline keys: Ctrl-B/F move, Ctrl-A/E jump, Ctrl-W deletes a word, Ctrl-U
deletes back to the start, and Ctrl-K deletes to the end. `user:<name>` and `organisation:<name>` (also `org:` and
`organization:`) match the owner. A GitHub owner is classified in the
background; until that is known, or on any other host, either prefix matches
the owner text.

## Commands

Commands act on the selected checkout, or on the selected project's main
checkout on the home list.

| Key | Command | Action |
|---|---|---|
| `a` | `:add <branch>` | Create a branch and its worktree in the project, then select it |
| `c`, Enter | `:cd` | Quit and change into the checkout (needs the [shell integration](integrations.md#shells)) |
| `dd` | `:remove` | Remove the worktree and keep its branch |
| `D` | `:delete` | Remove the worktree and delete its branch, if its commits are on its upstream or merged |
| `C` | `:clean` | Remove the project's merged and squash-merged worktrees |
| `P` | `:prune [all]` | Drop deleted and stale worktrees of the project, or of every project |
| `f` | `:fetch` | Fetch the project's remote, then refresh ahead and behind counts |
| `e` | `:edit` | Open the checkout in `$VISUAL` or `$EDITOR` |
| `A` | `:start [agent]` | Start Claude Code, Codex, or OpenCode in the checkout |
| `R` | `:resume [query]` | Resume a coding-agent session from the checkout |
| `yy` | `:copy` | Copy the checkout's path to the clipboard |

`:` opens a command line listing these commands, plus `refresh`, `sort`,
`tree`, `help`, and `quit`. Type to filter, Tab completes the name, and
Enter runs it with the rest of the line as arguments.

Removing a worktree, deleting a branch, and cleaning ask first, explaining
what will happen. A worktree with uncommitted changes is refused. A branch is
deleted only when its commits are on its upstream, or merged into the main
checkout when it has none; squash-merged branches are what `C` is for. Clean
shows the exact worktrees its plan would remove. Nothing passes `--force`.

Bonsai commands run in the background and stream their output into a pane
below the list, which stays usable; Esc closes the pane. Coding agents,
editors, and prune (which may ask about orphaned directories) take over the
terminal until they exit, then hq refreshes.

Warnings are appended to `~/.local/state/bonsai/hq.log`. `--json` waits for
status and sizes and includes `footprint` on each worktree.
