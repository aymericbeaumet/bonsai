use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::picker;
use crate::repo::Repo;
use crate::scope::{Project, Scope};
use crate::worktree::{Worktree, cleanup_empty_dirs};

#[derive(Debug, thiserror::Error)]
#[error("{error:#}")]
pub struct RemovalFailure {
    pub recovery: Option<PathBuf>,
    pub completed: Vec<String>,
    #[source]
    pub error: anyhow::Error,
}

pub fn run(
    config: &Config,
    scope: &Scope,
    branches: Vec<String>,
    delete_branch: bool,
    force: bool,
) -> Result<Option<PathBuf>> {
    let _root_lock = crate::paths::lock_root(&config.root_dir(), false)?;
    let projects = scope.projects();
    // Locks are taken before reading so a selection cannot race a mutation;
    // preflight still re-validates every target before it is touched.
    let _locks = projects
        .iter()
        .map(|project| project.repo.lock_mutations())
        .collect::<Result<Vec<_>>>()?;
    let mut candidates = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        for worktree in project.repo.bonsai_worktrees(&project.config)? {
            candidates.push((index, worktree));
        }
    }
    if candidates.is_empty() {
        match scope {
            Scope::Current(_) => bail!("no bonsai worktrees for this repo"),
            Scope::All(_) => bail!("no bonsai worktrees found"),
        }
    }

    let label = |(index, wt): &(usize, Worktree)| {
        let name = wt
            .branch
            .clone()
            .unwrap_or_else(|| wt.path.display().to_string());
        if scope.is_all() {
            format!("{}: {name}", projects[*index].id())
        } else {
            name
        }
    };
    let mut picked = Vec::new();
    if branches.is_empty() {
        let labels: Vec<String> = candidates.iter().map(label).collect();
        let selected = picker::multi_select_none_checked("Remove worktrees:", labels.clone())?;
        if selected.is_empty() {
            bail!("nothing selected");
        }
        let by_label = labels
            .iter()
            .enumerate()
            .map(|(index, label)| (label.as_str(), index))
            .collect::<HashMap<_, _>>();
        picked.extend(
            selected
                .iter()
                .filter_map(|label| by_label.get(label.as_str())),
        );
    } else {
        // A branch selects its worktree in every project in scope.
        for branch in &branches {
            let selected_path = crate::paths::canonicalize_ok(std::path::Path::new(branch));
            let matches = candidates
                .iter()
                .enumerate()
                .filter(|(_, (_, wt))| {
                    wt.branch.as_deref() == Some(branch.as_str())
                        || selected_path.as_ref().is_some_and(|path| {
                            crate::paths::canonicalize_or_self(&wt.path) == *path
                        })
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if matches.is_empty() {
                bail!("no bonsai worktree for branch '{branch}'");
            }
            picked.extend(matches);
        }
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|d| crate::paths::canonicalize_ok(&d));
    let inside = |wt: &Worktree| {
        cwd.as_ref()
            .is_some_and(|cwd| cwd.starts_with(crate::paths::canonicalize_or_self(&wt.path)))
    };
    let mut seen = HashSet::new();
    let mut targets = picked
        .into_iter()
        .map(|index| candidates[index].clone())
        .filter(|(_, wt)| seen.insert(crate::paths::canonicalize_or_self(&wt.path)))
        .collect::<Vec<_>>();
    targets.sort_by_key(|(_, wt)| inside(wt));

    for (index, wt) in &targets {
        let repo = &projects[*index].repo;
        preflight(repo, &projects[*index].config, wt, force)?;
        if delete_branch
            && !force
            && let Some(branch) = wt.branch.as_deref()
        {
            let upstream = repo
                .git
                .out(&["rev-parse", "--verify", &format!("{branch}@{{upstream}}")])
                .unwrap_or_else(|_| "HEAD".to_string());
            if !repo.git.ok(&[
                "merge-base",
                "--is-ancestor",
                wt.head.as_deref().unwrap_or(branch),
                &upstream,
            ]) {
                bail!("branch '{branch}' is not fully merged; use --force to discard it");
            }
        }
    }
    let mut recovery = None;
    let mut completed = Vec::new();
    let mut touched = Vec::new();
    let outcome = (|| -> Result<()> {
        for (index, wt) in &targets {
            let Project { repo, config } = &projects[*index];
            if !touched.contains(index) {
                touched.push(*index);
            }
            let canonical = crate::paths::canonicalize_or_self(&wt.path);
            preflight(repo, config, wt, force)?;
            let removed = remove_worktree(repo, config, wt, force);
            let path_removed = removed.is_ok() || wt.path.try_exists().is_ok_and(|exists| !exists);
            if path_removed && cwd.as_ref().is_some_and(|c| c.starts_with(&canonical)) {
                recovery = Some(repo.main_root.clone());
            }
            if path_removed {
                completed.push(wt.path.display().to_string());
            }
            removed?;
            if delete_branch && let Some(branch) = &wt.branch {
                delete_branch_checked(repo, wt)?;
                eprintln!("bonsai: [{branch}] deleted branch");
            }
        }
        Ok(())
    })();
    for index in touched {
        crate::workspace::sync_quietly(&projects[index].repo, &projects[index].config);
    }
    if let Err(error) = outcome {
        return Err(RemovalFailure {
            recovery,
            completed,
            error,
        }
        .into());
    }

    if recovery.is_some() {
        eprintln!("bonsai: current directory was removed, returning to the repo root");
    }
    Ok(recovery)
}

pub fn preflight(repo: &Repo, config: &Config, wt: &Worktree, force: bool) -> Result<()> {
    crate::paths::ensure_contained(&wt.path, &config.root_dir())?;
    let current = repo
        .worktrees()?
        .into_iter()
        .find(|current| current.path == wt.path)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "worktree registration changed: {}; retry",
                wt.path.display()
            )
        })?;
    if current.head != wt.head || current.branch != wt.branch || current.is_locked != wt.is_locked {
        bail!(
            "worktree changed since selection: {}; retry",
            wt.path.display()
        );
    }
    if wt.is_locked {
        bail!("worktree {} is locked; unlock it first", wt.path.display());
    }
    if let (Some(branch), Some(expected)) = (&wt.branch, &wt.head) {
        let actual = repo
            .git
            .out(&["rev-parse", "--verify", &format!("refs/heads/{branch}")])?;
        if &actual != expected {
            bail!("branch '{branch}' changed since selection; retry");
        }
    }
    if !force
        && !crate::git::Git::at(&wt.path)
            .out(&["status", "--porcelain"])?
            .is_empty()
    {
        bail!(
            "worktree {} has uncommitted changes; use --force to discard them",
            wt.path.display()
        );
    }
    Ok(())
}

