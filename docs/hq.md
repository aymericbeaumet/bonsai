# Headquarters

```sh
bonsai hq          # this project, or every project outside a repository
bonsai hq -a       # every project under the bonsai root
bonsai hq --json   # print the same inventory and exit
```

`hq` lists every project under the bonsai root, plus editor workspace folders
and the project you start it from. A checkout with no bonsai worktrees yet
still appears. Each project's main checkout, bonsai worktrees, and external
worktrees are shown together with the coding agents running in them, and
you can create, remove, clean, trim, and open them without leaving the list.
Press `?` inside hq for every key.

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
| running | Coding agents running in its checkouts now, such as `2 agents` |
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
| running | Coding agents running in it now, such as `claude, 2 codex` |
| notes | `main checkout`, `external`, `locked`, `prunable` |

The header above the list names the remote, the main checkout, and the
project's totals, suggests `P` when checkouts were deleted by hand, and
suggests `T` when its worktrees hold dependency directories that trimming
would free.
`upstream gone` usually means the pull request was merged and its branch
deleted; `C` cleans those.

The home list has four **views**, named by tabs on the top border with the
current one highlighted. They go from the shortest list to the longest, each
adding one level of grouping: `v` moves to the next view and `V` to the
previous one, wrapping around at either end.

| View | Top-level rows | Then |
|---|---|---|
| table | one row per project | |
| projects | `github.com/aymericbeaumet/bonsai` | its checkouts |
| owners | `github.com/aymericbeaumet` | its projects, then their checkouts |
| hosts | `github.com` | its owners, then projects, then checkouts |

Every group sums what it contains. `:view table`, `:view projects`,
`:view owners`, and `:view hosts` go straight to one. A **search** (`/`) lists matching
checkouts from every project. Columns that have nothing to show are hidden,
and on a narrow terminal the least useful ones go first.

The status line describes the selection in full: its path, how it relates to
its upstream, what its size is made of, and which agents run there, with
their process IDs.

## Running agents

hq looks for running coding agents (Claude Code, Codex, OpenCode, Cursor,
Gemini, and others) every two seconds, in the background, and lists each
under the checkout its directory is in. The `running` column, the titles,
and every group total count them. `p` (`:ps`) shows only the checkouts
where an agent is running, across every listed project or inside the open
one; `:ps codex` keeps one kind. It is the search `running:` and stays
live as agents start and exit; Esc clears it. Outside hq, `bonsai ps`
lists the same agents, and `bonsai ps -a` also those working outside any
checkout.

## Sizes

Sizes are measured in the background, once the list is on screen, and stream
in while large directories are walked. The last measurement of each checkout
is remembered in `~/.local/state/bonsai/footprints.json`, so the next `hq`
shows every total at once and updates it as checkouts are measured again; a
size that is still being measured is dimmed. After a command, only the
affected projects are measured again; Ctrl-R measures everything.

A checkout's size is the logical size of its files. Shared dependencies are
symlinks, hardlinked files, or external stores such as Go modules; their
store is reported once, not added to every checkout. Copy-on-write clones
report their full logical size as local, so the total can exceed the disk
space actually used.

## Keys

The motion keys work the same in every view: the home list, an open project,
the tree, search results, and also the `?` help, confirmation dialogs, and
the full-screen output (`o`), which they scroll like a pager.

| Keys | Action |
|---|---|
| `j`, ↓, Ctrl-N | Down one row; a count repeats (`5j`) |
| `k`, ↑, Ctrl-P | Up one row; a count repeats (`5k`) |
| `gg`, Home | First row; `3gg` goes to the third |
| `G`, End | Last row; `3G` goes to the third |
| Ctrl-D | Half a page down |
| Ctrl-U | Half a page up |
| Ctrl-F, Page Down | A page down |
| Ctrl-B, Page Up | A page up |
| Ctrl-E | Scroll down, keeping the cursor where it is |
| Ctrl-Y | Scroll up, keeping the cursor where it is |
| `H` | Top of the screen |
| `M` | Middle of the screen |
| `L` | Bottom of the screen |
| `zz` | Put the cursor row at the center |
| `zt` | Put the cursor row at the top |
| `zb` | Put the cursor row at the bottom |
| `{` | Previous project |
| `}` | Next project |
| Enter | Open a project; on a checkout, quit and change into it |
| `l`, →, Tab | Open a project |
| `h`, ←, Esc, Ctrl-O | Back: close the output, clear a search, leave the project |
| `/` | Fuzzy-find by host, owner, project, branch, path, stack, or agent |
| `p` | Only checkouts where a coding agent is running (`:ps [agent]`) |
| `v` | Next view: table, projects, owners, hosts |
| `V` | Previous view: hosts, owners, projects, table |
| Ctrl-S | Sort by name, recent activity, or size |
| Ctrl-R | Refresh |
| `o` | Show the latest command's output full screen |
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
the owner text. `running:` matches checkouts where a coding agent is running,
and `running:<agent>` those where that agent is.

