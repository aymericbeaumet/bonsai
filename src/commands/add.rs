use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::picker;
use crate::repo::{Repo, dir_collides, validate_branch_name};
use crate::worktree::path_for_branch;

pub fn run(
    config: &Config,
    branch: Option<String>,
    base: Option<String>,
    fetch: bool,
    path_override: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    let repo = Repo::require()?;

    if (fetch || config.add.fetch)
        && let Some(remote) = repo.remote_name(config)
    {
        repo.git.interactive(&["fetch", "--prune", &remote])?;
    }

    let branch = match branch {
        Some(branch) => branch,
        None => picker::text_with_suggestions(
            "Branch:",
            "pick an existing branch or type a new name to create it",
            branch_suggestions(&repo, config)?,
        )?,
    };
    validate_branch_name(&repo.git, &branch)?;

    let worktrees = repo.worktrees()?;
    let bonsai_dir = repo.bonsai_dir(config);

    // Idempotent: adding a branch that already has a bonsai worktree just cds
    // there. A checkout anywhere else (the main worktree, typically) is fatal:
    // git refuses two checkouts of one branch, and so do we.
    if let Some(wt) = worktrees
        .iter()
        .find(|wt| wt.branch.as_deref() == Some(branch.as_str()))
    {
        if wt.path.starts_with(&bonsai_dir) {
            eprintln!(
                "bonsai: '{branch}' already has a worktree at {}",
                wt.path.display()
            );
            return Ok(Some(wt.path.clone()));
        }
        bail!(
            "'{branch}' is checked out at {}; switch branches there or pick another name",
            wt.path.display()
        );
    }

    let path = match path_override {
        Some(p) => std::path::absolute(&p).context("invalid --path")?,
        None => path_for_branch(&bonsai_dir, &branch),
    };
    if let Some(other) = dir_collides(&path, &worktrees, &branch) {
        bail!(
            "path {} collides with the worktree for branch '{other}' (case-insensitive filesystem); use --path to pick another location",
            path.display()
        );
    }
    if path.exists() {
        // A leftover from a crash; only an empty dir is safe to reuse.
        if std::fs::remove_dir(&path).is_err() {
            bail!(
                "stale directory {} is in the way; run 'bonsai prune' first",
                path.display()
            );
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let path_str = path.to_string_lossy().into_owned();
    if repo.git.ok(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ]) {
        repo.git.run(&["worktree", "add", &path_str, &branch])?;
        eprintln!("bonsai: added worktree for '{branch}' at {path_str}");
    } else if let Some(remote_ref) = remote_ref_for(&repo, config, &branch)? {
        repo.git.run(&[
            "worktree",
            "add",
            "--track",
            "-b",
            &branch,
            &path_str,
            &remote_ref,
        ])?;
        eprintln!("bonsai: added worktree for '{branch}' (tracking {remote_ref}) at {path_str}");
    } else {
        let base = resolve_base(&repo, config, base)?;
        repo.git
            .run(&["worktree", "add", "-b", &branch, &path_str, &base])?;
        eprintln!("bonsai: created branch '{branch}' from {base}, worktree at {path_str}");
    }

    copy_files(&repo, config, &path);
    run_post_add(config, &branch, &path);

    Ok(Some(path))
}

/// Branches worth suggesting: locals without a worktree, plus remote branches
/// without a local counterpart.
fn branch_suggestions(repo: &Repo, config: &Config) -> Result<Vec<String>> {
    let checked_out: Vec<String> = repo
        .worktrees()?
        .into_iter()
        .filter_map(|wt| wt.branch)
        .collect();
    let locals: Vec<String> = repo
        .git
        .out(&["for-each-ref", "--format=%(refname:short)", "refs/heads"])?
        .lines()
        .map(str::to_string)
        .collect();
    let mut suggestions: Vec<String> = locals
        .iter()
        .filter(|b| !checked_out.contains(b))
        .cloned()
        .collect();
    if let Some(remote) = repo.remote_name(config) {
        let prefix = format!("{remote}/");
        for r in repo
            .git
            .out(&[
                "for-each-ref",
                "--format=%(refname:short)",
                &format!("refs/remotes/{remote}"),
            ])?
            .lines()
        {
            if let Some(branch) = r.strip_prefix(&prefix)
                && branch != "HEAD"
                && !locals.contains(&branch.to_string())
                && !suggestions.contains(&branch.to_string())
            {
                suggestions.push(branch.to_string());
            }
        }
    }
    suggestions.sort();
    Ok(suggestions)
}

/// Explicit remote-branch resolution, no git DWIM: configured remote first,
/// then a unique match across all remotes.
fn remote_ref_for(repo: &Repo, config: &Config, branch: &str) -> Result<Option<String>> {
    if let Some(remote) = repo.remote_name(config)
        && repo.git.ok(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{branch}"),
        ])
    {
        return Ok(Some(format!("{remote}/{branch}")));
    }
    let matches: Vec<String> = repo
        .git
        .out(&[
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/remotes/*/{branch}"),
        ])?
        .lines()
        .map(str::to_string)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap())),
        _ => bail!(
            "branch '{branch}' exists on several remotes ({}); pass --base to disambiguate",
            matches.join(", ")
        ),
    }
}

fn resolve_base(repo: &Repo, config: &Config, base: Option<String>) -> Result<String> {
    if let Some(base) = base {
        return Ok(base);
    }
    let default = repo.default_branch(config)?;
    if let Some(remote) = repo.remote_name(config)
        && repo.git.ok(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{default}"),
        ])
    {
        return Ok(format!("{remote}/{default}"));
    }
    if repo.git.ok(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{default}"),
    ]) {
        return Ok(default);
    }
    bail!("base ref '{default}' not found; pass --base");
}

/// Copy configured globs (e.g. .env files) from the worktree we are standing
/// in — falling back to the main worktree — into the new one. Best-effort.
fn copy_files(repo: &Repo, config: &Config, dest: &std::path::Path) {
    if config.add.copy.is_empty() {
        return;
    }
    let source = crate::git::Git::new()
        .out(&["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo.main_root.clone());
    for pattern in &config.add.copy {
        let full = format!("{}/{pattern}", source.display());
        let Ok(paths) = glob::glob(&full) else {
            eprintln!("bonsai: invalid copy glob '{pattern}', skipping");
            continue;
        };
        for path in paths.flatten().filter(|p| p.is_file()) {
            let Ok(rel) = path.strip_prefix(&source) else {
                continue;
            };
            let target = dest.join(rel);
            if target.exists() {
                continue;
            }
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::copy(&path, &target) {
                Ok(_) => eprintln!("bonsai: copied {}", rel.display()),
                Err(e) => eprintln!("bonsai: failed to copy {}: {e}", rel.display()),
            }
        }
    }
}

/// Run the post_add hook inside the new worktree. Failure is reported but
/// does not undo the add. Hook stdout goes to our stderr so wrapped stdout
/// capture stays clean.
fn run_post_add(config: &Config, branch: &str, path: &std::path::Path) {
    let Some(hook) = &config.add.post_add else {
        return;
    };
    eprintln!("bonsai: running post_add hook");
    let result = std::process::Command::new("sh")
        .arg("-c")
        .arg(hook)
        .current_dir(path)
        .env("BONSAI_BRANCH", branch)
        .env("BONSAI_WORKTREE", path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output();
    match result {
        Ok(output) => {
            use std::io::Write;
            let _ = std::io::stderr().write_all(&output.stdout);
            if !output.status.success() {
                eprintln!(
                    "bonsai: post_add hook failed (exit {:?})",
                    output.status.code()
                );
            }
        }
        Err(e) => eprintln!("bonsai: post_add hook failed to start: {e}"),
    }
}
