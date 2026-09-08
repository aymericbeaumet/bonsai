use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::commands::remove::{RemovalFailure, delete_branch_checked, preflight, remove_worktree};
use crate::config::Config;
use crate::git::Git;
use crate::parallel;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::Worktree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Merged,
    SquashMerged,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Merged => "merged",
            Reason::SquashMerged => "squash-merged",
        }
    }
}

/// Facts about a branch, gathered up front so the decision itself is pure.
#[derive(Debug, Default, Clone, Copy)]
pub struct BranchFacts {
    pub merged: bool,
    pub upstream_gone: bool,
    pub upstream_ahead: bool,
    pub squash_merged: bool,
}

/// Unpushed work always wins: a branch ahead of a live upstream is never
/// cleaned, whatever else matches.
pub fn classify(facts: BranchFacts) -> Option<Reason> {
    if facts.upstream_ahead && !facts.upstream_gone {
        return None;
    }
    if facts.merged {
        Some(Reason::Merged)
    } else if facts.squash_merged {
        Some(Reason::SquashMerged)
    } else {
        None
    }
}

#[derive(serde::Serialize)]
struct JsonEntry {
    branch: String,
    reason: &'static str,
    path: PathBuf,
}

#[derive(Default, serde::Serialize)]
struct JsonReport {
    dry_run: bool,
    planned: Vec<JsonEntry>,
    skipped_dirty: Vec<JsonEntry>,
    removed: Vec<String>,
    deleted_branches: Vec<String>,
    failures: Vec<String>,
    recovery: Option<PathBuf>,
}

pub fn run(
    config: &Config,
    repo: &Repo,
    dry_run: bool,
    yes: bool,
    no_fetch: bool,
    json: bool,
) -> Result<Option<PathBuf>> {
    let mut report = JsonReport {
        dry_run,
        ..Default::default()
    };
    let outcome = run_inner(config, repo, dry_run, yes, no_fetch, &mut report);
    if let Ok(recovery) = &outcome {
        report.recovery = recovery.clone();
    }
    if let Err(error) = &outcome {
        report.failures.push(format!("{error:#}"));
        if let Some(failure) = error.downcast_ref::<RemovalFailure>() {
            report.recovery = failure.recovery.clone();
        }
    }
    if json {
        crate::output::line(format_args!("{}", serde_json::to_string_pretty(&report)?))?;
    }
    outcome
}