## Commands

The same commands are available in every view. The selected row determines
their scope: a host includes all its projects and checkouts, an owner includes
its projects on that host, a project includes all its checkouts, and a checkout
includes only itself. Search results and an open project use checkout scope.
Selecting the main checkout never expands a destructive action to the project.
Creation and fetch operate once per project in that scope; commands that open
an editor or coding agent need one checkout. On host or owner groups, an
in-UI picker chooses that checkout without leaving HQ.

| Key | Command | Action |
|---|---|---|
| `a` | `:add <branch>` | Create a branch and its worktree in each project in scope |
| `c`, Enter | `:cd` | Quit and change into the checkout (needs the [shell integration](integrations.md#shells)) |
| `R` | `:remove` | Remove selected worktrees and keep their branches |
| `D` | `:delete` | Remove selected worktrees and delete their branches, if their commits are on their upstream or merged |
| `C` | `:clean` | Delete merged and squash-merged worktrees and their branches in the selection |
| `P` | `:prune` | Drop missing registrations (`directory deleted`) and delete eligible orphan directories in the selection; keep branches |
| `T` | `:trim` | Free disk: delete ignored files (`node_modules`, `target`, …) of selected worktrees |
| `f` | `:fetch` | Fetch selected projects' remotes, then refresh ahead and behind counts |
| `e` | `:edit` | Open the checkout in `$VISUAL` or `$EDITOR` |
| `s` | `:start [agent]` | Start Claude Code, Codex, or OpenCode in the checkout |
| `r` | `:resume [query]` | Resume a coding-agent session from the checkout |
| `y` | `:yank` | Copy the checkout's path to the clipboard |

`:` opens a command line listing these commands, plus `refresh`, `sort`,
`view`, `ps`, `output`, `help`, and `quit`. Type to filter, Tab completes the name, and
Enter runs it with the rest of the line as arguments.

Action bindings use the command's first letter. Destructive actions are
uppercase: `R` remove, `D` delete, `C` clean, `P` prune, and `T` trim.
Ctrl-R refresh and Ctrl-S sort leave `r` and `s` for resume and start.
`ps` also accepts `running` and `agents`; `yank` also accepts `copy`.
The footer uses up to two hint lines, keeping each key and its label together.
Distinct actions have separate descriptions in help and footer hints.
At narrow widths it prioritizes destructive actions, `?` help, and `:` commands;
these always expose the full command list. An ellipsis marks omitted hints.

For disk cleanup, choose what you want to keep:

| Command | Deletes | Keeps |
|---|---|---|
| `T` trim | Ignored dependency and build files in selected worktrees | Worktree directories, source files, branches, and protected local configuration |
| `R` remove | Selected worktree directories and their Git registrations | Branches |
| `D` delete | Selected worktree directories, registrations, and their branches | Other checkouts and branches |
| `C` clean | Eligible merged or squash-merged worktrees and branches | Unmerged work, dirty worktrees, and the main checkout |
| `P` prune | Stale registrations, eligible orphaned directories, and empty directories in scope | Branches and existing registered worktrees |

Removing worktrees, deleting branches, cleaning, trimming, and pruning ask
inside hq first, showing the precise targets and explaining what will happen.
Trim and prune verify the reviewed deletion plan under their mutation locks;
if its targets or protected paths change, execution stops for another review.
A worktree with uncommitted changes is refused. A branch is
deleted only when its commits are on its upstream, or merged into the main
checkout when it has none; squash-merged branches are what `C` is for. Clean
shows the exact worktrees its plan would remove. A worktree where another
coding agent is running is never removed, cleaned, or trimmed under it: the
question names the agent, and bonsai refuses or skips that worktree. Nothing
passes `--force`.

Trim shows the plan of `bonsai trim --dry-run --json` first: each worktree,
the space it frees, every ignored root it will delete, and the local configuration
and nested repositories it keeps. Confirming runs `bonsai trim --yes` on
exactly the worktrees you reviewed, then measures the affected projects again.
The main checkout is protected from trim and removal. External worktrees have the same trim and removal
actions and safeguards; their `external` note only means they live outside the
Bonsai root.

Bonsai commands, including prune, run in the background and stream progress,
results, and failures into a pane below the list. The list stays visible and
usable throughout; actions never switch to full-screen command output.
Press `o` to explicitly open the latest output full screen, or Esc to close
the finished pane. Coding agents and editors use the terminal until they exit,
then hq refreshes.

Warnings are appended to `~/.local/state/bonsai/hq.log`. `--json` waits for
status and sizes and includes `footprint` on each worktree, and `agents`:
the coding agents running in it (`kind`, `pid`, `cwd`, `started`, `caller`),
newest first.
