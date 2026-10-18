use std::path::PathBuf;

use clap::{Parser, Subcommand};

const ABOUT: &str = "Ergonomic git worktree manager";

const LONG_ABOUT: &str = "\
Ergonomic git worktree manager.

Run bonsai from inside any git clone; the worktrees it creates are stored
outside the repository, under a global root (default ~/.bonsai), at
<root>/<repo-id>/<branch>. Every command works identically from the main
checkout or from inside any worktree.

Scope: inside a repository, commands act on that project; -a/--all widens
them to every project under the root. Outside a repository every project is
in scope, and commands that need a single project prompt for one.
-C/--cwd <dir> runs bonsai as if it was started in <dir>, like git -C; it
composes with -a/--all.

Interactive use: enable the shell wrapper with `eval \"$(bonsai init zsh)\"`
so add/cd/remove move your shell automatically, and omit arguments to get
fuzzy pickers.

Scripted use (CI, AI agents): pass arguments explicitly; cd-capable commands
print the target path on stdout, so `cd \"$(bonsai add foo)\"` composes.
Pickers never appear without a terminal. See `bonsai agents`.

Configuration precedence, low to high: built-in defaults,
~/.config/bonsai/config.toml, <repo>/.bonsai.toml, `git config bonsai.*`,
BONSAI_* environment variables, command-line flags.";

const ADD_LONG: &str = "\
Create a worktree and cd into it (prints the path without the shell wrapper).
Existing branch names are preserved exactly. New branch inputs are slugified
segment-by-segment (`Fix API/Login` becomes `fix-api/login`); `/` remains the
branch and directory delimiter.

Branch resolution, in order:
  - already checked out in a bonsai worktree: reuse it (idempotent)
  - checked out in an external worktree: refuse it (external paths are read-only)
  - exists locally: check it out in the new worktree
  - exists on the remote: create a local branch tracking it
  - otherwise: create it from --base, or from the default branch

Before creating a worktree, bonsai fetches the selected remote with --prune,
so the remote default branch and remote-only branches are current. Disable
this with [add] fetch = false; --fetch overrides that setting for one add.

--base refs resolve against the directory you run bonsai from, so
`bonsai add fixup --base HEAD` inside a worktree stacks on that worktree's
HEAD. Untracked files matching the copy configuration (.env*, .envrc,
.mcp.json, CLAUDE.local.md, ... by default) are copied from your current
worktree (then the main one) into the new worktree, dependencies are
installed with the package manager detected from lockfiles (pnpm, npm, yarn,
bun, cargo, uv — lockfile-preserving, reported when the tool is missing, disable
with [add] install = false). Independent ecosystems install concurrently with
live output labelled by tool. Cargo's target directory is cloned from your
current worktree (then the main one) when the filesystem supports
copy-on-write, so compiled dependencies are reused without using more disk
(disable with [add] clone_cargo_target = false). Bonsai warns when an actionable package-manager
setting would make sibling worktree installs substantially faster, then the
[add] post_add command runs after every install inside the new worktree.

Examples:
  bonsai add                    # fuzzy-pick a branch, or type a new name
  bonsai add fix-parser         # branch + worktree, based on the default branch
  bonsai add fix-parser --base HEAD    # stack on the current checkout
  bonsai add -a                 # pick the project first, then the branch
  bonsai -C ~/src/api add fix   # another project, from anywhere
  cd \"$(bonsai add fix-parser)\"        # without the shell wrapper

Outside a repository, or with -a/--all, a picker chooses the project first
(terminal only); scripts pass -C <dir> instead.";

const LIST_LONG: &str = "\
List worktrees of the current repo, one per line, in tab-aligned columns:
<branch>  <path>  <flags>. Root and external worktrees are labelled
`<branch> (root)` and `<branch> (external)`. Flags: locked, prunable, dirty
(with --status). The main checkout is listed first. With -a/--all (the
default outside a repo), every project is listed, each row prefixed by its
repo id; worktrees whose clone is gone are listed as prunable. --json
outputs an array of {branch, path, main, external, locked, prunable, repo,
dirty?} instead. With --status,
dirty is null when Git cannot determine status; errors are never shown as clean.";

