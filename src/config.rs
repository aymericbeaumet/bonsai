use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

use crate::git::Git;

/// Layered configuration. Precedence (low to high):
/// defaults < ~/.config/bonsai/config.toml < <repo>/.bonsai.toml
/// < `git config bonsai.*` < BONSAI_* env vars (`__` nests:
/// BONSAI_CLEAN__FETCH) < CLI flags.
///
/// The git-config layer is the per-clone personal slot (like `user.email`):
/// `git config bonsai.root ~/src/worktrees`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where worktrees live: `<root>/<repo-id>/<branch>`.
    pub root: String,
    /// Preferred remote for tracking, fetching, and repo-id derivation.
    /// Unset: git's own `checkout.defaultRemote`, then "origin".
    pub remote: Option<String>,
    /// Skip default-branch detection entirely.
    pub default_branch: Option<String>,
    /// Maintain a .code-workspace file per repo (VS Code, Cursor, ...).
    pub workspace: bool,
    pub add: AddConfig,
    pub clean: CleanConfig,
    pub hq: HqConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HqConfig {
    pub auto_setup: bool,
    pub notifications: bool,
    pub tmux_status: bool,
}

impl Default for HqConfig {
    fn default() -> Self {
        Self {
            auto_setup: true,
            notifications: false,
            tmux_status: false,
        }
    }
}

/// Local-only files worth carrying into every new worktree by default:
/// environment files plus per-user AI harness config (Claude Code, Cursor,
/// Codex, OpenCode, ...), so any harness finds its setup in any worktree.
/// Tracked files are part of the checkout already and are never overwritten.
pub const DEFAULT_COPY: &[&str] = &[
    ".env",
    ".env.*",
    ".envrc",
    ".mcp.json",
    "CLAUDE.local.md",
    ".claude/settings.local.json",
    ".cursor/mcp.json",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AddConfig {
    /// Fetch the remote before creating a worktree.
    pub fetch: bool,
    /// Globs of untracked files copied into new worktrees, looked up in the
    /// current worktree first, then the main one. Setting this replaces the
    /// defaults (DEFAULT_COPY).
    pub copy: Vec<String>,
    /// Auto-install dependencies in new worktrees with the package manager
    /// detected from lockfiles (pnpm/npm/yarn/bun/cargo/uv).
    pub install: bool,
    /// Shell command run inside a freshly created worktree.
    pub post_add: Option<String>,
}

impl Default for AddConfig {
    fn default() -> Self {
        Self {
            fetch: true,
            copy: DEFAULT_COPY.iter().map(|s| s.to_string()).collect(),
            install: true,
            post_add: None,
        }
    }
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
            remote: None,
            default_branch: None,
            workspace: true,
            add: AddConfig::default(),
            clean: CleanConfig::default(),
            hq: HqConfig::default(),
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
    /// `toml_dir` is where `.bonsai.toml` is read from (the current
    /// worktree's checkout when it has one, else the main worktree); `git`
    /// is used to read the effective `git config bonsai.*` values.
    pub fn load(toml_dir: Option<&Path>, git: &Git) -> Result<Config> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        if let Some(global) = global_config_path() {
            figment = figment.merge(Toml::file(global));
        }
        if let Some(dir) = toml_dir {
            figment = figment.merge(Toml::file(dir.join(".bonsai.toml")));
        }
        figment = figment.merge(Serialized::defaults(git_config_patch(git)?));
        figment = figment.merge(Env::prefixed("BONSAI_").split("__"));
        let config: Config = figment.extract().context("invalid bonsai configuration")?;
        for pattern in &config.clean.protected {
            glob::Pattern::new(pattern)
                .with_context(|| format!("invalid clean.protected glob '{pattern}'"))?;
        }
        for pattern in &config.add.copy {
            glob::Pattern::new(pattern)
                .with_context(|| format!("invalid add.copy glob '{pattern}'"))?;
            let path = Path::new(pattern);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
            {
                anyhow::bail!("add.copy glob '{pattern}' must stay inside the checkout");
            }
        }
        Ok(config)
    }

    /// Canonicalized: git resolves symlinks (e.g. /tmp -> /private/tmp on
    /// macOS) when registering worktrees, and we compare paths by prefix.
    pub fn root_dir(&self) -> PathBuf {
        crate::paths::canonicalize_lenient(&expand_tilde(&self.root))
    }
}

/// Sparse overlay parsed from `git config bonsai.*`; unset fields serialize
/// to nothing and therefore merge to no-ops.
#[derive(Debug, Default, Serialize)]
struct GitConfigPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<bool>,
    add: GitConfigAddPatch,
    clean: GitConfigCleanPatch,
    hq: GitConfigHqPatch,
}

#[derive(Debug, Default, Serialize)]
struct GitConfigHqPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_status: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
struct GitConfigAddPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_add: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct GitConfigCleanPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protected: Option<Vec<String>>,
}

