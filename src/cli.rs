use std::path::PathBuf;

use clap::{Parser, Subcommand};

const ABOUT: &str = "Ergonomic git worktree manager";

const LONG_ABOUT: &str = "\
Ergonomic git worktree manager.

Run bonsai from inside any git clone; the worktrees it creates are stored
outside the repository, under a global root (default ~/.bonsai), at
<root>/<repo-id>/<branch>. Every command works identically from the main
checkout or from inside any worktree.

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
The branch input is slugified segment-by-segment (`Fix API/Login` becomes
`fix-api/login`); `/` remains the branch and directory delimiter.

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
bun, cargo, uv — lockfile-frozen, skipped when the tool is missing, disable
with [add] install = false). Bonsai warns when an actionable package-manager
setting would make sibling worktree installs substantially faster, then the
[add] post_add command runs inside the new worktree.

Examples:
  bonsai add                    # fuzzy-pick a branch, or type a new name
  bonsai add fix-parser         # branch + worktree, based on the default branch
  bonsai add fix-parser --base HEAD    # stack on the current checkout
  cd \"$(bonsai add fix-parser)\"        # without the shell wrapper";

const LIST_LONG: &str = "\
List worktrees of the current repo, one per line, in tab-aligned columns:
<branch>  <path>  <flags>. Root and external worktrees are labelled
`<branch> (root)` and `<branch> (external)`. Flags: locked, prunable, dirty
(with --status). The main checkout is listed first. With
--all, list every Bonsai-managed worktree across all repos (works outside a
repo). --json outputs an array of
{branch, path, main, external, locked, prunable, dirty?} instead.";

const REMOVE_LONG: &str = "\
Remove the worktrees of the given branches. Without arguments, opens a fuzzy
multi-select of this repo's worktrees (terminal only).

