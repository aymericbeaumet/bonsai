use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::config::Config;
use crate::git::Git;
use crate::repo::{Repo, WorktreeKind};
use crate::worktree::{cleanup_empty_dirs, find_worktree_dirs, repo_id_of};

/// Multi-root VS Code workspace file understood by VS Code, Cursor,
/// Windsurf, and other derivatives: one entry for the main checkout plus one
/// per worktree, labelled by branch. Kept in sync by add/remove/clean/prune
/// so `code "$(bonsai workspace)"` always shows the current worktrees.
pub fn file_path(repo: &Repo, config: &Config) -> PathBuf {
    let dir = repo.bonsai_dir(config);
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "bonsai".to_string());
    dir.join(format!("{name}.code-workspace"))
}

/// Rewrite the workspace file to match the current worktrees, or delete it
/// (and now-empty directories) when none remain.
pub fn sync(repo: &Repo, config: &Config) -> Result<PathBuf> {
    let dir = repo.bonsai_dir(config);
    let file = file_path(repo, config);
    let project_worktrees = repo.project_worktrees(config)?;
    let main_name = project_worktrees
        .iter()
        .find(|entry| entry.kind == WorktreeKind::Main)
        .map(|entry| entry.label())
        .unwrap_or_else(|| "(detached) (root)".to_string());
    let mut worktrees = project_worktrees
        .into_iter()
        .filter(|entry| entry.kind != WorktreeKind::Main && !entry.worktree.is_bare)
        .collect::<Vec<_>>();

    if worktrees.is_empty() {
        if file.exists() {
            std::fs::remove_file(&file)?;
            cleanup_empty_dirs(&dir, &config.root_dir());
        }
        return Ok(file);
    }

    worktrees.sort_by(|a, b| a.worktree.branch.cmp(&b.worktree.branch));
    let mut folders = vec![json!({
        "name": main_name,
        "path": repo.main_root,
    })];
    for entry in &worktrees {
        let wt = &entry.worktree;
        // Paths relative to the workspace file where possible.
        let path = wt
            .path
            .strip_prefix(&dir)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| wt.path.clone());
        folders.push(json!({
            "name": entry.label(),
            "path": path,
        }));
    }

    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        &file,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({"folders": folders}))?
        ),
    )?;
    Ok(file)
}

/// The global workspace file: every worktree of every repo under the bonsai
/// root, at `<root>/bonsai.code-workspace`.
pub fn global_file_path(config: &Config) -> PathBuf {
    config.root_dir().join("bonsai.code-workspace")
}

/// Rewrite the global workspace file from the on-disk layout (no repo
/// context needed), or delete it when no worktrees remain anywhere.
pub fn sync_global(config: &Config) -> Result<PathBuf> {
    let root = config.root_dir();
    let file = global_file_path(config);
    let dirs = find_worktree_dirs(&root);

    if dirs.is_empty() {
        if file.exists() {
            std::fs::remove_file(&file)?;
        }
        return Ok(file);
    }

    // (repo-id, branch label, relative path), sorted for a stable file.
    let mut entries: Vec<(String, String, PathBuf)> = dirs
        .into_iter()
        .map(|path| {
            let branch = Git::at(&path)
                .out(&["branch", "--show-current"])
                .ok()
                .filter(|b| !b.is_empty());
            let repo_id = repo_id_of(&root, &path, branch.as_deref())
                .unwrap_or_else(|| "(unknown)".to_string());
            let rel = path
                .strip_prefix(&root)
                .map(|p| p.to_path_buf())
                .unwrap_or(path);
            (
                repo_id,
                branch.unwrap_or_else(|| "(detached)".to_string()),
                rel,
            )
        })
        .collect();
    entries.sort();

    // Label with the repo's short name, falling back to the full repo-id
    // when two repos share one.
    let short = |id: &str| id.rsplit('/').next().unwrap_or(id).to_string();
    let mut name_owners = std::collections::HashMap::new();
    for (id, _, _) in &entries {
        name_owners
            .entry(short(id))
            .or_insert_with(std::collections::HashSet::new)
            .insert(id.clone());
    }
    let folders: Vec<_> = entries
        .iter()
        .map(|(id, branch, rel)| {
            let repo_label = if name_owners[&short(id)].len() > 1 {
                id.clone()
            } else {
                short(id)
            };
            json!({"name": format!("{repo_label} \u{00b7} {branch}"), "path": rel})
        })
        .collect();

    std::fs::write(
        &file,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({"folders": folders}))?
        ),
    )?;
    Ok(file)
}

/// Best-effort sync after a mutating command; failure to write an editor
/// convenience file must never fail the command itself.
pub fn sync_quietly(repo: &Repo, config: &Config) {
    if !config.workspace {
        return;
    }
    if let Err(e) = sync(repo, config) {
        eprintln!("bonsai: could not update workspace file: {e:#}");
    }
    if let Err(e) = sync_global(config) {
        eprintln!("bonsai: could not update global workspace file: {e:#}");
    }
}

/// Global variant for commands without a repo context (prune --all).
pub fn sync_global_quietly(config: &Config) {
    if !config.workspace {
        return;
    }
    if let Err(e) = sync_global(config) {
        eprintln!("bonsai: could not update global workspace file: {e:#}");
    }
}