fn run_inner(
    config: &Config,
    repo: &Repo,
    dry_run: bool,
    yes: bool,
    no_fetch: bool,
    report: &mut JsonReport,
) -> Result<Option<PathBuf>> {
    let _root_lock = crate::paths::lock_root(&config.root_dir(), false)?;
    let _lock = repo.lock_mutations()?;
    let remote = repo.remote_name(config);

    // The gone-upstream check is only as good as the local remote refs, so
    // clean fetches by default.
    if !no_fetch
        && config.clean.fetch
        && let Some(remote) = &remote
    {
        repo.git.interactive(&["fetch", "--prune", remote])?;
    }

    let default = repo.default_branch(config)?;
    let target_ref = match &remote {
        Some(remote)
            if repo.git.ok(&[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{remote}/{default}"),
            ]) =>
        {
            format!("{remote}/{default}")
        }
        _ => default.clone(),
    };
    let target = repo.git.out(&["rev-parse", "--verify", &target_ref])?;

    let merged: HashSet<String> = repo
        .git
        .out(&["branch", "--merged", &target, "--format=%(objectname)"])?
        .lines()
        .map(str::to_string)
        .collect();
    let tracking: HashMap<String, String> = repo
        .git
        .out(&[
            "for-each-ref",
            "--format=%(refname:short)\u{9}%(upstream:track)",
            "refs/heads",
        ])?
        .lines()
        .filter_map(|l| {
            let (branch, track) = l.split_once('\t')?;
            Some((branch.to_string(), track.to_string()))
        })
        .collect();

    let protected = config
        .clean
        .protected
        .iter()
        .map(|pattern| {
            glob::Pattern::new(pattern)
                .with_context(|| format!("invalid protected branch glob '{pattern}'"))
        })
        .collect::<Result<Vec<_>>>()?;

    let worktrees = repo.bonsai_worktrees(config)?;
    let workers = parallel::worker_count(worktrees.len());
    if workers > 1 {
        eprintln!(
            "bonsai: analyzing {} worktrees in parallel ({workers} jobs)",
            worktrees.len(),
        );
    }
    let checks = parallel::map_ordered(&worktrees, |wt| {
        let branch = wt.branch.clone()?;
        if branch == default
            || wt.is_locked
            || protected.iter().any(|pattern| pattern.matches(&branch))
        {
            return None;
        }
        let track = tracking.get(&branch).map(String::as_str).unwrap_or("");
        let mut facts = BranchFacts {
            merged: wt.head.as_ref().is_some_and(|head| merged.contains(head)),
            upstream_gone: track.contains("[gone]"),
            upstream_ahead: track.contains("ahead"),
            ..Default::default()
        };
        if classify(facts).is_none() && (!facts.upstream_ahead || facts.upstream_gone) {
            facts.squash_merged = is_squash_merged(&repo.git, &target, wt.head.as_deref()?);
        }
        let reason = classify(facts)?;
        Some((wt.clone(), branch, reason, is_dirty(wt)))
    });

    let mut candidates: Vec<(Worktree, String, Reason)> = Vec::new();
    for (wt, branch, reason, dirty) in checks.into_iter().flatten() {
        if dirty {
            eprintln!(
                "bonsai: [{branch}] skipped ({}): uncommitted changes",
                reason.as_str()
            );
            report.skipped_dirty.push(JsonEntry {
                branch,
                reason: reason.as_str(),
                path: wt.path,
            });
            continue;
        }
        candidates.push((wt, branch, reason));
    }
    report.planned = candidates
        .iter()
        .map(|(wt, branch, reason)| JsonEntry {
            branch: branch.clone(),
            reason: reason.as_str(),
            path: wt.path.clone(),
        })
        .collect();

    if candidates.is_empty() {
        eprintln!("bonsai: nothing to clean");
        return Ok(None);
    }

    eprintln!("bonsai: worktrees to remove (branches deleted too):");
    let branch_width = candidates
        .iter()
        .map(|(_, branch, _)| branch.chars().count())
        .max()
        .unwrap_or(0);
    let reason_width = candidates
        .iter()
        .map(|(_, _, reason)| reason.as_str().chars().count())
        .max()
        .unwrap_or(0);
    for (wt, branch, reason) in &candidates {
        let branch_padding = " ".repeat(branch_width.saturating_sub(branch.chars().count()));
        let reason = reason.as_str();
        let reason_padding = " ".repeat(reason_width.saturating_sub(reason.chars().count()));
        eprintln!(
            "  [{branch}]{branch_padding} {reason}{reason_padding}  {}",
            wt.path.display()
        );
    }
    if dry_run {
        eprintln!("bonsai: dry run, nothing removed");
        return Ok(None);
    }
    if !yes {
        let labels: Vec<String> = candidates.iter().map(|(_, b, _)| b.clone()).collect();
        let picked = picker::multi_select_all_checked("Confirm removal:", labels)?;
        candidates.retain(|(_, b, _)| picked.contains(b));
        if candidates.is_empty() {
            eprintln!("bonsai: nothing selected");
            return Ok(None);
        }
    }

    // Remove the worktree we are standing in last, then send the shell home.
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|d| crate::paths::canonicalize_ok(&d));
    let inside = |wt: &Worktree| {
        let canonical = crate::paths::canonicalize_or_self(&wt.path);
        cwd.as_ref().is_some_and(|c| c.starts_with(&canonical))
    };
    candidates.sort_by_key(|(wt, _, _)| inside(wt));

    if candidates.len() > 1 {
        eprintln!(
            "bonsai: removing {} worktrees safely in sequence",
            candidates.len()
        );
    }
    let mut cd_home = false;
    let outcome = (|| -> Result<()> {
        anyhow::ensure!(
            repo.git.out(&["rev-parse", "--verify", &target_ref])? == target,
            "integration target changed since selection; retry"
        );
        for (wt, _, _) in &candidates {
            preflight(repo, config, wt, false)?;
        }
        for (wt, branch, _) in &candidates {
            preflight(repo, config, wt, false)?;
            let removed = remove_worktree(repo, config, wt, false);
            let path_removed = removed.is_ok() || wt.path.try_exists().is_ok_and(|exists| !exists);
            cd_home |= path_removed && inside(wt);
            if path_removed {
                report.removed.push(branch.clone());
            }
            removed?;
            delete_branch_checked(repo, wt)?;
            report.deleted_branches.push(branch.clone());
            eprintln!("bonsai: [{branch}] deleted branch");
        }
        Ok(())
    })();
    crate::workspace::sync_quietly(repo, config);
    if let Err(error) = outcome {
        return Err(RemovalFailure {
            recovery: cd_home.then(|| repo.main_root.clone()),
            completed: report.removed.clone(),
            error,
        }
        .into());
    }

    if cd_home {
        eprintln!("bonsai: current directory was removed, returning to the repo root");
        Ok(Some(repo.main_root.clone()))
    } else {
        Ok(None)
    }
}

/// Squash-merge detection: synthesize a commit holding the branch's whole
/// diff since the merge-base, then ask `git cherry` whether an equivalent
/// change already exists in the target.
fn is_squash_merged(git: &Git, target: &str, branch: &str) -> bool {
    let Ok(base) = git.out(&["merge-base", target, branch]) else {
        return false;
    };
    let Ok(tree) = git.out(&["rev-parse", &format!("{branch}^{{tree}}")]) else {
        return false;
    };
    let Ok(synth) = git.out(&["commit-tree", &tree, "-p", &base, "-m", "_"]) else {
        return false;
    };
    match git.out(&["cherry", target, &synth]) {
        Ok(out) => out.is_empty() || out.lines().all(|l| l.starts_with('-')),
        Err(_) => false,
    }
}

fn is_dirty(wt: &Worktree) -> bool {
    Git::at(&wt.path)
        .out(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let f = BranchFacts::default;
        assert_eq!(classify(f()), None);
        assert_eq!(
            classify(BranchFacts {
                merged: true,
                ..f()
            }),
            Some(Reason::Merged)
        );
        assert_eq!(
            classify(BranchFacts {
                upstream_gone: true,
                ..f()
            }),
            None
        );
        assert_eq!(
            classify(BranchFacts {
                squash_merged: true,
                ..f()
            }),
            Some(Reason::SquashMerged)
        );
        // Unpushed commits protect the branch, even when it looks merged.
        assert_eq!(
            classify(BranchFacts {
                merged: true,
                upstream_ahead: true,
                ..f()
            }),
            None
        );
        // A missing upstream never proves that local commits were integrated.
        assert_eq!(
            classify(BranchFacts {
                upstream_gone: true,
                upstream_ahead: true,
                ..f()
            }),
            None
        );
    }
}
