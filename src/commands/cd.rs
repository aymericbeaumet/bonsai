use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::git::Git;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::{find_worktree_dirs, last_activity};

struct Candidate {
    label: String,
    branch: Option<String>,
    path: PathBuf,
    last_change: Option<SystemTime>,
}

pub fn run(config: &Config, query: Option<String>) -> Result<Option<PathBuf>> {
    let mut candidates = candidates(config)?;
    if candidates.is_empty() {
        bail!("no worktrees found");
    }
    // Most recently worked-in first, so the default pick is the freshest.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.last_change));

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

    let options = styled_options(&candidates);
    let picked = picker::select_styled("Worktree:", options, query.as_deref())?;
    Ok(Some(candidates.swap_remove(picked).path))
}

/// Inside a repo: every worktree registered with Git, including worktrees
/// created outside Bonsai. Outside: every worktree under the Bonsai root,
/// labelled by repo.
fn candidates(config: &Config) -> Result<Vec<Candidate>> {
    if let Some(repo) = Repo::discover()? {
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
                last_change: last_activity(&wt.path),
                path: wt.path,
            });
        }
        return Ok(out);
    }
    let root = config.root_dir();
    let mut out = Vec::new();
    for path in find_worktree_dirs(&root) {
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        let branch = Git::at(&path)
            .out(&["branch", "--show-current"])
            .ok()
            .filter(|b| !b.is_empty());
        let label = match &branch {
            Some(b) => format!("{} \u{2192} {b}", rel.display()),
            None => rel.display().to_string(),
        };
        out.push(Candidate {
            label,
            branch,
            last_change: last_activity(&path),
            path,
        });
    }
    Ok(out)
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
