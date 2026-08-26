use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::git::Git;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::find_worktree_dirs;

struct Candidate {
    label: String,
    path: PathBuf,
}

pub fn run(config: &Config, query: Option<String>) -> Result<Option<PathBuf>> {
    let candidates = candidates(config)?;
    if candidates.is_empty() {
        bail!("no worktrees found");
    }

    if let Some(query) = &query {
        // Exact label/branch match wins, then a unique substring match;
        // anything ambiguous falls through to the picker pre-filtered.
        if let Some(c) = candidates.iter().find(|c| c.label == *query) {
            return Ok(Some(c.path.clone()));
        }
        let matching: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.label.contains(query.as_str()))
            .collect();
        if matching.len() == 1 {
            return Ok(Some(matching[0].path.clone()));
        }
        if matching.is_empty() {
            bail!("no worktree matches '{query}'");
        }
    }

    let labels: Vec<String> = candidates.iter().map(|c| c.label.clone()).collect();
    let picked = picker::select("Worktree:", labels, query.as_deref())?;
    let candidate = candidates
        .into_iter()
        .find(|c| c.label == picked)
        .expect("picked label comes from candidates");
    Ok(Some(candidate.path))
}

/// Inside a repo: its main worktree + its bonsai worktrees. Outside: every
/// worktree under the bonsai root, labelled by repo.
fn candidates(config: &Config) -> Result<Vec<Candidate>> {
    if let Some(repo) = Repo::discover()? {
        let bonsai_dir = repo.bonsai_dir(config);
        let mut out = Vec::new();
        for wt in repo.worktrees()? {
            if wt.is_bare {
                continue;
            }
            let branch = wt
                .branch
                .clone()
                .unwrap_or_else(|| "(detached)".to_string());
            let label = if wt.path.starts_with(&bonsai_dir) {
                branch
            } else {
                format!("{branch} (repo root)")
            };
            out.push(Candidate {
                label,
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
        let label = match branch {
            Some(b) => format!("{} \u{2192} {b}", rel.display()),
            None => rel.display().to_string(),
        };
        out.push(Candidate { label, path });
    }
    Ok(out)
}
