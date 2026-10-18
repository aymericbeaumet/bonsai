use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::commands::remove::{RemovalFailure, delete_branch_checked, preflight, remove_worktree};
use crate::config::Config;
use crate::git::Git;
use crate::parallel;
use crate::picker;
use crate::scope::{Project, Scope};
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
    /// Set when every project is in scope and reports form an array.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    dry_run: bool,
    planned: Vec<JsonEntry>,
    skipped_dirty: Vec<JsonEntry>,
    removed: Vec<String>,
    deleted_branches: Vec<String>,
    failures: Vec<String>,
    recovery: Option<PathBuf>,
}

impl JsonReport {
    fn fail(&mut self, error: &anyhow::Error) {
        self.failures.push(format!("{error:#}"));
        if let Some(failure) = error.downcast_ref::<RemovalFailure>() {
            self.recovery = failure.recovery.clone();
        }
    }
}

/// One project's integrated worktrees, analyzed under its mutation lock.
struct Plan {
    index: usize,
    target_ref: String,
    target: String,
    candidates: Vec<(Worktree, String, Reason)>,
    _lock: std::fs::File,
}

pub fn run(
    config: &Config,
    scope: &Scope,
    dry_run: bool,
    yes: bool,
    no_fetch: bool,
    json: bool,
) -> Result<Option<PathBuf>> {
    let mut reports = scope
        .projects()
        .iter()
        .map(|project| JsonReport {
            repo: scope.is_all().then(|| project.id()),
            dry_run,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let outcome = clean(config, scope, dry_run, yes, no_fetch, &mut reports);
    if let Scope::Current(_) = scope {
        if let Ok(recovery) = &outcome {
            reports[0].recovery = recovery.clone();
        }
        if let Err(error) = &outcome {
            reports[0].fail(error);
        }
    }
    if json {
        let text = match scope {
            Scope::Current(_) => serde_json::to_string_pretty(&reports[0])?,
            Scope::All(_) => serde_json::to_string_pretty(&reports)?,
        };
        crate::output::line(format_args!("{text}"))?;
    }
    outcome
}

/// With every project in scope, a failing project is reported and skipped;
/// otherwise its error is returned as is.
fn clean(
    config: &Config,
    scope: &Scope,
    dry_run: bool,
    yes: bool,
    no_fetch: bool,
    reports: &mut [JsonReport],
) -> Result<Option<PathBuf>> {
    let _root_lock = crate::paths::lock_root(&config.root_dir(), false)?;
    let projects = scope.projects();
    let all = scope.is_all();
    let tag = |index: usize, branch: &str| {
        if all {
            format!("{} {branch}", projects[index].id())
        } else {
            branch.to_string()
        }
    };
    let mut failed = 0;
    let mut plans = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        match plan(index, project, no_fetch, &mut reports[index], &tag) {
            Ok(plan) => plans.push(plan),
            Err(error) if all => {
                eprintln!("bonsai: [{}] skipped: {error:#}", project.id());
                reports[index].fail(&error);
                failed += 1;
            }
            Err(error) => return Err(error),
        }
    }
    plans.retain(|plan| !plan.candidates.is_empty());

    if plans.is_empty() {
        eprintln!("bonsai: nothing to clean");
        return finish(failed, None);
    }

    eprintln!("bonsai: worktrees to remove (branches deleted too):");
    let rows = plans
        .iter()
        .flat_map(|plan| {
            plan.candidates
                .iter()
                .map(|(wt, branch, reason)| (tag(plan.index, branch), reason.as_str(), wt))
        })
        .collect::<Vec<_>>();
    let tag_width = rows
        .iter()
        .map(|(tag, _, _)| tag.chars().count())
        .max()
        .unwrap_or(0);
    let reason_width = rows
        .iter()
        .map(|(_, reason, _)| reason.chars().count())
        .max()
        .unwrap_or(0);
    for (tag, reason, wt) in &rows {
        let tag_padding = " ".repeat(tag_width.saturating_sub(tag.chars().count()));
        let reason_padding = " ".repeat(reason_width.saturating_sub(reason.chars().count()));
        eprintln!(
            "  [{tag}]{tag_padding} {reason}{reason_padding}  {}",
            wt.path.display()
        );
    }
    if dry_run {
        eprintln!("bonsai: dry run, nothing removed");
        return finish(failed, None);
    }
    if !yes {
        let labels: Vec<String> = rows.iter().map(|(tag, _, _)| tag.clone()).collect();
        let picked = picker::multi_select_all_checked("Confirm removal:", labels)?;
        for plan in &mut plans {
            let index = plan.index;
            plan.candidates
                .retain(|(_, branch, _)| picked.contains(&tag(index, branch)));
        }
        plans.retain(|plan| !plan.candidates.is_empty());
        if plans.is_empty() {
            eprintln!("bonsai: nothing selected");
            return finish(failed, None);
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
    for plan in &mut plans {
        plan.candidates.sort_by_key(|(wt, _, _)| inside(wt));
    }
    plans.sort_by_key(|plan| plan.candidates.iter().any(|(wt, _, _)| inside(wt)));

    let total = plans
        .iter()
        .map(|plan| plan.candidates.len())
        .sum::<usize>();
    if total > 1 {
        eprintln!("bonsai: removing {total} worktrees safely in sequence");
    }
    let mut recovery = None;
    for plan in &plans {
        let project = &projects[plan.index];
        let report = &mut reports[plan.index];
        match execute(project, plan, report, &inside) {
            Ok(home) => {
                recovery = recovery.or(home.clone());
                report.recovery = home;
            }
            Err(error) if all => {
                eprintln!("bonsai: [{}] {error:#}", project.id());
                report.fail(&error);
                recovery = recovery.or(report.recovery.clone());
                failed += 1;
            }
            Err(error) => return Err(error),
        }
    }
    if recovery.is_some() {
        eprintln!("bonsai: current directory was removed, returning to the repo root");
    }
    finish(failed, recovery)
}

fn finish(failed: usize, recovery: Option<PathBuf>) -> Result<Option<PathBuf>> {
    if failed == 0 {
        return Ok(recovery);
    }
    let error = anyhow::anyhow!("{failed} project(s) could not be cleaned");
    match recovery {
        Some(recovery) => Err(RemovalFailure {
            recovery: Some(recovery),
            completed: Vec::new(),
            error,
        }
        .into()),
        None => Err(error),
    }
}

fn plan(
    index: usize,
    project: &Project,
    no_fetch: bool,
    report: &mut JsonReport,
    tag: &impl Fn(usize, &str) -> String,
) -> Result<Plan> {
    let Project { repo, config } = project;
    let lock = repo.lock_mutations()?;
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
                "bonsai: [{}] skipped ({}): uncommitted changes",
                tag(index, &branch),
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
    Ok(Plan {
        index,
        target_ref,
        target,
        candidates,
        _lock: lock,
    })
}

/// Remove a project's confirmed worktrees and branches; returns its main
/// checkout when the current directory was removed.
fn execute(
    project: &Project,
    plan: &Plan,
    report: &mut JsonReport,
    inside: &impl Fn(&Worktree) -> bool,
) -> Result<Option<PathBuf>> {
    let Project { repo, config } = project;
    let mut cd_home = false;
    let outcome = (|| -> Result<()> {
        anyhow::ensure!(
            repo.git.out(&["rev-parse", "--verify", &plan.target_ref])? == plan.target,
            "integration target changed since selection; retry"
        );
        for (wt, _, _) in &plan.candidates {
            preflight(repo, config, wt, false)?;
        }
        for (wt, branch, _) in &plan.candidates {
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
            recovery: cd_home.then(|| crate::paths::canonicalize_or_self(&repo.main_root)),
            completed: report.removed.clone(),
            error,
        }
        .into());
    }
    Ok(cd_home.then(|| crate::paths::canonicalize_or_self(&repo.main_root)))
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