/// Read the effective (system < global < local) `bonsai.*` git config.
/// Multi-valued keys (`git config --add bonsai.add.copy ...`) accumulate.
fn git_config_patch(git: &Git) -> Result<GitConfigPatch> {
    let mut patch = GitConfigPatch::default();
    // Exit code 1 (no matches) and running outside a repo are both fine.
    let bytes = match git.out_bytes(&["config", "--get-regexp", "--null", r"^bonsai\."]) {
        Ok(bytes) => bytes,
        Err(error) if error.status == Some(1) => return Ok(patch),
        Err(error) => return Err(error.into()),
    };
    for entry in String::from_utf8_lossy(&bytes)
        .split('\0')
        .filter(|entry| !entry.is_empty())
    {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        // git lowercases key sections; accept kebab-case spellings too.
        let key = key.to_lowercase().replace('-', "");
        let value = value.to_string();
        let boolean = || {
            git_bool(&value)
                .with_context(|| format!("invalid boolean for git config {key}: '{value}'"))
        };
        match key.as_str() {
            "bonsai.root" => patch.root = Some(value),
            "bonsai.remote" => patch.remote = Some(value),
            "bonsai.defaultbranch" => patch.default_branch = Some(value),
            "bonsai.workspace" => patch.workspace = Some(boolean()?),
            "bonsai.add.fetch" => patch.add.fetch = Some(boolean()?),
            "bonsai.add.install" => patch.add.install = Some(boolean()?),
            "bonsai.add.postadd" => patch.add.post_add = Some(value),
            "bonsai.add.copy" => patch.add.copy.get_or_insert_default().push(value),
            "bonsai.clean.fetch" => patch.clean.fetch = Some(boolean()?),
            "bonsai.clean.protected" => patch.clean.protected.get_or_insert_default().push(value),
            "bonsai.hq.autosetup" => patch.hq.auto_setup = Some(boolean()?),
            "bonsai.hq.notifications" => patch.hq.notifications = Some(boolean()?),
            "bonsai.hq.tmuxstatus" => patch.hq.tmux_status = Some(boolean()?),
            _ => eprintln!("bonsai: ignoring unknown git config key '{key}'"),
        }
    }
    Ok(patch)
}

fn git_bool(value: &str) -> Option<bool> {
    match value.to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => value.parse::<i64>().ok().map(|number| number != 0),
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
            // Neutralize the host's git config for this jail.
            jail.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
            jail.set_env("GIT_CONFIG_NOSYSTEM", "1");
            let git = Git::at(&repo);
            std::fs::write(
                home.join(".config/bonsai/config.toml"),
                "root = \"/global-root\"\nremote = \"upstream\"\n[clean]\nfetch = false\n",
            )
            .unwrap();

            // Global config overrides defaults.
            let config = Config::load(Some(&repo), &git).unwrap();
            assert_eq!(config.root, "/global-root");
            assert_eq!(config.remote.as_deref(), Some("upstream"));
            assert!(!config.clean.fetch);
            assert_eq!(config.add.copy, DEFAULT_COPY);
            assert!(config.add.fetch);
            assert!(config.add.install);
            assert!(config.hq.auto_setup);
            assert!(!config.hq.notifications);
            assert!(!config.hq.tmux_status);

            // Repo config overrides global.
            std::fs::write(
                repo.join(".bonsai.toml"),
                "default_branch = \"develop\"\nremote = \"origin\"\n[add]\ninstall = false\n",
            )
            .unwrap();
            let config = Config::load(Some(&repo), &git).unwrap();
            assert_eq!(config.remote.as_deref(), Some("origin"));
            assert_eq!(config.default_branch.as_deref(), Some("develop"));
            assert_eq!(config.root, "/global-root");
            assert!(!config.add.install);

            // Env overrides repo config, with __ nesting.
            jail.set_env("BONSAI_REMOTE", "fork");
            jail.set_env("BONSAI_CLEAN__FETCH", "true");
            jail.set_env("BONSAI_ADD__INSTALL", "true");
            jail.set_env("BONSAI_HQ__AUTO_SETUP", "false");
            jail.set_env("BONSAI_HQ__NOTIFICATIONS", "true");
            let config = Config::load(Some(&repo), &git).unwrap();
            assert_eq!(config.remote.as_deref(), Some("fork"));
            assert!(config.clean.fetch);
            assert!(config.add.install);
            assert!(!config.hq.auto_setup);
            assert!(config.hq.notifications);
            Ok(())
        });
    }

    #[test]
    fn git_bool_values() {
        assert_eq!(git_bool("true"), Some(true));
        assert_eq!(git_bool("Yes"), Some(true));
        assert_eq!(git_bool(""), Some(true)); // `git config bonsai.add.fetch` bare
        assert_eq!(git_bool("0"), Some(false));
        assert_eq!(git_bool("banana"), None);
    }

    #[test]
    fn tilde_expansion() {
        let home = std::env::home_dir().unwrap();
        assert_eq!(expand_tilde("~/.bonsai"), home.join(".bonsai"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }
}
