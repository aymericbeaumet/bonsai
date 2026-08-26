use anyhow::Result;

use crate::config::Config;
use crate::git::Git;
use crate::repo::Repo;
use crate::worktree::{Worktree, find_worktree_dirs};

pub fn run(config: &Config, all: bool, status: bool) -> Result<()> {
    if all {
        return run_all(config, status);
    }
    let repo = Repo::require()?;
    let bonsai_dir = repo.bonsai_dir(config);
    for wt in repo.worktrees()? {
        if wt.is_bare {
            continue;
        }
        let is_main = !wt.path.starts_with(&bonsai_dir);
        let mut flags = Vec::new();
        if is_main {
            flags.push("main");
        }
        if wt.is_locked {
            flags.push("locked");
        }
        if wt.is_prunable {
            flags.push("prunable");
        }
        if status && is_dirty(&wt) {
            flags.push("dirty");
        }
        println!(
            "{}\t{}\t{}",
            wt.branch.as_deref().unwrap_or("(detached)"),
            wt.path.display(),
            flags.join(",")
        );
    }
    Ok(())
}

/// Global listing scans the bonsai root on disk (there is no repo context to
/// ask git from); branch names are read from each checkout.
fn run_all(config: &Config, status: bool) -> Result<()> {
    let root = config.root_dir();
    for path in find_worktree_dirs(&root) {
        let git = Git::at(&path);
        let branch = git
            .out(&["branch", "--show-current"])
            .ok()
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "(unknown)".to_string());
        let mut flags = Vec::new();
        if status {
            let dirty = git
                .out(&["status", "--porcelain"])
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if dirty {
                flags.push("dirty");
            }
        }
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        println!("{}\t{}\t{}", branch, rel.display(), flags.join(","));
    }
    Ok(())
}

fn is_dirty(wt: &Worktree) -> bool {
    Git::at(&wt.path)
        .out(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}
