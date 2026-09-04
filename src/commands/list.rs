use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;
use tabwriter::TabWriter;

use crate::config::Config;
use crate::git::Git;
use crate::repo::{Repo, WorktreeKind};
use crate::worktree::{find_worktree_dirs, repo_id_of};

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
        if e.dirty == Some(true) {
            flags.push("dirty");
        }
        let kind = if e.main {
            WorktreeKind::Main
        } else if e.external {
            WorktreeKind::External
        } else {
            WorktreeKind::Managed
        };
        let branch = kind.label(e.branch.as_deref());
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

fn repo_entries(config: &Config, status: bool) -> Result<Vec<Entry>> {
    let repo = Repo::require()?;
    let id = repo.id(config);
    let mut entries = Vec::new();
    for project_worktree in repo.project_worktrees(config)? {
        let kind = project_worktree.kind;
        let wt = project_worktree.worktree;
        if wt.is_bare {
            continue;
        }
        entries.push(Entry {
            main: kind == WorktreeKind::Main,
            external: kind == WorktreeKind::External,
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
            external: false,
            locked: false,
            prunable: false,
            dirty: status.then(|| is_dirty(&path)),
            path,
        });
    }
    Ok(entries)
}

fn is_dirty(path: &std::path::Path) -> bool {
    Git::at(path)
        .out(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}
