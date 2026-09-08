use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::parallel;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::{cleanup_empty_dirs, find_worktree_dirs_checked};

pub fn run(config: &Config, repo: Option<&Repo>, all: bool, yes: bool) -> Result<()> {
    let root = config.root_dir();
    let _root_lock = crate::paths::lock_root(&root, all)?;
    let repo = if all {
        None
    } else {
        Some(repo.ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?)
    };
    let _lock = repo.map(Repo::lock_mutations).transpose()?;

    let orphans: Vec<PathBuf> = if all {
        // Whole-root sweep: a checkout whose `.git` file points at a git dir
        // that no longer exists (the main clone was deleted) is unrecoverable
        // from the git side and can only be found here.
        let paths = find_worktree_dirs_checked(&root)?;
        let mut orphans = Vec::new();
        for path in paths {
            if is_orphan(&path)? {
                orphans.push(path);
            }
        }
        orphans
    } else {
        let repo = repo.expect("repository required above");
        repo.git.run(&["worktree", "prune"])?;
        eprintln!("bonsai: pruned stale worktree registrations");
        // Directories on disk that git does not know about (crash leftovers).
        let registered: HashSet<PathBuf> = repo
            .worktrees()?
            .iter()
            .filter_map(|wt| crate::paths::canonicalize_ok(&wt.path))
            .collect();
        let directory = crate::paths::ensure_contained(&repo.bonsai_dir(config), &root)?;
        let paths = find_worktree_dirs_checked(&directory)?;
        let mut orphans = Vec::new();
        for path in paths {
            let canonical = crate::paths::canonicalize_or_self(&path);
            if !registered.contains(&canonical) && is_orphan(&path)? {
                orphans.push(path);
            }
        }
        orphans
    };

    if !orphans.is_empty() {
        let snapshots = orphans
            .iter()
            .map(std::fs::symlink_metadata)
            .collect::<std::io::Result<Vec<_>>>()?;
        eprintln!("bonsai: orphaned directories (not registered as worktrees):");
        for (index, path) in orphans.iter().enumerate() {
            eprintln!("  [{}/{}] {}", index + 1, orphans.len(), path.display());
        }
        // They may contain uncommitted work, hence the confirmation.
        if yes || picker::confirm("Delete these directories?")? {
            let workers = parallel::worker_count(orphans.len());
            if workers > 1 {
                eprintln!(
                    "bonsai: deleting {} orphaned directories in parallel ({workers} jobs)",
                    orphans.len(),
                );
            }
            let pending = orphans.iter().zip(&snapshots).collect::<Vec<_>>();
            let results = parallel::map_ordered(&pending, |(path, snapshot)| -> Result<()> {
                crate::paths::ensure_contained(path, &root)?;
                let current = std::fs::symlink_metadata(path)?;
                anyhow::ensure!(
                    current.is_dir() && same_directory(snapshot, &current),
                    "orphan directory changed since selection: {}",
                    path.display()
                );
                anyhow::ensure!(
                    is_orphan(path)?,
                    "orphan metadata changed since selection: {}",
                    path.display()
                );
                std::fs::remove_dir_all(path)?;
                Ok(())
            });
            let mut failures = 0;
            for (index, (path, result)) in orphans.iter().zip(results).enumerate() {
                match result {
                    Ok(()) => {
                        eprintln!(
                            "bonsai: [{}/{}] deleted {}",
                            index + 1,
                            orphans.len(),
                            path.display()
                        );
                        if let Some(parent) = path.parent() {
                            cleanup_empty_dirs(parent, &root);
                        }
                    }
                    Err(error) => {
                        failures += 1;
                        eprintln!(
                            "bonsai: [{}/{}] failed to delete {}: {error}",
                            index + 1,
                            orphans.len(),
                            path.display()
                        );
                    }
                }
            }
            if failures > 0 {
                bail!("failed to delete {failures} orphaned directories");
            }
        }
    }

    if let Some(repo) = repo {
        crate::workspace::sync_quietly(repo, config);
    } else {
        crate::workspace::sync_global_quietly(config);
    }
    remove_empty_tree(&root);
    eprintln!("bonsai: done");
    Ok(())
}

/// The target of a linked checkout's `.git` file (`gitdir: <path>`).
fn is_orphan(worktree: &Path) -> Result<bool> {
    let dot_git = worktree.join(".git");
    if !std::fs::symlink_metadata(&dot_git)?.file_type().is_file() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(dot_git)?;
    let Some(target) = content
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|target| !target.is_empty())
    else {
        eprintln!(
            "bonsai: preserving {}: unrecognized .git metadata",
            worktree.display()
        );
        return Ok(false);
    };
    let path = PathBuf::from(target);
    let target = if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    };
    match std::fs::symlink_metadata(target) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// Depth-first removal of empty directories under `root` (root itself stays).
/// Workspace ownership belongs to the workspace writer; extensions alone
/// cannot distinguish generated files from a user's editor configuration.
fn remove_empty_tree(dir: &Path) {
    if !std::fs::symlink_metadata(dir).is_ok_and(|metadata| metadata.is_dir()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && path.file_name().is_none_or(|name| name != ".locks")
            && std::fs::symlink_metadata(path.join(".git"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            remove_empty_tree(&path);
            let _ = std::fs::remove_dir(&path); // only succeeds when empty
        }
    }
}

fn same_directory(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev() && before.ino() == after.ino()
    }
    #[cfg(not(unix))]
    {
        before
            .created()
            .ok()
            .zip(after.created().ok())
            .is_some_and(|(before, after)| before == after)
    }
}