pub fn delete_branch_checked(repo: &Repo, wt: &Worktree) -> Result<()> {
    let branch = wt
        .branch
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cannot delete a detached branch"))?;
    let expected = wt
        .head
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("branch '{branch}' has no known tip"))?;
    if repo
        .worktrees()?
        .iter()
        .any(|current| current.branch.as_deref() == Some(branch))
    {
        bail!("branch '{branch}' is checked out again; preserving it");
    }
    repo.git.run(&[
        "update-ref",
        "-d",
        &format!("refs/heads/{branch}"),
        expected,
    ])?;
    // Match `git branch -d`'s cleanup without allowing its unchecked ref deletion.
    let _ = repo
        .git
        .run(&["config", "--remove-section", &format!("branch.{branch}")]);
    Ok(())
}

pub fn remove_worktree(repo: &Repo, config: &Config, wt: &Worktree, force: bool) -> Result<()> {
    crate::paths::ensure_contained(&wt.path, &config.root_dir())?;
    if wt.is_locked && !force {
        bail!(
            "worktree {} is locked; run 'git worktree unlock {}' first",
            wt.path.display(),
            wt.path.display()
        );
    }
    // Windows cannot delete the directory a process has as its cwd; step out
    // to the main checkout first (harmless elsewhere, and the shell is being
    // sent there anyway).
    let canonical = crate::paths::canonicalize_or_self(&wt.path);
    if std::env::current_dir()
        .ok()
        .and_then(|d| crate::paths::canonicalize_ok(&d))
        .is_some_and(|cwd| cwd.starts_with(&canonical))
    {
        std::env::set_current_dir(&repo.main_root)
            .context("could not leave the worktree before removal")?;
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
    let label = wt.branch.as_deref().unwrap_or("detached");
    eprintln!("bonsai: [{label}] removed worktree {}", wt.path.display());
    if let Some(parent) = wt.path.parent() {
        cleanup_empty_dirs(parent, &config.root_dir());
    }
    Ok(())
}
