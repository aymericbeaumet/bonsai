use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Result, bail};

use crate::parallel;
use crate::picker;
use crate::scope::Scope;
use crate::worktree::last_activity;

struct Candidate {
    /// Repo id, shown when several projects are in scope.
    project: Option<String>,
    label: String,
    branch: Option<String>,
    path: PathBuf,
    last_change: Option<SystemTime>,
}

impl Candidate {
    fn search(&self) -> String {
        match &self.project {
            Some(project) => format!("{project} {}", self.label),
            None => self.label.clone(),
        }
    }
}

pub fn run(scope: &Scope, query: Option<String>) -> Result<Option<PathBuf>> {
    pick(scope, query.as_deref(), "Worktree:").map(Some)
}

/// Resolve a worktree from the scope: a unique exact branch or label match
/// wins, then a unique substring match; anything else opens the picker
/// pre-filtered with the query.
pub fn pick(scope: &Scope, query: Option<&str>, prompt: &str) -> Result<PathBuf> {
    let mut candidates = candidates(scope)?;
    if candidates.is_empty() {
        bail!("no worktrees found");
    }
    if let Some(query) = query {
        let exact: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.label == query || c.branch.as_deref() == Some(query))
            .collect();
        if exact.len() == 1 {
            return Ok(exact[0].path.clone());
        }
        let lowered = query.to_lowercase();
        let matching: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.search().to_lowercase().contains(&lowered))
            .collect();
        if exact.is_empty() && matching.len() == 1 {
            return Ok(matching[0].path.clone());
        }
    }

    let activity = parallel::map_ordered(&candidates, |candidate| last_activity(&candidate.path));
    for (candidate, last_change) in candidates.iter_mut().zip(activity) {
        candidate.last_change = last_change;
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.last_change));
    let options = styled_options(&candidates);
    let picked = picker::select_styled(prompt, options, query)?;
    Ok(candidates.swap_remove(picked).path)
}

/// Every worktree registered with Git for each project in scope, including
/// worktrees created outside Bonsai.
fn candidates(scope: &Scope) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for project in scope.projects() {
        let id = scope.is_all().then(|| project.id());
        for entry in project.repo.project_worktrees(&project.config)? {
            let label = entry.label();
            let wt = entry.worktree;
            if wt.is_bare {
                continue;
            }
            out.push(Candidate {
                project: id.clone(),
                label,
                branch: wt.branch,
                last_change: None,
                path: wt.path,
            });
        }
    }
    Ok(out)
}

fn styled_options(candidates: &[Candidate]) -> Vec<picker::StyledOption> {
    let rows = candidates
        .iter()
        .map(|candidate| picker::RecentRow {
            columns: candidate
                .project
                .iter()
                .cloned()
                .chain([candidate.label.clone()])
                .collect(),
            search: candidate.search(),
            last_change: candidate.last_change,
        })
        .collect::<Vec<_>>();
    picker::recent_options(&rows)
}
