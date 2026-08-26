use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::{Worktree, cleanup_empty_dirs};

pub fn run(
    config: &Config,
    branches: Vec<String>,
    delete_branch: bool,
    force: bool,
) -> Result<Option<PathBuf>> {
    let repo = Repo::require()?;
    let worktrees = repo.bonsai_worktrees(config)?;
    if worktrees.is_empty() {
        bail!("no bonsai worktrees for this repo");
    }

    let selected: Vec<String> = if branches.is_empty() {
        let labels: Vec<String> = worktrees
            .iter()
            .filter_map(|wt| wt.branch.clone())
            .collect();
        picker::multi_select_none_checked("Remove worktrees:", labels)?
    } else {
        branches
    };
    if selected.is_empty() {
        bail!("nothing selected");
    }

    let mut targets = Vec::new();
    for branch in &selected {
        match worktrees
            .iter()
            .find(|wt| wt.branch.as_deref() == Some(branch.as_str()))
        {
            Some(wt) => targets.push(wt.clone()),
            None => bail!("no bonsai worktree for branch '{branch}'"),
        }
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|d| d.canonicalize().ok());
    let mut cd_home = false;
    for wt in &targets {
        let canonical = wt.path.canonicalize().unwrap_or_else(|_| wt.path.clone());
        remove_worktree(&repo, config, wt, force)?;
        if cwd.as_ref().is_some_and(|c| c.starts_with(&canonical)) {
            cd_home = true;
        }
        if delete_branch && let Some(branch) = &wt.branch {
            let flag = if force { "-D" } else { "-d" };
            repo.git.run(&["branch", flag, branch])?;
            eprintln!("bonsai: deleted branch '{branch}'");
        }
    }

    if cd_home {
        eprintln!("bonsai: current directory was removed, returning to the repo root");
        Ok(Some(repo.main_root.clone()))
    } else {
        Ok(None)
    }
}

pub fn remove_worktree(repo: &Repo, config: &Config, wt: &Worktree, force: bool) -> Result<()> {
    if wt.is_locked && !force {
        bail!(
            "worktree {} is locked; run 'git worktree unlock {}' first",
            wt.path.display(),
            wt.path.display()
        );
    }
    let path = wt.path.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    if let Err(e) = repo.git.run(&args) {
        if e.stderr.contains("contains modified or untracked files") {
            bail!(
                "worktree {} has uncommitted changes; use --force to discard them",
                wt.path.display()
            );
        }
        return Err(e.into());
    }
    eprintln!("bonsai: removed worktree {}", wt.path.display());
    if let Some(parent) = wt.path.parent() {
        cleanup_empty_dirs(parent, &config.root_dir());
    }
    Ok(())
}
