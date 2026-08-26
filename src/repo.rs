use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::git::Git;
use crate::worktree::Worktree;

/// The repository the current directory belongs to. All operations are
/// anchored on the main worktree, so bonsai behaves identically whether run
/// from the original clone or from any of its linked worktrees.
pub struct Repo {
    pub main_root: PathBuf,
    pub git: Git,
}

impl Repo {
    pub fn discover() -> Result<Option<Repo>> {
        let git = Git::new();
        if !git.ok(&["rev-parse", "--git-dir"]) {
            return Ok(None);
        }
        // The first entry of `worktree list` is always the main worktree
        // (or the bare git dir), regardless of which worktree we run from.
        let bytes = git.out_bytes(&["worktree", "list", "--porcelain", "-z"])?;
        let worktrees = Worktree::parse_list(&bytes);
        let main = worktrees
            .first()
            .ok_or_else(|| anyhow!("git worktree list returned no entries"))?;
        let main_root = main.path.clone();
        Ok(Some(Repo {
            git: Git::at(&main_root),
            main_root,
        }))
    }

    pub fn require() -> Result<Repo> {
        Self::discover()?.context("not inside a git repository")
    }

    /// All worktrees, main first.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let bytes = self
            .git
            .out_bytes(&["worktree", "list", "--porcelain", "-z"])?;
        Ok(Worktree::parse_list(&bytes))
    }

    /// Worktrees managed by bonsai: the ones living under our repo dir.
    pub fn bonsai_worktrees(&self, config: &Config) -> Result<Vec<Worktree>> {
        let dir = self.bonsai_dir(config);
        Ok(self
            .worktrees()?
            .into_iter()
            .filter(|wt| wt.path.starts_with(&dir))
            .collect())
    }

    /// `<root>/<repo-id>` — where this repo's worktrees live.
    pub fn bonsai_dir(&self, config: &Config) -> PathBuf {
        let mut dir = config.root_dir();
        for segment in self.id(config).split('/') {
            dir.push(segment);
        }
        dir
    }

    /// Stable identifier derived from the remote URL (`github.com/owner/repo`),
    /// falling back to `local/<dirname>-<hash>` for remote-less repos.
    pub fn id(&self, config: &Config) -> String {
        self.remote_name(config)
            .and_then(|remote| self.git.out(&["remote", "get-url", &remote]).ok())
            .and_then(|url| repo_id_from_url(&url))
            .unwrap_or_else(|| self.fallback_id())
    }

    /// The remote to use, first existing wins: bonsai config > git's own
    /// `checkout.defaultRemote` > "origin" > the only remote when there is
    /// exactly one.
    pub fn remote_name(&self, config: &Config) -> Option<String> {
        let mut remotes = self
            .git
            .out(&["remote"])
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut preferred: Vec<String> = Vec::new();
        if let Some(remote) = &config.remote {
            preferred.push(remote.clone());
        }
        if let Ok(remote) = self.git.out(&["config", "checkout.defaultRemote"])
            && !remote.is_empty()
        {
            preferred.push(remote);
        }
        preferred.push("origin".to_string());
        for candidate in preferred {
            if remotes.contains(&candidate) {
                return Some(candidate);
            }
        }
        if remotes.len() == 1 {
            return Some(remotes.remove(0));
        }
        None
    }

    fn branch_ref_exists(&self, name: &str, remote: Option<&str>) -> bool {
        if let Some(remote) = remote
            && self.git.ok(&[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{remote}/{name}"),
            ])
        {
            return true;
        }
        self.git.ok(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
    }

    fn fallback_id(&self) -> String {
        let canonical = crate::paths::canonicalize_or_self(&self.main_root);
        let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());
        let dirname = self
            .main_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        format!(
            "local/{dirname}-{:02x}{:02x}{:02x}{:02x}",
            hash[0], hash[1], hash[2], hash[3]
        )
    }

    /// Default branch resolution: config > <remote>/HEAD > git's own
    /// `init.defaultBranch` (when the ref exists) > well-known names.
    /// Ambiguity is fatal — `clean` deletes based on this answer.
    pub fn default_branch(&self, config: &Config) -> Result<String> {
        if let Some(branch) = &config.default_branch {
            return Ok(branch.clone());
        }
        let remote = self.remote_name(config);
        if let Some(remote) = &remote
            && let Ok(head) = self.git.out(&[
                "symbolic-ref",
                "--short",
                &format!("refs/remotes/{remote}/HEAD"),
            ])
            && let Some(branch) = head.strip_prefix(&format!("{remote}/"))
        {
            return Ok(branch.to_string());
        }
        if let Ok(name) = self.git.out(&["config", "init.defaultBranch"])
            && !name.is_empty()
            && self.branch_ref_exists(&name, remote.as_deref())
        {
            return Ok(name);
        }
        let candidates = ["main", "master", "trunk", "develop"];
        for refs in ["refs/remotes", "refs/heads"] {
            let matches: Vec<&str> = candidates
                .iter()
                .filter(|name| {
                    let r = if refs == "refs/remotes" {
                        let Some(remote) = &remote else { return false };
                        format!("{refs}/{remote}/{name}")
                    } else {
                        format!("{refs}/{name}")
                    };
                    self.git.ok(&["show-ref", "--verify", "--quiet", &r])
                })
                .copied()
                .collect();
            match matches.len() {
                0 => continue,
                1 => return Ok(matches[0].to_string()),
                _ => bail!(
                    "ambiguous default branch ({}); run 'git remote set-head {} --auto' or set default_branch in .bonsai.toml",
                    matches.join(", "),
                    remote.as_deref().unwrap_or("origin"),
                ),
            }
        }
        bail!(
            "cannot determine the default branch; run 'git remote set-head {} --auto' or set default_branch in .bonsai.toml",
            remote.as_deref().unwrap_or("origin"),
        )
    }
}

