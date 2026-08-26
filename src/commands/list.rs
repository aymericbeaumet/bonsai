use std::path::PathBuf;

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
    let mut entries = Vec::new();
    for wt in repo.worktrees()? {
        if wt.is_bare {
            continue;
        }
        entries.push(Entry {
            main: !wt.path.starts_with(&bonsai_dir),
            dirty: status.then(|| is_dirty(&wt.path)),
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

fn is_dirty(path: &std::path::Path) -> bool {
    Git::at(path)
        .out(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}
