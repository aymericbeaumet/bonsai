use std::io::{self, IsTerminal};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::agents;
use crate::config::Config;
use crate::ignored::{self, Entry, Keep, KeepReason, Kept, Plan, format_bytes};
use crate::picker;
use crate::scope::Scope;

mod progress;

pub struct Options {
    pub dry_run: bool,
    pub yes: bool,
    pub json: bool,
    pub reviewed_plan: Option<PathBuf>,
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

#[derive(Deserialize)]
struct ReviewedReport {
    planned: Vec<ReviewedWorktree>,
}

#[derive(Deserialize, PartialEq, Eq)]
struct ReviewedWorktree {
    path: PathBuf,
    entries: Vec<ReviewedEntry>,
    kept: Vec<ReviewedKept>,
}

#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct ReviewedEntry {
    path: PathBuf,
}

#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct ReviewedKept {
    path: PathBuf,
    reason: String,
}

fn normalize_review(mut planned: Vec<ReviewedWorktree>) -> Vec<ReviewedWorktree> {
    for worktree in &mut planned {
        worktree.entries.sort();
        worktree.kept.sort();
    }
    planned.sort_by(|left, right| left.path.cmp(&right.path));
    planned
}

fn review_roots(planned: &[JsonWorktree]) -> Result<Vec<ReviewedWorktree>> {
    Ok(normalize_review(serde_json::from_value(
        serde_json::to_value(planned)?,
    )?))
}

fn verify_reviewed(path: &Path, planned: &[JsonWorktree]) -> Result<()> {
    let reviewed: ReviewedReport = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read trim review {}", path.display()))?,
    )
    .context("invalid trim reviewed plan")?;
    anyhow::ensure!(
        normalize_review(reviewed.planned) == review_roots(planned)?,
        "trim plan changed; review again"
    );
    Ok(())
}

fn json_worktree(target: &Target, plan: &Plan) -> JsonWorktree {
    JsonWorktree {
        branch: target.branch.clone(),
        repo: target.repo.clone(),
        path: target.path.clone(),
        bytes: plan.bytes(),
        entries: plan.entries.clone(),
        kept: plan.kept.clone(),
    }
}

