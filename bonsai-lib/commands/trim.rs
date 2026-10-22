use std::path::PathBuf;

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::Serialize;

use crate::agents;
use crate::config::Config;
use crate::ignored::{self, Entry, Keep, KeepReason, Kept, Plan, format_bytes};
use crate::picker;
use crate::scope::Scope;

pub struct Options {
    pub dry_run: bool,
    pub yes: bool,
    pub json: bool,
}

struct Target {
    label: String,
    branch: Option<String>,
    repo: Option<String>,
    path: PathBuf,
    /// Index of the project, whose configuration supplies the keep rules.
    project: usize,
}

#[derive(Serialize)]
struct JsonWorktree {
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    path: PathBuf,
    bytes: u64,
    entries: Vec<Entry>,
    kept: Vec<Kept>,
}

#[derive(Serialize)]
struct JsonSkipped {
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    path: PathBuf,
}

#[derive(Default, Serialize)]
struct Report {
    dry_run: bool,
    bytes: u64,
    planned: Vec<JsonWorktree>,
    /// Worktrees another coding agent is working in: never trimmed under it.
    skipped_agents: Vec<JsonSkipped>,
    trimmed: Vec<PathBuf>,
    freed: u64,
}

pub fn run(config: &Config, scope: &Scope, selection: Vec<String>, options: Options) -> Result<()> {
    let mut report = Report {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let outcome = run_inner(config, scope, &selection, &options, &mut report);
    if options.json && outcome.is_ok() {
        crate::output::line(format_args!("{}", serde_json::to_string_pretty(&report)?))?;
    }
    outcome
}

fn run_inner(
    config: &Config,
    scope: &Scope,
    selection: &[String],
    options: &Options,
    report: &mut Report,
) -> Result<()> {
    let root = config.root_dir();
    let _root_lock = crate::paths::lock_root(&root, false)?;
    let projects = scope.projects();
    // Removal holds the same locks: a worktree is never trimmed while it is
    // being removed, and the plan stays valid until deletion.
    let _locks = projects
        .iter()
        .map(|project| project.repo.lock_mutations())
        .collect::<Result<Vec<_>>>()?;
    let keeps: Vec<Keep> = projects
        .iter()
        .map(|project| Keep::from_config(&project.config))
        .collect();
    let mut targets = targets(scope, selection)?;

    let running = agents::running();
    targets.retain(|target| {
        let busy = agents::busy(&running, &target.path);
        if busy.is_empty() {
            return true;
        }
        eprintln!(
            "bonsai: [{}] skipped: agent running: {}",
            target.label,
            agents::describe(&busy)
        );
        report.skipped_agents.push(JsonSkipped {
            branch: target.branch.clone(),
            repo: target.repo.clone(),
            path: target.path.clone(),
        });
        false
    });
    if targets.len() > 1 {
        eprintln!(
            "bonsai: measuring ignored files in {} worktrees in parallel",
            targets.len()
        );
    }
    let plans: Vec<Result<Plan>> = targets
        .par_iter()
        .map(|target| {
            crate::paths::ensure_contained(&target.path, &root)?;
            ignored::plan(&target.path, &keeps[target.project])
        })
        .collect();
    let mut planned: Vec<(Target, Plan)> = Vec::new();
    for (target, plan) in targets.into_iter().zip(plans) {
        match plan {
            Ok(plan) if !plan.entries.is_empty() => planned.push((target, plan)),
            Ok(_) => {}
            Err(error) => eprintln!("bonsai: [{}] skipped: {error:#}", target.label),
        }
    }
    report.bytes = planned.iter().map(|(_, plan)| plan.bytes()).sum();
    report.planned = planned
        .iter()
        .map(|(target, plan)| JsonWorktree {
            branch: target.branch.clone(),
            repo: target.repo.clone(),
            path: target.path.clone(),
            bytes: plan.bytes(),
            entries: plan.entries.clone(),
            kept: plan.kept.clone(),
        })
        .collect();
    if planned.is_empty() {
        eprintln!("bonsai: nothing to trim");
        return Ok(());
    }

    print_plan(&planned, options.dry_run);
    if options.dry_run {
        eprintln!("bonsai: dry run, nothing deleted");
        return Ok(());
    }
    if !options.yes {
        if !picker::is_interactive() {
            bail!("cannot confirm without a terminal; pass --yes to delete them");
        }
        let labels: Vec<String> = planned
            .iter()
            .map(|(target, plan)| format!("{}  {}", target.label, format_bytes(plan.bytes())))
            .collect();
        let picked = picker::multi_select_all_checked("Confirm trim:", labels.clone())?;
        let mut index = 0;
        planned.retain(|_| {
            index += 1;
            picked.contains(&labels[index - 1])
        });
        if planned.is_empty() {
            eprintln!("bonsai: nothing selected");
            return Ok(());
        }
    }

    // Every ignored path is independent: delete them all concurrently.
    let work: Vec<(usize, PathBuf)> = planned
        .iter()
        .enumerate()
        .flat_map(|(index, (target, plan))| {
            plan.entries
                .iter()
                .map(move |entry| (index, target.path.join(&entry.path)))
        })
        .collect();
    let results: Vec<Result<()>> = work
        .par_iter()
        .map(|(_, path)| {
            crate::paths::ensure_contained(path, &root)?;
            Ok(ignored::remove_tree(path)?)
        })
        .collect();
    let mut failed = vec![false; planned.len()];
    for ((index, path), result) in work.iter().zip(results) {
        if let Err(error) = result {
            failed[*index] = true;
            let label = &planned[*index].0.label;
            eprintln!(
                "bonsai: [{label}] failed to delete {}: {error:#}",
                path.display()
            );
        }
    }
    for ((target, plan), failed) in planned.iter().zip(&failed) {
        if !failed {
            eprintln!(
                "bonsai: [{}] trimmed {} ({})",
                target.label,
                format_bytes(plan.bytes()),
                count(plan.entries.len(), "path", "paths")
            );
            report.trimmed.push(target.path.clone());
            report.freed += plan.bytes();
        }
    }
    eprintln!("bonsai: reclaimed {}", format_bytes(report.freed));
    let failures = failed.iter().filter(|failed| **failed).count();
    if failures > 0 {
        bail!("failed to trim {failures} worktrees");
    }
    Ok(())
}

/// Bonsai-managed worktrees in scope: every one, or those whose branch or
/// exact path was named (in every project that has one).
fn targets(scope: &Scope, selection: &[String]) -> Result<Vec<Target>> {
    let projects = scope.projects();
    let mut targets = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        let id = scope.is_all().then(|| project.id());
        for worktree in project.repo.bonsai_worktrees(&project.config)? {
            if worktree.is_missing() {
                continue;
            }
            let name = worktree
                .branch
                .clone()
                .unwrap_or_else(|| worktree.path.display().to_string());
            targets.push(Target {
                label: match &id {
                    Some(id) => format!("{id}: {name}"),
                    None => name,
                },
                branch: worktree.branch,
                repo: id.clone(),
                path: worktree.path,
                project: index,
            });
        }
    }
    if selection.is_empty() {
        return Ok(targets);
    }
    let mut selected = Vec::new();
    for wanted in selection {
        let path = crate::paths::canonicalize_ok(std::path::Path::new(wanted));
        let before = selected.len();
        for target in &targets {
            let matches = target.branch.as_deref() == Some(wanted.as_str())
                || path
                    .as_ref()
                    .is_some_and(|path| crate::paths::canonicalize_or_self(&target.path) == *path);
            if matches && !selected.iter().any(|t: &&Target| t.path == target.path) {
                selected.push(target);
            }
        }
        if selected.len() == before {
            bail!("no bonsai worktree for branch '{wanted}'");
        }
    }
    let keep: Vec<PathBuf> = selected.iter().map(|target| target.path.clone()).collect();
    targets.retain(|target| keep.contains(&target.path));
    Ok(targets)
}