/// Normalize a git remote URL to `host/path` (ghq-style).
pub fn repo_id_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // scp-like syntax (git@github.com:owner/repo.git) has no scheme.
    let rest = if let Some((scheme, rest)) = url.split_once("://") {
        match scheme {
            "ssh" | "git" | "http" | "https" | "git+ssh" | "ssh+git" => rest,
            _ => return None, // file://, etc: fall back to local id
        }
    } else if let Some((head, tail)) = url.split_once(':') {
        // Not scp-like: local paths (./x:y), Windows drive letters (C:\x,
        // C:/x — same single-letter rule git itself applies), UNC paths.
        if head.contains('/') || head.contains('\\') || head.len() == 1 || tail.starts_with("//") {
            return None;
        }
        return finish_repo_id(head, tail);
    } else {
        return None; // bare local path
    };
    let (host_port, path) = rest.split_once('/')?;
    finish_repo_id(host_port, path)
}

fn finish_repo_id(host: &str, path: &str) -> Option<String> {
    // Strip userinfo, then port.
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    let host = host.to_lowercase();
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{host}/{path}"))
}

/// Validate a branch name before it touches the filesystem.
pub fn validate_branch_name(git: &Git, name: &str) -> Result<()> {
    if name.starts_with('-') || !git.ok(&["check-ref-format", "--branch", name]) {
        bail!("invalid branch name: '{name}'");
    }
    Ok(())
}

/// Best-effort case-insensitive path collision probe (APFS): if `path` exists
/// on disk but is registered to a different branch, adding would corrupt it.
pub fn dir_collides(path: &Path, worktrees: &[Worktree], branch: &str) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let canonical = crate::paths::canonicalize_ok(path)?;
    worktrees
        .iter()
        .filter(|wt| wt.branch.as_deref().is_some_and(|b| b != branch))
        .find(|wt| crate::paths::canonicalize_ok(&wt.path).is_some_and(|other| other == canonical))
        .and_then(|wt| wt.branch.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_normalization() {
        let cases = [
            (r"C:\Users\me\repo.git", None),
            ("C:/Users/me/repo.git", None),
            (r"\\server\share\repo", None),
            (
                "git@github.com:Owner/Repo.git",
                Some("github.com/Owner/Repo"),
            ),
            ("git@github.com:owner/repo", Some("github.com/owner/repo")),
            (
                "ssh://git@github.com/Owner/Repo",
                Some("github.com/Owner/Repo"),
            ),
            (
                "ssh://git@github.com:22/Owner/Repo.git",
                Some("github.com/Owner/Repo"),
            ),
            (
                "https://github.com/owner/repo.git/",
                Some("github.com/owner/repo"),
            ),
            (
                "https://user:pass@GitHub.com/owner/repo",
                Some("github.com/owner/repo"),
            ),
            (
                "http://github.com:8080/owner/repo",
                Some("github.com/owner/repo"),
            ),
            (
                "git://github.com/owner/repo.git",
                Some("github.com/owner/repo"),
            ),
            (
                "https://gitlab.com/group/subgroup/repo.git",
                Some("gitlab.com/group/subgroup/repo"),
            ),
            ("git+ssh://git@github.com/o/r", Some("github.com/o/r")),
            ("file:///home/user/repo", None),
            ("/home/user/repo", None),
            ("./relative/repo", None),
            ("", None),
            ("https://github.com/", None),
        ];
        for (url, expected) in cases {
            assert_eq!(repo_id_from_url(url).as_deref(), expected, "url: {url}");
        }
    }
}
