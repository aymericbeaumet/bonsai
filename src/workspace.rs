use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::config::Config;
use crate::repo::Repo;
use crate::worktree::cleanup_empty_dirs;

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
    let mut worktrees = repo.bonsai_worktrees(config)?;

    if worktrees.is_empty() {
        if file.exists() {
            std::fs::remove_file(&file)?;
            cleanup_empty_dirs(&dir, &config.root_dir());
        }
        return Ok(file);
    }

    worktrees.sort_by(|a, b| a.branch.cmp(&b.branch));
    let main_name = repo
        .main_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "main".to_string());
    let mut folders = vec![json!({
        "name": format!("{main_name} (main)"),
        "path": repo.main_root,
    })];
    for wt in &worktrees {
        // Paths relative to the workspace file where possible.
        let path = wt
            .path
            .strip_prefix(&dir)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| wt.path.clone());
        folders.push(json!({
            "name": wt.branch.as_deref().unwrap_or("(detached)"),
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

/// Best-effort sync after a mutating command; failure to write an editor
/// convenience file must never fail the command itself.
pub fn sync_quietly(repo: &Repo, config: &Config) {
    if !config.workspace {
        return;
    }
    if let Err(e) = sync(repo, config) {
        eprintln!("bonsai: could not update workspace file: {e:#}");
    }
}