The branch itself is kept unless -d/--delete-branch is passed (use `clean`
for \"remove everything that is merged\"). Worktrees with uncommitted changes
are refused unless --force. Removing the worktree you are standing in is
supported: the shell wrapper brings you back to the main checkout.

Examples:
  bonsai remove                 # fuzzy pick
  bonsai remove fix-parser -d   # remove worktree and delete the branch";

const PRUNE_LONG: &str = "\
Housekeeping for the bonsai root: runs `git worktree prune` for the current
repo, deletes orphaned directories (crash leftovers not registered as
worktrees, after confirmation), and removes empty directories. With --all,
sweeps the entire bonsai root instead, including worktrees whose main clone
has been deleted (their .git file points nowhere).";

const CLEAN_LONG: &str = "\
Remove every worktree whose branch is already integrated into the default
branch, then delete those branches. Detects three cases: regular merges,
squash-merges (content comparison), and branches whose upstream was deleted
after merging (the GitHub PR flow) — which is why clean fetches with --prune
first (disable with --no-fetch or [clean] fetch = false).

Never touched: the main checkout, the default branch, locked worktrees,
dirty worktrees (skipped and reported, even with --yes/--force), branches
with unpushed commits on a live upstream, and branches matching [clean]
protected globs. The plan is always printed; without --yes (alias
-f/--force) a confirmation multi-select opens (terminal only).

Examples:
  bonsai clean --dry-run        # show what would be removed
  bonsai clean --yes            # no confirmation (scripts, agents)
  bonsai clean -f               # same: --force is an alias of --yes";

const CD_LONG: &str = "\
Jump to a worktree. With the shell wrapper this cds; without it the target
path is printed on stdout. Inside a repo, candidates are that repo's
Git-registered worktrees—including external worktrees created by other
tools—plus its main checkout. Outside a repo, candidates are every
Bonsai-managed worktree of every repo. An exact branch match wins, then a
unique substring match; anything ambiguous opens the fuzzy picker
pre-filtered with the query (terminal only).

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
editing keys. The picker renders inline below the current prompt and clears
on exit.

The picker searches provider, title, worktree/directory, and session ID. An
exact session ID or a unique substring skips the picker; otherwise QUERY is
used as its starting filter. The selected harness opens in the session's
original directory, or the main checkout if that historical worktree no
longer exists.

Session stores are read from the harnesses' standard locations, respecting
CLAUDE_CONFIG_DIR, CODEX_HOME, CODEX_SQLITE_HOME, XDG_DATA_HOME, and
OPENCODE_DB. Run this command inside any worktree of the project whose
sessions you want to search.";

const INIT_LONG: &str = "\
Print the shell integration for zsh, bash, or fish. Add to your shell rc:

  eval \"$(bonsai init zsh)\"     # zsh (also loads completions)
  eval \"$(bonsai init bash)\"    # bash
  bonsai init fish | source     # fish

The wrapper makes add/cd/remove/clean move your shell into the right
directory. It uses a plain `cd`, so chpwd-based tools (zoxide, direnv,
starship, ...) keep working.";

const AGENTS_LONG: &str = "\
Print a concise markdown usage contract intended for AI coding agents
(Claude Code, Cursor, Codex, OpenCode, ...). Append it to the file your
harnesses read:

  bonsai agents >> AGENTS.md";

const WORKSPACE_LONG: &str = "\
bonsai maintains multi-root VS Code workspace files — the native, standard
way to get every worktree as a root folder in the Explorer of VS Code,
Cursor, Windsurf, and other derivatives, no extension required. Editors
watch the file, so the left tree updates live as bonsai adds and removes
worktrees.

  <root>/<repo-id>/<repo>.code-workspace   per repo: all registered worktrees
  <root>/bonsai.code-workspace             global: Bonsai-managed worktrees

Both are kept up to date by add/remove/clean/prune (disable with
`workspace = false`). This command refreshes one and prints its path:

  code \"$(bonsai workspace)\"          # this repo's worktrees
  cursor \"$(bonsai workspace --all)\"  # everything under the bonsai root

The per-repo file includes external worktrees registered by other tools. The
global file cannot discover those paths without a current repo context.

Claude Code and Codex desktop open plain folders: point them at a worktree
directory (<root>/<repo-id>/<branch>) or use `bonsai cd`.";

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
    /// Create a worktree (and its branch) under the bonsai root, then cd into it
    #[command(long_about = ADD_LONG)]
    Add {
        /// Branch input to slugify and check out; created if it does not exist.
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
    },
    /// List worktrees of the current repo (or every repo with --all)
    #[command(alias = "ls", long_about = LIST_LONG)]
    List {
        /// List every bonsai-managed worktree across all repos
        #[arg(long)]
        all: bool,
        /// Add a dirty flag (runs git status in each worktree; slower)
        #[arg(long)]
        status: bool,
        /// Output JSON instead of TSV
        #[arg(long)]
        json: bool,
    },
    /// Remove worktrees, keeping their branches unless -d
    #[command(alias = "rm", long_about = REMOVE_LONG)]
    Remove {
        /// Branches whose worktrees to remove; omit (on a terminal) to
        /// fuzzy multi-select
        branches: Vec<String>,
        /// Also delete the branch (refuses unmerged branches unless --force)
        #[arg(short = 'd', long)]
        delete_branch: bool,
        /// Discard uncommitted changes / force-delete the branch
        #[arg(short, long)]
        force: bool,
    },
    /// Clean up stale registrations, orphaned directories, and empty dirs
    #[command(long_about = PRUNE_LONG)]
    Prune {
        /// Sweep the whole bonsai root, including repos whose clone is gone
        #[arg(long)]
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
    },
    /// Jump to a worktree (fuzzy; works across all repos when outside one)
    #[command(long_about = CD_LONG)]
    Cd {
        /// Branch name or fuzzy query; prints the path without the wrapper
        query: Option<String>,
    },
    /// Resume a Claude Code, Codex, or OpenCode session from this project
    #[command(long_about = RESUME_LONG)]
    Resume {
        /// Session ID or fuzzy query across provider, title, and worktree
        query: Option<String>,
    },
    /// Print the repo's .code-workspace file path (refreshing it first)
    #[command(long_about = WORKSPACE_LONG)]
    Workspace {
        /// The global workspace instead: every worktree of every repo
        #[arg(long)]
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
