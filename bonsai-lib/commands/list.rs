use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;
use tabwriter::TabWriter;

use crate::config::Config;
use crate::git::Git;
use crate::parallel;
use crate::repo::{Repo, WorktreeKind};
use crate::scope::Scope;
use crate::worktree::{current_branch, find_worktree_dirs, repo_id_of};

#[derive(Serialize)]
struct Entry {
    branch: Option<String>,
    path: PathBuf,
    main: bool,
    external: bool,
    locked: bool,
    prunable: bool,
    /// Repo identifier (`github.com/owner/repo`), for grouping in UIs.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    /// Omitted when not requested, null when Git could not determine status.
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<Option<bool>>,
}

pub fn run(config: &Config, scope: &Scope, status: bool, json: bool) -> Result<()> {
    let mut entries = Vec::new();
    for project in scope.projects() {
        entries.extend(repo_entries(&project.config, &project.repo, status)?);
    }
    if scope.is_all() {
        entries.extend(orphan_entries(config, &entries, status));
    }
    if json {
        crate::output::line(format_args!("{}", serde_json::to_string_pretty(&entries)?))?;
        return Ok(());
    }
    let stdout = io::stdout();
    let mut output = TabWriter::new(stdout.lock()).padding(2);
    for e in entries {
        let mut flags = Vec::new();
        if e.locked {
            flags.push("locked");
        }
        if e.prunable {
            flags.push("prunable");
        }
        if e.dirty == Some(Some(true)) {
            flags.push("dirty");
        } else if e.dirty == Some(None) {
            flags.push("status unknown");
        }
        let kind = if e.main {
            WorktreeKind::Main
        } else if e.external {
            WorktreeKind::External
        } else {
            WorktreeKind::Managed
        };
        let branch = kind.label(e.branch.as_deref());
        if scope.is_all() {
            write!(output, "{}\t", e.repo.as_deref().unwrap_or("-"))?;
        }
        if flags.is_empty() {
            writeln!(output, "{}\t{}", branch, e.path.display())?;
        } else {
            writeln!(
                output,
                "{}\t{}\t{}",
                branch,
                e.path.display(),
                flags.join(",")
            )?;
        }
    }
    output.flush()?;
    Ok(())
}

fn repo_entries(config: &Config, repo: &Repo, status: bool) -> Result<Vec<Entry>> {
    let id = repo.id(config);
    let worktrees = repo
        .project_worktrees(config)?
        .into_iter()
        .filter(|entry| !entry.worktree.is_bare)
        .collect::<Vec<_>>();
    let make_entry = |project_worktree: &crate::repo::ProjectWorktree| {
        let kind = project_worktree.kind;
        let wt = &project_worktree.worktree;
        Entry {
            main: kind == WorktreeKind::Main,
            external: kind == WorktreeKind::External,
            dirty: status.then(|| is_dirty(&wt.path)),
            repo: Some(id.clone()),
            branch: wt.branch.clone(),
            path: wt.path.clone(),
            locked: wt.is_locked,
            prunable: wt.is_prunable,
        }
    };
    if status {
        Ok(parallel::map_ordered(&worktrees, make_entry))
    } else {
        Ok(worktrees.iter().map(make_entry).collect())
    }
}

/// Checkouts under the root that no project registers, typically because
/// their clone was deleted; branch names are read from each checkout.
fn orphan_entries(config: &Config, listed: &[Entry], status: bool) -> Vec<Entry> {
    let root = config.root_dir();
    let listed = listed
        .iter()
        .map(|entry| crate::paths::canonicalize_or_self(&entry.path))
        .collect::<std::collections::HashSet<_>>();
    let paths = find_worktree_dirs(&root)
        .into_iter()
        .filter(|path| !listed.contains(&crate::paths::canonicalize_or_self(path)))
        .collect::<Vec<_>>();
    parallel::map_ordered(&paths, |path| {
        let branch = current_branch(path);
        Entry {
            repo: repo_id_of(&root, path, branch.as_deref()),
            branch,
            main: false,
            external: false,
            locked: false,
            prunable: true,
            dirty: status.then(|| is_dirty(path)),
            path: path.clone(),
        }
    })
}

fn is_dirty(path: &std::path::Path) -> Option<bool> {
    match Git::at(path).out(&["status", "--porcelain"]) {
        Ok(status) => Some(!status.is_empty()),
        Err(error) => {
            eprintln!(
                "bonsai: status unavailable for {}: {error:#}",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_worktree_status_is_unknown() {
        let temporary = tempfile::tempdir().unwrap();
        assert_eq!(super::is_dirty(&temporary.path().join("missing")), None);
    }

    #[test]
    fn unknown_status_is_explicitly_null_in_json() {
        let mut entry = super::Entry {
            branch: None,
            path: "/missing".into(),
            main: false,
            external: false,
            locked: false,
            prunable: false,
            repo: None,
            dirty: Some(None),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("dirty").unwrap().is_null());
        entry.dirty = None;
        assert!(serde_json::to_value(&entry).unwrap().get("dirty").is_none());
    }
}
