use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::git::Git;
use crate::repo::Repo;
use crate::worktree::find_worktree_dirs;

#[derive(Serialize)]
struct Entry {
    branch: Option<String>,
    path: PathBuf,
    main: bool,
    locked: bool,
    prunable: bool,
    /// Repo identifier (`github.com/owner/repo`), for grouping in UIs.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
}

pub fn run(config: &Config, all: bool, status: bool, json: bool) -> Result<()> {
    let entries = if all {
        global_entries(config, status)?
    } else {
        repo_entries(config, status)?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    for e in entries {
        let mut flags = Vec::new();
        if e.main {
            flags.push("main");
        }
        if e.locked {
            flags.push("locked");
        }
        if e.prunable {
            flags.push("prunable");
        }
        if e.dirty == Some(true) {
            flags.push("dirty");
        }
        println!(
            "{}\t{}\t{}",
            e.branch.as_deref().unwrap_or("(detached)"),
            e.path.display(),
            flags.join(",")
        );
    }
    Ok(())
}

fn repo_entries(config: &Config, status: bool) -> Result<Vec<Entry>> {
    let repo = Repo::require()?;
    let bonsai_dir = repo.bonsai_dir(config);
    let id = repo.id(config);
    let mut entries = Vec::new();
    for wt in repo.worktrees()? {
        if wt.is_bare {
            continue;
        }
        entries.push(Entry {
            main: !wt.path.starts_with(&bonsai_dir),
            dirty: status.then(|| is_dirty(&wt.path)),
            repo: Some(id.clone()),
            branch: wt.branch,
            path: wt.path,
            locked: wt.is_locked,
            prunable: wt.is_prunable,
        });
    }
    Ok(entries)
}

/// Global listing scans the bonsai root on disk (there is no repo context to
/// ask git from); branch names are read from each checkout.
fn global_entries(config: &Config, status: bool) -> Result<Vec<Entry>> {
    let root = config.root_dir();
    let mut entries = Vec::new();
    for path in find_worktree_dirs(&root) {
        let branch = Git::at(&path)
            .out(&["branch", "--show-current"])
            .ok()
            .filter(|b| !b.is_empty());
        entries.push(Entry {
            repo: repo_id_of(&root, &path, branch.as_deref()),
            branch,
            main: false,
            locked: false,
            prunable: false,
            dirty: status.then(|| is_dirty(&path)),
            path,
        });
    }
    Ok(entries)
}

/// The repo-id is the worktree's path relative to the root, minus the branch
/// segments (`<root>/<repo-id>/<branch dirs...>`).
fn repo_id_of(root: &Path, path: &Path, branch: Option<&str>) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let segments: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let branch_segments = branch.map_or(1, |b| b.split('/').count());
    let keep = segments.len().checked_sub(branch_segments)?;
    if keep == 0 {
        return None;
    }
    Some(segments[..keep].join("/"))
}

fn is_dirty(path: &std::path::Path) -> bool {
    Git::at(path)
        .out(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::repo_id_of;
    use std::path::Path;

    #[test]
    fn repo_id_strips_branch_segments() {
        let root = Path::new("/b");
        let cases = [
            (
                "/b/github.com/o/r/feat",
                Some("feat"),
                Some("github.com/o/r"),
            ),
            (
                "/b/github.com/o/r/feature/login",
                Some("feature/login"),
                Some("github.com/o/r"),
            ),
            ("/b/local/x-1234/feat", None, Some("local/x-1234")),
            ("/b/feat", Some("feat"), None),
            ("/elsewhere/feat", Some("feat"), None),
        ];
        for (path, branch, expected) in cases {
            assert_eq!(
                repo_id_of(root, Path::new(path), branch).as_deref(),
                expected,
                "path: {path}"
            );
        }
    }
}