pub fn run(config: &Config, scope: &Scope, selection: Vec<String>, options: Options) -> Result<()> {
    let mut report = Report {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let outcome = run_inner(config, scope, &selection, &options, &mut report);
    if options.json {
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
            projects[target.project]
                .repo
                .validate_linked_worktree(&target.path)?;
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
        .map(|(target, plan)| json_worktree(target, plan))
        .collect();
    if let Some(reviewed) = &options.reviewed_plan {
        verify_reviewed(reviewed, &report.planned)?;
    }
    if planned.is_empty() {
        eprintln!("bonsai: nothing to trim");
        return Ok(());
    }

    print_plan(&planned, options.dry_run, options.json);
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
    // Recheck after the interactive wait: another agent may have started.
    let running = agents::running();
    for (target, plan) in &planned {
        let current = projects[target.project]
            .repo
            .validate_linked_worktree(&target.path)?;
        anyhow::ensure!(
            !current.is_locked,
            "[{}] worktree was locked during confirmation",
            target.label
        );
        let busy = agents::busy(&running, &target.path);
        if !busy.is_empty() {
            bail!(
                "[{}] agent started during confirmation: {}",
                target.label,
                agents::describe(&busy)
            );
        }
        if options.reviewed_plan.is_some() && !options.yes {
            let fresh = ignored::plan(&target.path, &keeps[target.project])?;
            anyhow::ensure!(
                review_roots(&[json_worktree(target, plan)])?
                    == review_roots(&[json_worktree(target, &fresh)])?,
                "trim plan changed; review again"
            );
        }
    }
    let work: Vec<(usize, &Entry)> = planned
        .iter()
        .enumerate()
        .flat_map(|(index, (_, plan))| plan.entries.iter().map(move |entry| (index, entry)))
        .collect();
    let progress = progress::Progress::new(
        !options.json
            && (io::stderr().is_terminal()
                || std::env::var_os("_BONSAI_PROGRESS").is_some_and(|value| value == "1")),
        work.iter().map(|(_, entry)| entry.items).sum(),
        work.len(),
    );
    let delete = || {
        work.par_iter()
            .map(|(index, entry)| {
                let target = &planned[*index].0;
                let outcome = (|| {
                    validate_entry(&target.path, &entry.path)?;
                    Ok(ignored::remove_tree_with_progress(
                        &target.path.join(&entry.path),
                        &progress.removed,
                    )?)
                })();
                progress.finished.fetch_add(1, Ordering::Relaxed);
                outcome
            })
            .collect::<Vec<Result<()>>>()
    };
    let results = ignored::with_deletion_pool(delete);
    drop(progress);
    let mut failed = vec![false; planned.len()];
    for ((index, entry), result) in work.iter().zip(results) {
        if let Err(error) = result {
            failed[*index] = true;
            let label = &planned[*index].0.label;
            eprintln!(
                "bonsai: [{label}] failed to delete {}: {error:#}",
                entry.path.display()
            );
        } else {
            report.freed += entry.bytes;
        }
    }
    let width = crossterm::terminal::size().map_or(88, |(width, _)| usize::from(width));
    let mut project = None;
    eprintln!("bonsai: trim results:");
    for ((target, plan), failed) in planned.iter().zip(&failed) {
        if !failed {
            if project != Some(target.project) {
                project = Some(target.project);
                if let Some(repo) = &target.repo {
                    eprintln!("{}", fit(repo, width));
                }
            }
            let name = target.branch.as_deref().unwrap_or("detached");
            let row = format!(
                "  {:>7}  {} ({})",
                format_bytes(plan.bytes()),
                name.escape_debug(),
                count(plan.entries.len(), "path", "paths")
            );
            eprintln!("{}", fit(&row, width));
            report.trimmed.push(target.path.clone());
        }
    }
    eprintln!("bonsai: reclaimed {}", format_bytes(report.freed));
    let failures = failed.iter().filter(|failed| **failed).count();
    if failures > 0 {
        bail!("failed to trim {failures} worktrees");
    }
    Ok(())
}

/// Registered linked worktrees in scope: every one, or those whose branch or
/// exact path was named (in every project that has one).
fn targets(scope: &Scope, selection: &[String]) -> Result<Vec<Target>> {
    let projects = scope.projects();
    let mut targets = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        let id = scope.is_all().then(|| project.id());
        for worktree in project.repo.linked_worktrees()? {
            if worktree.is_missing() || worktree.is_locked {
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
            bail!("no linked worktree for branch '{wanted}'");
        }
    }
    let keep: Vec<PathBuf> = selected.iter().map(|target| target.path.clone()).collect();
    targets.retain(|target| keep.contains(&target.path));
    Ok(targets)
}

/// The plan names the largest paths of each worktree, or every path for a
/// dry run (`--json` always has them all).
fn print_plan(planned: &[(Target, Plan)], every_path: bool, json: bool) {
    let width = crossterm::terminal::size().map_or(88, |(width, _)| usize::from(width));
    let color = !json && io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    eprintln!("bonsai: ignored files to delete:");
    for line in plan_lines(planned, every_path, width, color) {
        eprintln!("{line}");
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

fn plan_lines(
    planned: &[(Target, Plan)],
    every_path: bool,
    width: usize,
    color: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut project = None;
    for (target, plan) in planned {
        if project != Some(target.project) {
            project = Some(target.project);
            if let Some(repo) = &target.repo {
                let heading = fit(repo, width);
                lines.push(if color {
                    format!("\x1b[1;36m{heading}\x1b[0m")
                } else {
                    heading
                });
            }
        }
        let name = target
            .branch
            .as_deref()
            .unwrap_or_else(|| target.path.to_str().unwrap_or("detached"));
        let row = fit(
            &format!(
                "  {:>7}  {}",
                format_bytes(plan.bytes()),
                name.escape_debug()
            ),
            width,
        );
        lines.push(if color {
            format!("\x1b[1;32m{row}\x1b[0m")
        } else {
            row
        });
        let shown = if every_path { plan.entries.len() } else { 3 };
        for entry in plan.entries.iter().take(shown) {
            let row = format!(
                "           {}  {}",
                entry.path.display().to_string().escape_debug(),
                format_bytes(entry.bytes)
            );
            // Explicit dry runs retain complete paths for review and copying.
            lines.push(if every_path { row } else { fit(&row, width) });
        }
        if plan.entries.len() > shown {
            lines.push(fit(
                &format!(
                    "           +{} paths (--dry-run lists all)",
                    plan.entries.len() - shown
                ),
                width,
            ));
        }
    }
    lines
}

fn fit(text: &str, width: usize) -> String {
    use ratatui::text::Line;
    if Line::from(text).width() <= width {
        return text.to_string();
    }
    let mut result = String::new();
    for ch in text.chars() {
        if Line::from(format!("{result}{ch}…")).width() > width {
            break;
        }
        result.push(ch);
    }
    if width > 0 {
        result.push('…');
    }
    result
}

/// Check parents while leaving the final symlink unresolved: deletion
/// unlinks that entry, including dangling links and links to shared stores.
fn validate_entry(worktree: &Path, rel: &Path) -> Result<()> {
    anyhow::ensure!(
        !rel.as_os_str().is_empty()
            && rel
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid ignored path: {}",
        rel.display()
    );
    let parent = rel.parent().unwrap_or_else(|| Path::new(""));
    if !parent.as_os_str().is_empty() {
        crate::paths::ensure_contained(&worktree.join(parent), worktree)?;
    }
    Ok(())
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_groups_projects_and_fits_narrow_terminals() {
        let planned: Vec<_> = ["ab/first", "ab/second"]
            .into_iter()
            .map(|branch| {
                (
                    Target {
                        label: branch.into(),
                        branch: Some(branch.into()),
                        repo: Some("github.com/owner/project".into()),
                        path: PathBuf::from("/checkout"),
                        project: 0,
                    },
                    Plan {
                        entries: vec![Entry {
                            path: PathBuf::from("web/node_modules/a-very-long-package-name"),
                            bytes: 4096,
                            items: 2,
                        }],
                        kept: vec![],
                    },
                )
            })
            .collect();
        let lines = plan_lines(&planned, false, 32, false);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("github.com"))
                .count(),
            1
        );
        assert!(
            lines
                .iter()
                .all(|line| ratatui::text::Line::from(line.as_str()).width() <= 32)
        );
        assert!(lines.iter().any(|line| line.ends_with('…')));
        assert!(
            plan_lines(&planned, true, 32, false)
                .iter()
                .any(|line| line.contains("a-very-long-package-name"))
        );
        assert_eq!(fit("界界界", 5), "界界…");
    }

    #[test]
    fn ignored_paths_cannot_escape_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["", ".", "..", "a/../../b", "/outside"] {
            assert!(
                validate_entry(dir.path(), Path::new(path)).is_err(),
                "{path}"
            );
        }
        assert!(validate_entry(dir.path(), Path::new("node_modules")).is_ok());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
            assert!(validate_entry(dir.path(), Path::new("escape/file")).is_err());
            assert!(validate_entry(dir.path(), Path::new("escape")).is_ok());
        }
    }
}
