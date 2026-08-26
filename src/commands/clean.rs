use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::commands::remove::remove_worktree;
use crate::config::Config;
use crate::git::Git;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::Worktree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Merged,
    UpstreamGone,
    SquashMerged,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Merged => "merged",
            Reason::UpstreamGone => "upstream gone",
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
    } else if facts.upstream_gone {
        Some(Reason::UpstreamGone)
    } else if facts.squash_merged {
        Some(Reason::SquashMerged)
    } else {
        None
    }
}

pub fn run(config: &Config, dry_run: bool, yes: bool, no_fetch: bool) -> Result<Option<PathBuf>> {
    let repo = Repo::require()?;
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
    let target = match &remote {
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

    let merged: Vec<String> = repo
        .git
        .out(&["branch", "--merged", &target, "--format=%(refname:short)"])?
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

    let protected = |branch: &str| {
        config
            .clean
            .protected
            .iter()
            .any(|p| glob::Pattern::new(p).is_ok_and(|p| p.matches(branch)))
    };

    let mut candidates: Vec<(Worktree, String, Reason)> = Vec::new();
    for wt in repo.bonsai_worktrees(config)? {
        let Some(branch) = wt.branch.clone() else {
            continue;
        };
        if branch == default || wt.is_locked || protected(&branch) {
            continue;
        }
        let track = tracking.get(&branch).map(String::as_str).unwrap_or("");
        let mut facts = BranchFacts {
            merged: merged.iter().any(|b| b == &branch),
            upstream_gone: track.contains("[gone]"),
            upstream_ahead: track.contains("ahead"),
            ..Default::default()
        };
        if classify(facts).is_none() && !facts.upstream_ahead {
            facts.squash_merged = is_squash_merged(&repo.git, &target, &branch);
        }
        let Some(reason) = classify(facts) else {
            continue;
        };
        if is_dirty(&wt) {
            eprintln!(
                "bonsai: skipping '{branch}' ({}): uncommitted changes",
                reason.as_str()
            );
            continue;
        }
        candidates.push((wt, branch, reason));
    }

    if candidates.is_empty() {
        eprintln!("bonsai: nothing to clean");
        return Ok(None);
    }

    eprintln!("bonsai: worktrees to remove (branches deleted too):");
    for (wt, branch, reason) in &candidates {
        eprintln!("  {branch}\t[{}]\t{}", reason.as_str(), wt.path.display());
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
        .and_then(|d| d.canonicalize().ok());
    let inside = |wt: &Worktree| {
        let canonical = wt.path.canonicalize().unwrap_or_else(|_| wt.path.clone());
        cwd.as_ref().is_some_and(|c| c.starts_with(&canonical))
    };
    candidates.sort_by_key(|(wt, _, _)| inside(wt));

    let mut cd_home = false;
    for (wt, branch, _) in &candidates {
        if inside(wt) {
            cd_home = true;
        }
        remove_worktree(&repo, config, wt, false)?;
        // -d would refuse gone/squash-merged branches; the checks above are
        // the safety justification for -D.
        repo.git.run(&["branch", "-D", branch])?;
        eprintln!("bonsai: deleted branch '{branch}'");
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
            Some(Reason::UpstreamGone)
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
        // ...but not when the upstream is gone (ahead-of-nothing).
        assert_eq!(
            classify(BranchFacts {
                upstream_gone: true,
                upstream_ahead: true,
                ..f()
            }),
            Some(Reason::UpstreamGone)
        );
    }
}