const REMOVE_LONG: &str = "\
Remove worktrees by branch or exact checkout path (including detached
worktrees). Without arguments, opens a fuzzy
multi-select of this repo's worktrees (terminal only).

The branch itself is kept unless -d/--delete-branch is passed (use `clean`
for \"remove everything that is merged\"). Worktrees with uncommitted changes
are refused unless --force. Removing the worktree you are standing in is
supported: the shell wrapper brings you back to the main checkout.

With -a/--all (the default outside a repo), candidates span every project,
labelled by repo id, and a branch argument selects that branch's worktree
in every project that has one.

Examples:
  bonsai remove                 # fuzzy pick
  bonsai remove fix-parser -d   # remove worktree and delete the branch";

const PRUNE_LONG: &str = "\
Housekeeping for the bonsai root: runs `git worktree prune` for the current
repo, deletes orphaned directories (crash leftovers not registered as
worktrees, after confirmation), and removes empty directories. With
-a/--all (the default outside a repo), prunes every project and sweeps the
entire bonsai root, including worktrees whose main clone has been deleted
(their .git file points nowhere). Confirmed independent
orphan directories are deleted concurrently with numbered result lines.";

const CLEAN_LONG: &str = "\
Remove every worktree whose branch is already integrated into the default
branch, then delete those branches. Detects regular merges and squash-merges
(content comparison), including branches whose upstream was deleted after
merging. A missing upstream alone never proves integration. Fetches with
--prune first (disable with --no-fetch or [clean] fetch = false).

Never touched: the main checkout, the default branch, locked worktrees,
dirty worktrees (skipped and reported, even with --yes/--force), branches
with unpushed commits on a live upstream, and branches matching [clean]
protected globs. The plan is always printed; without --yes (alias
-f/--force) a confirmation multi-select opens (terminal only).

With -a/--all (the default outside a repo), every project is analyzed with
its own configuration, then one combined plan is confirmed; --json prints
an array with one report per project, each carrying its repo id.

Dirty and merge-equivalence checks run concurrently and retain worktree order
in the plan. Removal and branch deletion remain sequential because they mutate
one shared Git repository.

Examples:
  bonsai clean --dry-run        # show what would be removed
  bonsai clean --yes            # no confirmation (scripts, agents)
  bonsai clean -f               # same: --force is an alias of --yes
  bonsai clean -a --dry-run     # every project";

const CD_LONG: &str = "\
Jump to a worktree. With the shell wrapper this cds; without it the target
path is printed on stdout. Inside a repo, candidates are that repo's
Git-registered worktrees—including external worktrees created by other
tools—plus its main checkout. With -a/--all, and outside a repo, candidates
are the worktrees of every project, labelled by repo id. A unique exact
branch match wins, then a unique substring match; anything ambiguous opens
the fuzzy picker pre-filtered with the query (terminal only).

The picker lists worktrees most recently worked-in first, each with a
last-change age colored by freshness: green = today, yellow = this week,
dim = older (respects NO_COLOR). It renders inline below the current prompt
and clears on exit. Filtering is asynchronous. Arrow keys and Ctrl-P/N
navigate; Ctrl-W/A/E/U and the usual cursor keys edit the query.";

const RESUME_LONG: &str = "\
Resume an AI coding session from any Git-registered worktree of the current
project, including external worktrees created by other tools. Bonsai combines
every resumable top-level Claude Code, Codex, and OpenCode session into one
fuzzy picker, ordered by the time each session was last used. Rows share
`bonsai cd`'s compact relative ages, freshness colors, navigation, and query
editing keys. Provider stores and independent transcript files are scanned
concurrently. The picker renders inline below the current prompt and clears on
exit.

The picker searches provider, title, worktree/directory, and session ID. An
exact session ID or a unique substring skips the picker; otherwise QUERY is
used as its starting filter. The selected harness opens in the session's
original directory, or the main checkout if that historical worktree no
longer exists.

Session stores are read from the harnesses' standard locations, respecting
CLAUDE_CONFIG_DIR, CODEX_HOME, CODEX_SQLITE_HOME, XDG_DATA_HOME, and
OPENCODE_DB. Run this command inside any worktree of the project whose
sessions you want to search, or pass -a/--all to search every project.";

const INIT_LONG: &str = "\
Print the shell integration for zsh, bash, or fish. Add to your shell rc:

  eval \"$(bonsai init zsh)\"     # zsh (also loads completions)
  eval \"$(bonsai init bash)\"    # bash
  bonsai init fish | source     # fish

The wrapper makes add/cd/remove/clean move your shell into the right
directory when stdout is a terminal. Substitutions, pipes, and redirections
preserve the raw path; `command bonsai` always bypasses the wrapper. It uses
a plain `cd`, so directory-change hooks keep working.";

const AGENTS_LONG: &str = "\
Print the bundled Agent Skill without YAML frontmatter for AI coding agents
(Claude Code, Cursor, Codex, OpenCode, ...). Append it to the file your
harnesses read:

  bonsai agents >> AGENTS.md";

const WORKSPACE_LONG: &str = "\
bonsai maintains multi-root VS Code workspace files — the native, standard
way to get every worktree as a root folder in the Explorer of VS Code,
Cursor, Windsurf, and other derivatives, no extension required. Editors
watch the file, so the left tree updates live as bonsai adds and removes
worktrees. Workspace settings, tasks, launch configuration, comments, and
custom folders are preserved when Bonsai refreshes its folders.

  <root>/<repo-id>/<repo>.code-workspace   per repo: all registered worktrees
  <root>/bonsai.code-workspace             global: Bonsai-managed worktrees

Both are kept up to date by add/remove/clean/prune (disable with
`workspace = false`). This command refreshes one and prints its path:

  code \"$(bonsai workspace)\"          # this repo's worktrees
  cursor \"$(bonsai workspace -a)\"     # everything under the bonsai root

Outside a repo, the global file is the default.

The per-repo file includes external worktrees registered by other tools. The
global file cannot discover those paths without a current repo context.

Claude Code and Codex desktop open plain folders: point them at a worktree
directory (<root>/<repo-id>/<branch>) or use `bonsai cd`.";

const HQ_LONG: &str = "\
Browse projects in the terminal, including each project's main checkout
and all Git-registered worktrees. Inside a repository hq shows that project,
even when it has no Bonsai-managed worktrees yet; -a/--all, and running
outside a repository, show every project under the Bonsai root. Editor
workspace files contribute checkouts that live outside the root.

The home list is one row per project, in alphabetical order by host, owner,
and repository. s cycles between that and most recent. Each row
shows the host, the repository owner, and the name, not the branch. A heading
names each visible column. An open tree and an open project use a ruled
frame; the home list stays a table. t opens
and closes the tree, grouping checkouts by host, then owner, then
repository. Host, owner, and project each have their own colour, in the table and in the tree. j/k or the
arrow keys move one row, as do ctrl-e and ctrl-y. ctrl-d and ctrl-u move
half a page. gg jumps to the top and G to the bottom. The cursor moves
inside the window, and the list scrolls when it comes within hq.scrolloff
rows of the top or bottom (default 10). / fuzzy-finds by host, owner, project, branch, path, stack,
or harness.
The / line and the : command line take readline keys (Ctrl-B/F, Ctrl-A/E,
Ctrl-W, Ctrl-U, Ctrl-K). user:<name> and organisation:<name> (also org: and
organization:) match the
owner. Enter and o open that project, listing its checkouts by branch.
ctrl-i does the same. A search that matches does too. ctrl-o and esc go up
one step, clearing a search or leaving the project. : runs add, remove,
clean, and prune.
r refreshes. q quits. The checkout path stays on the status line.

Git status and checkout sizes fill in on that same row after the list
appears. A missing checkout is marked missing and is not passed to git.
Warnings are appended to ~/.local/state/bonsai/hq.log. --json prints the
inventory, including status and each footprint, and exits without a
terminal.

Examples:
  bonsai hq
  bonsai hq -a
  bonsai hq --json
  bonsai --root ~/trees hq";

const START_LONG: &str = "\
Start a new Claude Code, Codex, or OpenCode session in the current worktree.
The harness inherits your terminal and current directory, including when
you run this from a subdirectory or an external worktree.

With -a/--all, or outside a repository, pick a worktree from every project
first (terminal only); the harness then starts there.

Pass a provider explicitly, or omit it to pick from installed providers.
When only one provider is installed it is selected automatically. Use
--prompt to supply an initial prompt while staying in an interactive session.

Examples:
  bonsai start                         # choose an installed coding tool
  bonsai start codex                   # start a new Codex session here
  bonsai start -a codex                # pick a worktree from any project
  bonsai start claude --prompt 'Fix the failing tests'

Use bonsai resume to find a past session, or bonsai hq to browse worktrees.";

const SKILL_LONG: &str = "\
The bonsai Agent Skill (SKILL.md, agentskills.io format) teaches AI coding
agents the worktree workflow, its invariants, and the destructive-command
policy.

  bonsai skill              # print SKILL.md to stdout
  bonsai skill install      # install into every detected harness:
                            #   Claude Code  ~/.claude/skills/bonsai/
                            #   Codex        ~/.codex/skills/bonsai/
                            #   OpenCode     ~/.config/opencode/skills/bonsai/
                            #   Cursor       ~/.agents/skills/bonsai/
                            #   Pi           ~/.pi/agent/skills/bonsai/
  bonsai skill install --all  # skip detection, install everywhere";

#[derive(Debug, Subcommand)]
pub enum SkillAction {
    /// Install SKILL.md into the skill directories of detected AI harnesses
    Install {
        /// Install for every known harness, even undetected ones
        #[arg(long)]
        all: bool,
    },
}

/// Ergonomic git worktree manager.
#[derive(Debug, Parser)]
#[command(name = "bonsai", version, about = ABOUT, long_about = LONG_ABOUT)]
pub struct Cli {
    /// Run as if bonsai was started in DIR (repeatable, like git -C)
    #[arg(short = 'C', long = "cwd", global = true, value_name = "DIR", action = clap::ArgAction::Append)]
    pub cwd: Vec<PathBuf>,

    /// Root directory holding all worktrees (default: ~/.bonsai)
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<String>,

    /// Remote used for tracking, fetching, and repo identification
    /// (default: checkout.defaultRemote, then origin)
    #[arg(long, global = true, value_name = "NAME")]
    pub remote: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Browse worktrees in the terminal
    #[command(long_about = HQ_LONG)]
    Hq {
        /// Print the worktree inventory as JSON and exit
        #[arg(long)]
        json: bool,
        /// Show every project, not only the current one
        #[arg(short, long)]
        all: bool,
    },
    /// Start a new coding session in the current worktree
    #[command(long_about = START_LONG)]
    Start {
        /// Coding tool to launch (pick from installed tools when omitted)
        #[arg(value_enum)]
        provider: Option<crate::commands::start::Provider>,
        /// Initial prompt for the new interactive session
        #[arg(long)]
        prompt: Option<String>,
        /// Pick the worktree from every project first
        #[arg(short, long)]
        all: bool,
    },
    /// Create a worktree (and its branch) under the bonsai root, then cd into it
    #[command(long_about = ADD_LONG)]
    Add {
        /// Existing branch to check out, or a new branch input to slugify.
        /// Slashes remain nested branch/path delimiters. Omit it (on a
        /// terminal) to fuzzy-pick or type a new name
        branch: Option<String>,
        /// Base ref for a newly created branch, resolved against the current
        /// directory (default: the default branch)
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Fetch even when [add] fetch = false
        #[arg(long)]
        fetch: bool,
        /// Override the worktree path inside this project's Bonsai directory
        /// (escape hatch for path collisions)
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
        /// Pick the project first, from every project
        #[arg(short, long)]
        all: bool,
    },
    /// List worktrees of the current repo (or every repo with -a)
    #[command(alias = "ls", long_about = LIST_LONG)]
    List {
        /// List the worktrees of every project
        #[arg(short, long)]
        all: bool,
        /// Add a dirty flag (runs git status across worktrees concurrently)
        #[arg(long)]
        status: bool,
        /// Output JSON instead of TSV
        #[arg(long)]
        json: bool,
    },
    /// Remove worktrees, keeping their branches unless -d
    #[command(alias = "rm", long_about = REMOVE_LONG)]
    Remove {
        /// Branches or exact managed paths to remove; omit (on a terminal) to
        /// fuzzy multi-select
        branches: Vec<String>,
        /// Also delete the branch (refuses unmerged branches unless --force)
        #[arg(short = 'd', long)]
        delete_branch: bool,
        /// Discard uncommitted changes / force-delete the branch
        #[arg(short, long)]
        force: bool,
        /// Select worktrees from every project
        #[arg(short, long)]
        all: bool,
    },
    /// Clean up stale registrations, orphaned directories, and empty dirs
    #[command(long_about = PRUNE_LONG)]
    Prune {
        /// Prune every project and sweep the whole bonsai root, including
        /// repos whose clone is gone
        #[arg(short, long)]
        all: bool,
        /// Do not ask for confirmation
        #[arg(short = 'y', long, visible_short_alias = 'f', visible_alias = "force")]
        yes: bool,
    },
    /// Remove worktrees merged into the default branch (incl. squash-merges)
    #[command(long_about = CLEAN_LONG)]
    Clean {
        /// Show what would be removed without touching anything
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Do not ask for confirmation (dirty worktrees are still skipped)
        #[arg(short = 'y', long, visible_short_alias = 'f', visible_alias = "force")]
        yes: bool,
        /// Skip the initial fetch --prune
        #[arg(long)]
        no_fetch: bool,
        /// Output the plan/result as JSON on stdout
        #[arg(long)]
        json: bool,
        /// Clean every project
        #[arg(short, long)]
        all: bool,
    },
    /// Jump to a worktree (fuzzy; every project with -a or outside a repo)
    #[command(long_about = CD_LONG)]
    Cd {
        /// Branch name or fuzzy query; prints the path without the wrapper
        query: Option<String>,
        /// Jump to a worktree of any project
        #[arg(short, long)]
        all: bool,
    },
    /// Resume a Claude Code, Codex, or OpenCode session from this project (or all with -a)
    #[command(long_about = RESUME_LONG)]
    Resume {
        /// Session ID or fuzzy query across provider, title, and worktree
        #[arg(conflicts_with_all = ["provider", "session"])]
        query: Option<String>,
        /// Provider of the exact session to resume
        #[arg(long, value_enum, requires = "session")]
        provider: Option<crate::commands::start::Provider>,
        /// Exact session ID; never falls back to a fuzzy match
        #[arg(long, requires = "provider")]
        session: Option<String>,
        /// Search the sessions of every project
        #[arg(short, long)]
        all: bool,
    },
    /// Print the repo's .code-workspace file path (refreshing it first)
    #[command(long_about = WORKSPACE_LONG)]
    Workspace {
        /// The global workspace instead: every worktree of every repo
        #[arg(short, long)]
        all: bool,
    },
    /// Print the shell wrapper enabling auto-cd (eval "$(bonsai init zsh)")
    #[command(long_about = INIT_LONG)]
    Init {
        /// Shell flavor to emit
        shell: crate::shell::Shell,
    },
    /// Print AI-agent usage instructions (bonsai agents >> AGENTS.md)
    #[command(long_about = AGENTS_LONG)]
    Agents,
    /// Print or install the bonsai Agent Skill (SKILL.md)
    #[command(long_about = SKILL_LONG)]
    Skill {
        #[command(subcommand)]
        action: Option<SkillAction>,
    },
    /// Print shell completions
    Completions {
        /// Shell flavor to emit
        shell: clap_complete::Shell,
    },
}
