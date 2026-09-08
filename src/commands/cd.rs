use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::parallel;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::{current_branch, find_worktree_dirs, last_activity};

struct Candidate {
    label: String,
    branch: Option<String>,
    path: PathBuf,
    last_change: Option<SystemTime>,
}

pub fn run(config: &Config, repo: Option<&Repo>, query: Option<String>) -> Result<Option<PathBuf>> {
    let mut candidates = candidates(config, repo)?;
    if candidates.is_empty() {
        bail!("no worktrees found");
    }
    if let Some(query) = &query {
        // Exact label/branch match wins, then a unique substring match;
        // anything ambiguous falls through to the picker pre-filtered.
        if let Some(c) = candidates.iter().find(|c| {
            c.label == *query || c.branch.as_deref().is_some_and(|branch| branch == query)
        }) {
            return Ok(Some(c.path.clone()));
        }
        let query = query.to_lowercase();
        let matching: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.label.to_lowercase().contains(&query))
            .collect();
        if matching.len() == 1 {
            return Ok(Some(matching[0].path.clone()));
        }
    }

    let activity = parallel::map_ordered(&candidates, |candidate| last_activity(&candidate.path));
    for (candidate, last_change) in candidates.iter_mut().zip(activity) {
        candidate.last_change = last_change;
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.last_change));
    let options = styled_options(&candidates);
    let picked = picker::select_styled("Worktree:", options, query.as_deref())?;
    Ok(Some(candidates.swap_remove(picked).path))
}

/// Inside a repo: every worktree registered with Git, including worktrees
/// created outside Bonsai. Outside: every worktree under the Bonsai root,
/// labelled by repo.
fn candidates(config: &Config, repo: Option<&Repo>) -> Result<Vec<Candidate>> {
    if let Some(repo) = repo {
        let mut out = Vec::new();
        for entry in repo.project_worktrees(config)? {
            let label = entry.label();
            let wt = entry.worktree;
            if wt.is_bare {
                continue;
            }
            out.push(Candidate {
                label,
                branch: wt.branch,
                last_change: None,
                path: wt.path,
            });
        }
        return Ok(out);
    }
    let root = config.root_dir();
    let paths = find_worktree_dirs(&root);
    Ok(parallel::map_ordered(&paths, |path| {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        let branch = current_branch(path);
        let label = match &branch {
            Some(b) => format!("{} \u{2192} {b}", rel.display()),
            None => rel.display().to_string(),
        };
        Candidate {
            label,
            branch,
            last_change: None,
            path: path.clone(),
        }
    }))
}

fn styled_options(candidates: &[Candidate]) -> Vec<picker::StyledOption> {
    let rows = candidates
        .iter()
        .map(|candidate| picker::RecentRow {
            columns: vec![candidate.label.clone()],
            search: candidate.label.clone(),
            last_change: candidate.last_change,
        })
        .collect::<Vec<_>>();
    picker::recent_options(&rows)
}
