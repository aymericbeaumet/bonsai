use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::parallel;
use crate::picker;
use crate::repo::Repo;
use crate::scope::Scope;
use crate::worktree::{cleanup_empty_dirs, find_worktree_dirs_checked};

pub struct Options {
    pub dry_run: bool,
    pub yes: bool,
    pub json: bool,
    pub reviewed_plan: Option<PathBuf>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Report {
    dry_run: bool,
    /// Registrations whose directory is gone.
    stale: Vec<PathBuf>,
    /// Directories under the root that no repository registers.
    orphans: Vec<PathBuf>,
    deleted: Vec<PathBuf>,
}

pub fn run(config: &Config, scope: &Scope, options: Options) -> Result<()> {
    run_selected(config, scope, &[], options)
}

pub fn run_selected(
    config: &Config,
    scope: &Scope,
    targets: &[String],
    options: Options,
) -> Result<()> {
    let mut report = Report {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let outcome = run_inner(config, scope, targets, &options, &mut report);
    if options.json && outcome.is_ok() {
        crate::output::line(format_args!("{}", serde_json::to_string_pretty(&report)?))?;
    }
    outcome
}

fn run_inner(
    config: &Config,
    scope: &Scope,
    targets: &[String],
    options: &Options,
    report: &mut Report,
) -> Result<()> {
    let Options { dry_run, yes, .. } = *options;
    let root = config.root_dir();
    let all = scope.is_all();
    let _root_lock = crate::paths::lock_root(&root, all)?;
    let _locks = scope
        .projects()
        .iter()
        .map(|project| project.repo.lock_mutations())
        .collect::<Result<Vec<_>>>()?;
    let selection = resolve_selection(scope, &root, targets).map_err(|error| {
        if options.reviewed_plan.is_some() {
            error.context("prune plan changed; review again")
        } else {
            error
        }
    })?;
    let mut registrations = Vec::new();
    for project in scope.projects() {
        let stale = missing_registrations(&project.repo, selection.as_ref())?;
        report.stale.extend(stale.iter().cloned());
        registrations.push((project, stale));
    }

    let orphans: Vec<PathBuf> = if all {
        // Whole-root sweep: a checkout whose `.git` file points at a git dir
        // that no longer exists (the main clone was deleted) is unrecoverable
        // from the git side and can only be found here.
        let paths = find_worktree_dirs_checked(&root)?;
        let mut orphans = Vec::new();
        for path in paths {
            if selected(&path, selection.as_ref()) && deletable_orphan(&path)? {
                orphans.push(path);
            }
        }
        orphans
    } else {
        let Scope::Current(project) = scope else {
            unreachable!("a single project is in scope")
        };
        let repo = &project.repo;
        let config = &project.config;
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
            if selected(&path, selection.as_ref())
                && !registered.contains(&canonical)
                && deletable_orphan(&path)?
            {
                orphans.push(path);
            }
        }
        orphans
    };

    report.orphans = orphans.clone();
    if let Some(path) = &options.reviewed_plan {
        let expected: Report = serde_json::from_slice(
            &std::fs::read(path).context("cannot read the reviewed prune plan")?,
        )
        .context("cannot parse the reviewed prune plan")?;
        let paths = |paths: &[PathBuf]| paths.iter().cloned().collect::<HashSet<_>>();
        anyhow::ensure!(
            paths(&expected.stale) == paths(&report.stale)
                && paths(&expected.orphans) == paths(&report.orphans),
            "prune plan changed; review again"
        );
    }
    if dry_run {
        for (project, stale) in &registrations {
            let tag = if all {
                format!("[{}] ", project.id())
            } else {
                String::new()
            };
            report_pruned(&tag, stale, true);
        }
        if !orphans.is_empty() {
            eprintln!("bonsai: orphaned directories (not registered as worktrees):");
            for (index, path) in orphans.iter().enumerate() {
                eprintln!("  [{}/{}] {}", index + 1, orphans.len(), path.display());
            }
        }
        eprintln!("bonsai: dry run, nothing pruned or deleted");
        return Ok(());
    }
    let snapshots = orphans
        .iter()
        .map(std::fs::symlink_metadata)
        .collect::<std::io::Result<Vec<_>>>()?;
    for (project, stale) in &registrations {
        prune_registrations(&project.repo, stale)?;
        let tag = if all {
            format!("[{}] ", project.id())
        } else {
            String::new()
        };
        report_pruned(&tag, stale, false);
    }
    if !orphans.is_empty() {
        eprintln!("bonsai: orphaned directories (not registered as worktrees):");
        for (index, path) in orphans.iter().enumerate() {
            eprintln!("  [{}/{}] {}", index + 1, orphans.len(), path.display());
        }
        // They may contain uncommitted work, hence the confirmation.
        if !yes && !picker::is_interactive() {
            bail!("cannot confirm without a terminal; pass --yes to delete them");
        }
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
                    deletable_orphan(path)?,
                    "orphan metadata changed since selection: {}",
                    path.display()
                );
                crate::ignored::remove_tree(path)?;
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
                        report.deleted.push(path.clone());
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

    match scope {
        Scope::Current(project) => crate::workspace::sync_quietly(&project.repo, &project.config),
        Scope::All(projects) => {
            for project in projects.iter().filter(|project| project.config.workspace) {
                if let Err(error) = crate::workspace::sync(&project.repo, &project.config) {
                    eprintln!(
                        "bonsai: [workspace:{}] could not update: {error:#}",
                        project.id()
                    );
                }
            }
            crate::workspace::sync_global_quietly(config);
        }
    }
    if selection.is_none() {
        remove_empty_tree(&root);
    }
    eprintln!("bonsai: done");
    Ok(())
}

fn missing_registrations(
    repo: &Repo,
    selection: Option<&HashSet<PathBuf>>,
) -> Result<Vec<PathBuf>> {
    Ok(repo
        .linked_worktrees()?
        .into_iter()
        .filter(|worktree| worktree.is_missing() && selected(&worktree.path, selection))
        .map(|worktree| worktree.path)
        .collect())
}

fn prune_registrations(repo: &Repo, stale: &[PathBuf]) -> Result<()> {
    for path in stale {
        let worktree = repo.validate_linked_worktree(path)?;
        anyhow::ensure!(
            worktree.is_missing(),
            "worktree is no longer missing: {}; retry",
            path.display()
        );
        repo.git
            .run(&["worktree", "remove", &path.to_string_lossy()])?;
    }
    Ok(())
}

fn selected(path: &Path, selection: Option<&HashSet<PathBuf>>) -> bool {
    selection.is_none_or(|paths| paths.contains(&crate::paths::canonicalize_lenient(path)))
}

fn resolve_selection(
    scope: &Scope,
    root: &Path,
    targets: &[String],
) -> Result<Option<HashSet<PathBuf>>> {
    if targets.is_empty() {
        return Ok(None);
    }
    let mut worktrees = Vec::new();
    for project in scope.projects() {
        worktrees.extend(project.repo.linked_worktrees()?);
    }
    let mut selection = HashSet::new();
    for wanted in targets {
        let path = crate::paths::canonicalize_lenient(&std::path::absolute(wanted)?);
        let mut matched = false;
        for worktree in &worktrees {
            let checkout = crate::paths::canonicalize_lenient(&worktree.path);
            if worktree.branch.as_deref() == Some(wanted) || checkout == path {
                selection.insert(checkout);
                matched = true;
            }
        }
        if !matched
            && path.starts_with(root)
            && path.join(".git").is_file()
            && crate::paths::ensure_contained(&path, root).is_ok()
            && is_orphan(&path)?
        {
            selection.insert(path);
            matched = true;
        }
        anyhow::ensure!(matched, "no linked worktree or orphan for '{wanted}'");
    }
    Ok(Some(selection))
}

fn report_pruned(tag: &str, stale: &[PathBuf], dry_run: bool) {
    let count = stale.len();
    if count > 0 {
        let plural = if count == 1 { "" } else { "s" };
        let verb = if dry_run { "would prune" } else { "pruned" };
        eprintln!("bonsai: {tag}{verb} {count} stale worktree registration{plural}:");
        for path in stale {
            eprintln!("  {}", path.display());
        }
    }
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

fn deletable_orphan(worktree: &Path) -> Result<bool> {
    if !is_orphan(worktree)? {
        return Ok(false);
    }
    if let Some(nested) = nested_checkout(worktree)? {
        eprintln!(
            "bonsai: preserving {}: contains a nested checkout: {}",
            worktree.display(),
            nested.display()
        );
        return Ok(false);
    }
    Ok(true)
}

/// An orphan can still contain independent work from a surviving repository.
/// Preserve any nested `.git` metadata, including standalone repositories,
/// without following directory links or assuming the project is in scope.
fn nested_checkout(root: &Path) -> Result<Option<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            match std::fs::symlink_metadata(path.join(".git")) {
                Ok(_) => return Ok(Some(path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    pending.push(path);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(None)
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
