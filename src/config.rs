use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

/// Layered configuration. Precedence (low to high):
/// defaults < ~/.config/bonsai/config.toml < <repo>/.bonsai.toml
/// < BONSAI_* env vars (`__` nests: BONSAI_CLEAN__FETCH) < CLI flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where worktrees live: `<root>/<repo-id>/<branch>`.
    pub root: String,
    /// Preferred remote for tracking, fetching, and repo-id derivation.
    pub remote: String,
    /// Skip default-branch detection entirely.
    pub default_branch: Option<String>,
    pub add: AddConfig,
    pub clean: CleanConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct AddConfig {
    /// Fetch the remote before creating a worktree.
    pub fetch: bool,
    /// Globs of untracked files copied from the current worktree into new
    /// ones (e.g. [".env*", ".envrc"]).
    pub copy: Vec<String>,
    /// Shell command run inside a freshly created worktree.
    pub post_add: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CleanConfig {
    /// Fetch --prune before computing merged/gone branches.
    pub fetch: bool,
    /// Branch globs never cleaned.
    pub protected: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: "~/.bonsai".to_string(),
            remote: "origin".to_string(),
            default_branch: None,
            add: AddConfig::default(),
            clean: CleanConfig::default(),
        }
    }
}

impl Default for CleanConfig {
    fn default() -> Self {
        Self {
            fetch: true,
            protected: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(repo_root: Option<&Path>) -> Result<Config> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        if let Some(global) = global_config_path() {
            figment = figment.merge(Toml::file(global));
        }
        if let Some(root) = repo_root {
            figment = figment.merge(Toml::file(root.join(".bonsai.toml")));
        }
        figment = figment.merge(Env::prefixed("BONSAI_").split("__"));
        figment.extract().context("invalid bonsai configuration")
    }

    /// Canonicalized: git resolves symlinks (e.g. /tmp -> /private/tmp on
    /// macOS) when registering worktrees, and we compare paths by prefix.
    pub fn root_dir(&self) -> PathBuf {
        canonicalize_lenient(&expand_tilde(&self.root))
    }
}

/// Canonicalize as much of the path as exists, keeping the rest verbatim.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonicalize_lenient(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// `$XDG_CONFIG_HOME/bonsai/config.toml`, defaulting to `~/.config`.
fn global_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::home_dir()?.join(".config"),
    };
    Some(base.join("bonsai").join("config.toml"))
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::home_dir() {
            return home.join(rest);
        }
    } else if path == "~"
        && let Some(home) = std::env::home_dir()
    {
        return home;
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::result_large_err)] // figment::Error is large by design
    fn precedence_defaults_global_repo_env() {
        figment::Jail::expect_with(|jail| {
            let home = jail.directory().join("home");
            let repo = jail.directory().join("repo");
            std::fs::create_dir_all(home.join(".config/bonsai")).unwrap();
            std::fs::create_dir_all(&repo).unwrap();
            jail.set_env("HOME", home.to_string_lossy());
            jail.set_env("XDG_CONFIG_HOME", home.join(".config").to_string_lossy());
            std::fs::write(
                home.join(".config/bonsai/config.toml"),
                "root = \"/global-root\"\nremote = \"upstream\"\n[clean]\nfetch = false\n",
            )
            .unwrap();

            // Global config overrides defaults.
            let config = Config::load(Some(&repo)).unwrap();
            assert_eq!(config.root, "/global-root");
            assert_eq!(config.remote, "upstream");
            assert!(!config.clean.fetch);
            assert!(config.add.copy.is_empty());

            // Repo config overrides global.
            std::fs::write(
                repo.join(".bonsai.toml"),
                "default_branch = \"develop\"\nremote = \"origin\"\n",
            )
            .unwrap();
            let config = Config::load(Some(&repo)).unwrap();
            assert_eq!(config.remote, "origin");
            assert_eq!(config.default_branch.as_deref(), Some("develop"));
            assert_eq!(config.root, "/global-root");

            // Env overrides repo config, with __ nesting.
            jail.set_env("BONSAI_REMOTE", "fork");
            jail.set_env("BONSAI_CLEAN__FETCH", "true");
            let config = Config::load(Some(&repo)).unwrap();
            assert_eq!(config.remote, "fork");
            assert!(config.clean.fetch);
            Ok(())
        });
    }

    #[test]
    fn tilde_expansion() {
        let home = std::env::home_dir().unwrap();
        assert_eq!(expand_tilde("~/.bonsai"), home.join(".bonsai"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }
}
