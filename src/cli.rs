use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Ergonomic git worktree manager: worktrees for any repo, centralized under
/// a global root, with fuzzy pickers and automatic branch creation.
#[derive(Debug, Parser)]
#[command(name = "bonsai", version, about)]
pub struct Cli {
    /// Root directory holding all worktrees (default: ~/.bonsai)
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<String>,

    /// Remote used for tracking, fetching, and repo identification
    #[arg(long, global = true, value_name = "NAME")]
    pub remote: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create a worktree (and its branch) under the bonsai root, then cd into it
    Add {
        /// Branch to check out; created automatically if it does not exist
        branch: Option<String>,
        /// Base ref for a newly created branch (default: the default branch)
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Fetch the remote first
        #[arg(long)]
        fetch: bool,
        /// Override the worktree path (escape hatch for path collisions)
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },
    /// List worktrees of the current repo (or all repos with --all)
    #[command(alias = "ls")]
    List {
        /// List every bonsai-managed worktree across all repos
        #[arg(long)]
        all: bool,
        /// Show dirty status (runs git status in each worktree)
        #[arg(long)]
        status: bool,
    },
    /// Remove worktrees (fuzzy picker when no branch is given)
    #[command(alias = "rm")]
    Remove {
        /// Branches whose worktrees to remove
        branches: Vec<String>,
        /// Also delete the branch (refuses unmerged unless --force)
        #[arg(short = 'd', long)]
        delete_branch: bool,
        /// Discard uncommitted changes / force-delete the branch
        #[arg(short, long)]
        force: bool,
    },
    /// Clean up stale worktree registrations, orphaned directories, and empty dirs
    Prune {
        /// Sweep the whole bonsai root, including repos whose clone is gone
        #[arg(long)]
        all: bool,
        /// Do not ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Remove worktrees whose branch is merged into the default branch
    Clean {
        /// Show what would be removed without touching anything
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
        /// Skip the initial fetch --prune
        #[arg(long)]
        no_fetch: bool,
    },
    /// Jump to a worktree (fuzzy picker; global when outside a repo)
    Cd {
        /// Branch name or fuzzy query
        query: Option<String>,
    },
    /// Print the shell wrapper enabling auto-cd (eval "$(bonsai init zsh)")
    Init { shell: crate::shell::Shell },
    /// Print shell completions
    Completions { shell: clap_complete::Shell },
}