/// The plan names the largest paths of each worktree, or every path for a
/// dry run (`--json` always has them all).
fn print_plan(planned: &[(Target, Plan)], every_path: bool) {
    let shown = if every_path { usize::MAX } else { 4 };
    eprintln!("bonsai: ignored files to delete (worktrees, branches, and kept paths stay):");
    let label_width = planned
        .iter()
        .map(|(target, _)| target.label.chars().count())
        .max()
        .unwrap_or(0);
    let sizes: Vec<String> = planned
        .iter()
        .map(|(_, plan)| format_bytes(plan.bytes()))
        .collect();
    let size_width = sizes.iter().map(String::len).max().unwrap_or(0);
    for ((target, plan), size) in planned.iter().zip(&sizes) {
        let mut names: Vec<String> = match plan.entries.as_slice() {
            [only] => vec![only.path.display().to_string()],
            entries => entries
                .iter()
                .take(shown)
                .map(|entry| format!("{} {}", entry.path.display(), format_bytes(entry.bytes)))
                .collect(),
        };
        if plan.entries.len() > shown {
            names.push(format!(
                "+{} more (-n lists all)",
                plan.entries.len() - shown
            ));
        }
        let padding = " ".repeat(label_width.saturating_sub(target.label.chars().count()));
        eprintln!(
            "  [{}]{padding} {size:>size_width$}  {}",
            target.label,
            names.join(", ")
        );
    }
    let total: u64 = planned.iter().map(|(_, plan)| plan.bytes()).sum();
    let kept = |reason| {
        planned
            .iter()
            .flat_map(|(_, plan)| &plan.kept)
            .filter(|kept| kept.reason == reason)
            .count()
    };
    let mut summary = format!(
        "bonsai: {} across {}",
        format_bytes(total),
        count(planned.len(), "worktree", "worktrees")
    );
    let kept: Vec<String> = [
        (KeepReason::Protected, "protected path", "protected paths"),
        (
            KeepReason::Repository,
            "nested repository",
            "nested repositories",
        ),
    ]
    .into_iter()
    .map(|(reason, one, many)| (kept(reason), one, many))
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, one, many)| count(n, one, many))
    .collect();
    if !kept.is_empty() {
        summary.push_str(&format!("; keeping {}", kept.join(" and ")));
    }
    eprintln!("{summary}");
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
