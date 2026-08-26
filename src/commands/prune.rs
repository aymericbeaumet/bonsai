use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::{cleanup_empty_dirs, find_worktree_dirs};

pub fn run(config: &Config, all: bool, yes: bool) -> Result<()> {
    let root = config.root_dir();
    let mut orphans: Vec<PathBuf> = Vec::new();

    if all {
        // Whole-root sweep: a checkout whose `.git` file points at a git dir
        // that no longer exists (the main clone was deleted) is unrecoverable
        // from the git side and can only be found here.
        for path in find_worktree_dirs(&root) {
            if gitdir_target(&path).is_none_or(|gitdir| !gitdir.exists()) {
                orphans.push(path);
            }
        }
    } else {
        let repo = Repo::require()?;
        repo.git.run(&["worktree", "prune"])?;
        eprintln!("bonsai: pruned stale worktree registrations");
        // Directories on disk that git does not know about (crash leftovers).
        let registered: HashSet<PathBuf> = repo
            .worktrees()?
            .iter()
            .filter_map(|wt| wt.path.canonicalize().ok())
            .collect();
        for path in find_worktree_dirs(&repo.bonsai_dir(config)) {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !registered.contains(&canonical) {
                orphans.push(path);
            }
        }
    }

    if !orphans.is_empty() {
        eprintln!("bonsai: orphaned directories (not registered as worktrees):");
        for path in &orphans {
            eprintln!("  {}", path.display());
        }
        // They may contain uncommitted work, hence the confirmation.
        if yes || picker::confirm("Delete these directories?")? {
            for path in &orphans {
                std::fs::remove_dir_all(path)?;
                eprintln!("bonsai: deleted {}", path.display());
                if let Some(parent) = path.parent() {
                    cleanup_empty_dirs(parent, &root);
                }
            }
        }
    }

    remove_empty_tree(&root);
    eprintln!("bonsai: done");
    Ok(())
}

/// The target of a linked checkout's `.git` file (`gitdir: <path>`).
fn gitdir_target(worktree: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let target = content.strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(target);
    Some(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    })
}

/// Depth-first removal of empty directories under `root` (root itself stays).
fn remove_empty_tree(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.join(".git").exists() {
            remove_empty_tree(&path);
            let _ = std::fs::remove_dir(&path); // only succeeds when empty
        }
    }
}
