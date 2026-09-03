use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::git::Git;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::{find_worktree_dirs, last_activity};

struct Candidate {
    label: String,
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

    let options = styled_options(&candidates);
    let picked = picker::select_styled("Worktree:", options, query.as_deref())?;
    Ok(Some(candidates.swap_remove(picked).path))
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
        let label = match branch {
            Some(b) => format!("{} \u{2192} {b}", rel.display()),
            None => rel.display().to_string(),
        };
        out.push(Candidate {
            label,
            last_change: last_activity(&path),
            path,
        });
    }
    Ok(out)
}

/// Picker rows: `<label>  <age>`, the age column aligned and colored by
/// freshness (green = today, yellow = this week, dim = older).
fn styled_options(candidates: &[Candidate]) -> Vec<picker::StyledOption> {
    let color = std::io::IsTerminal::is_terminal(&std::io::stderr())
        && std::env::var_os("NO_COLOR").is_none();
    let now = SystemTime::now();
    let width = candidates.iter().map(|c| c.label.len()).max().unwrap_or(0);
    candidates
        .iter()
        .map(|c| {
            let age = c
                .last_change
                .and_then(|t| now.duration_since(t).ok())
                .map(format_age);
            let plain = match &age {
                Some(age) => format!("{:width$}  {age}", c.label),
                None => c.label.clone(),
            };
            let styled = match &age {
                Some(age) if color => {
                    let elapsed = now
                        .duration_since(c.last_change.expect("age implies last_change"))
                        .unwrap_or_default();
                    format!("{:width$}  {}{age}\u{1b}[0m", c.label, age_color(elapsed))
                }
                _ => plain.clone(),
            };
            picker::StyledOption { plain, styled }
        })
        .collect()
}

/// Compact relative age: 42s, 5m, 3h, 2d, 4w, 6mo, 1y.
fn format_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        86_400..604_800 => format!("{}d", secs / 86_400),
        604_800..2_592_000 => format!("{}w", secs / 604_800),
        2_592_000..31_536_000 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_536_000),
    }
}

/// Freshness gradient: bright green (< 1h), green (today), yellow (this
/// week), dim (older).
fn age_color(elapsed: Duration) -> &'static str {
    match elapsed.as_secs() {
        0..3_600 => "\u{1b}[92m",
        3_600..86_400 => "\u{1b}[32m",
        86_400..604_800 => "\u{1b}[33m",
        _ => "\u{1b}[2m",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_format_compactly() {
        let cases = [
            (30, "30s"),
            (90, "1m"),
            (3 * 3_600, "3h"),
            (2 * 86_400, "2d"),
            (13 * 86_400, "1w"),
            (40 * 86_400, "1mo"),
            (2 * 31_536_000, "2y"),
        ];
        for (secs, expected) in cases {
            assert_eq!(format_age(Duration::from_secs(secs)), expected);
        }
    }

    #[test]
    fn age_colors_follow_freshness() {
        assert_eq!(age_color(Duration::from_secs(60)), "\u{1b}[92m");
        assert_eq!(age_color(Duration::from_secs(2 * 3_600)), "\u{1b}[32m");
        assert_eq!(age_color(Duration::from_secs(3 * 86_400)), "\u{1b}[33m");
        assert_eq!(age_color(Duration::from_secs(30 * 86_400)), "\u{1b}[2m");
    }
}
