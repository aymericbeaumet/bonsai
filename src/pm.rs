use std::path::{Path, PathBuf};

use serde::Deserialize;

const PNPM_WORKTREE_DOCS: &str = "https://pnpm.io/git-worktrees";
const BUN_WORKTREE_DOCS: &str = "https://bun.sh/docs/pm/global-store";
const YARN_CONFIG_DOCS: &str = "https://yarnpkg.com/configuration/yarnrc/";
const UV_CACHE_DOCS: &str = "https://docs.astral.sh/uv/concepts/cache/#cache-directory";

#[derive(Debug, PartialEq, Eq)]
pub struct WorktreeWarning {
    pub message: String,
    pub docs: &'static str,
}

/// Package managers bonsai knows how to run in a fresh worktree. Yarn keeps
/// two variants because classic (1.x) and berry (2+) take different flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Pnpm,
    Npm,
    YarnClassic,
    YarnBerry,
    Bun,
    Cargo,
    Uv,
}

impl PackageManager {
    pub fn program(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Npm => "npm",
            Self::YarnClassic | Self::YarnBerry => "yarn",
            Self::Bun => "bun",
            Self::Cargo => "cargo",
            Self::Uv => "uv",
        }
    }

    /// Install arguments chosen to never mutate the checkout (frozen
    /// lockfiles) and to lean on each tool's shared store/cache.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::Pnpm => &["install", "--frozen-lockfile", "--prefer-offline"],
            Self::Npm => &["ci", "--prefer-offline", "--no-audit", "--no-fund"],
            Self::YarnClassic => &["install", "--frozen-lockfile", "--prefer-offline"],
            // Berry has no --prefer-offline; its cache is used by default.
            Self::YarnBerry => &["install", "--immutable"],
            Self::Bun => &["install", "--frozen-lockfile"],
            // Populates the shared ~/.cargo registry without building.
            Self::Cargo => &["fetch", "--locked"],
            Self::Uv => &["sync", "--frozen"],
        }
    }

    fn worktree_warning(self, dir: &Path) -> Option<WorktreeWarning> {
        match self {
            Self::Pnpm => pnpm_worktree_warning(dir),
            Self::YarnBerry => yarn_worktree_warning(dir),
            Self::Bun => bun_worktree_warning(dir),
            Self::Uv => uv_worktree_warning(dir),
            // npm and Yarn Classic use global download caches by default;
            // Cargo likewise shares registry and Git sources in CARGO_HOME.
            Self::Npm | Self::YarnClassic | Self::Cargo => None,
        }
    }
}

/// Actionable configuration warnings for the package managers detected in a
/// worktree. This is deliberately independent of auto-install: users who run
/// installs themselves still benefit from a worktree-friendly store layout.
pub fn worktree_warnings(dir: &Path) -> Vec<WorktreeWarning> {
    detect(dir)
        .into_iter()
        .filter_map(|pm| pm.worktree_warning(dir))
        .collect()
}

/// Every package manager applicable to `dir` (a Rust+JS monorepo yields
/// several), with at most one JS package manager. Keyed on lockfiles: a
/// project without one has nothing reproducible to install, and running a
/// lockfile-writing install would dirty the fresh worktree.
pub fn detect(dir: &Path) -> Vec<PackageManager> {
    let mut pms = Vec::new();
    if let Some(js) = detect_js(dir) {
        pms.push(js);
    }
    if dir.join("Cargo.lock").is_file() {
        pms.push(PackageManager::Cargo);
    }
    if dir.join("uv.lock").is_file() {
        pms.push(PackageManager::Uv);
    }
    pms
}

fn detect_js(dir: &Path) -> Option<PackageManager> {
    if let Some((name, major)) = package_manager_field(dir) {
        match name.as_str() {
            "pnpm" => return Some(PackageManager::Pnpm),
            "npm" => return Some(PackageManager::Npm),
            "bun" => return Some(PackageManager::Bun),
            "yarn" => {
                return Some(match major {
                    Some(version) if version.major >= 2 => PackageManager::YarnBerry,
                    Some(_) => PackageManager::YarnClassic,
                    None => yarn_flavor(dir),
                });
            }
            _ => {} // unknown manager: fall back to lockfiles
        }
    }
    if dir.join("pnpm-lock.yaml").is_file() {
        Some(PackageManager::Pnpm)
    } else if dir.join("bun.lock").is_file() || dir.join("bun.lockb").is_file() {
        Some(PackageManager::Bun)
    } else if dir.join("yarn.lock").is_file() {
        Some(yarn_flavor(dir))
    } else if dir.join("package-lock.json").is_file() || dir.join("npm-shrinkwrap.json").is_file() {
        Some(PackageManager::Npm)
    } else {
        None
    }
}

fn yarn_flavor(dir: &Path) -> PackageManager {
    if dir.join(".yarnrc.yml").is_file() {
        PackageManager::YarnBerry
    } else {
        PackageManager::YarnClassic
    }
}

/// (name, version) from package.json's `packageManager` field. Malformed JSON
/// or a missing field falls back to lockfile detection.
fn package_manager_field(dir: &Path) -> Option<(String, Option<Version>)> {
    let raw = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let field = json.get("packageManager")?.as_str()?;
    let (name, version) = match field.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (field, None),
    };
    Some((name.to_string(), version.and_then(Version::parse)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split(['.', '+', '-']);
        Some(Self {
            major: parts.next()?.parse().ok()?,
            minor: parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
            patch: parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PnpmConfig {
    virtual_store_type: Option<String>,
    enable_global_virtual_store: Option<bool>,
}

fn pnpm_worktree_warning(dir: &Path) -> Option<WorktreeWarning> {
    let version = package_manager_version(dir, "pnpm");
    if version.is_some_and(|v| {
        v < (Version {
            major: 10,
            minor: 12,
            patch: 1,
        })
    }) {
        return Some(WorktreeWarning {
            message: "pnpm needs version 10.12.1+ and a global virtual store for efficient worktrees; upgrade pnpm, then configure pnpm-workspace.yaml"
                .to_string(),
            docs: PNPM_WORKTREE_DOCS,
        });
    }
    let config = read_yaml::<PnpmConfig>(&dir.join("pnpm-workspace.yaml")).unwrap_or_default();
    if config.virtual_store_type.as_deref() == Some("global")
        || config.enable_global_virtual_store == Some(true)
    {
        return None;
    }
    let message = if version.is_some_and(|v| {
        v < (Version {
            major: 11,
            minor: 23,
            patch: 0,
        })
    }) {
        "pnpm is using a per-worktree virtual store; add 'enableGlobalVirtualStore: true' to pnpm-workspace.yaml"
            .to_string()
    } else {
        "pnpm is using a per-worktree virtual store; add 'virtualStoreType: global' to pnpm-workspace.yaml"
            .to_string()
    };
    Some(WorktreeWarning {
        message,
        docs: PNPM_WORKTREE_DOCS,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BunConfig {
    #[serde(default)]
    install: BunInstallConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BunInstallConfig {
    linker: Option<String>,
    global_store: Option<bool>,
}

fn bun_worktree_warning(dir: &Path) -> Option<WorktreeWarning> {
    if package_manager_version(dir, "bun")
        .is_some_and(|v| v.major < 1 || (v.major == 1 && v.minor < 4))
    {
        return Some(WorktreeWarning {
            message: "Bun needs version 1.4+ for a global virtual store; upgrade Bun, then set linker = \"isolated\" and globalStore = true under [install] in bunfig.toml"
                .to_string(),
            docs: BUN_WORKTREE_DOCS,
        });
    }
    let config = read_toml::<BunConfig>(&dir.join("bunfig.toml")).unwrap_or_default();
    if config.install.linker.as_deref() == Some("isolated")
        && config.install.global_store == Some(true)
    {
        return None;
    }
    Some(WorktreeWarning {
        message: "Bun is materializing dependencies in every worktree; set linker = \"isolated\" and globalStore = true under [install] in bunfig.toml"
            .to_string(),
        docs: BUN_WORKTREE_DOCS,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YarnConfig {
    enable_global_cache: Option<bool>,
    node_linker: Option<String>,
    nm_mode: Option<String>,
}

fn yarn_worktree_warning(dir: &Path) -> Option<WorktreeWarning> {
    let config = read_yaml::<YarnConfig>(&dir.join(".yarnrc.yml")).unwrap_or_default();
    let version = package_manager_version(dir, "yarn");
    let global_cache = config
        .enable_global_cache
        .unwrap_or_else(|| version.is_some_and(|v| v.major >= 4));
    let linker_is_shared = match config.node_linker.as_deref().unwrap_or("pnp") {
        "node-modules" => config.nm_mode.as_deref() == Some("hardlinks-global"),
        "pnp" | "pnpm" => true,
        _ => false,
    };
    if global_cache && linker_is_shared {
        return None;
    }
    let setting = if !global_cache {
        "set enableGlobalCache: true"
    } else {
        "use Yarn PnP or set nmMode: hardlinks-global with the node-modules linker"
    };
    Some(WorktreeWarning {
        message: format!("Yarn is keeping dependency data per worktree; {setting} in .yarnrc.yml"),
        docs: YARN_CONFIG_DOCS,
    })
}

#[derive(Debug, Default, Deserialize)]
struct UvConfig {
    #[serde(rename = "no-cache")]
    no_cache: Option<bool>,
    #[serde(rename = "cache-dir")]
    cache_dir: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PyprojectConfig {
    #[serde(default)]
    tool: PyprojectTool,
}

#[derive(Debug, Default, Deserialize)]
struct PyprojectTool {
    #[serde(default)]
    uv: UvConfig,
}

fn uv_worktree_warning(dir: &Path) -> Option<WorktreeWarning> {
    let config = if dir.join("uv.toml").is_file() {
        read_toml::<UvConfig>(&dir.join("uv.toml")).unwrap_or_default()
    } else {
        read_toml::<PyprojectConfig>(&dir.join("pyproject.toml"))
            .unwrap_or_default()
            .tool
            .uv
    };
    let no_cache = std::env::var("UV_NO_CACHE")
        .ok()
        .and_then(|value| parse_bool(&value))
        .or(config.no_cache)
        .unwrap_or(false);
    let cache_dir = std::env::var("UV_CACHE_DIR")
        .ok()
        .or(config.cache_dir)
        .filter(|path| cache_is_inside_worktree(dir, path));
    if !no_cache && cache_dir.is_none() {
        return None;
    }
    let problem = if no_cache {
        "uv's cache is disabled"
    } else {
        "uv's cache directory is inside the worktree"
    };
    Some(WorktreeWarning {
        message: format!(
            "{problem}; use uv's default shared system cache for fast worktree installs"
        ),
        docs: UV_CACHE_DOCS,
    })
}

fn package_manager_version(dir: &Path, expected: &str) -> Option<Version> {
    let (name, version) = package_manager_field(dir)?;
    (name == expected).then_some(version).flatten()
}

fn read_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_yaml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn cache_is_inside_worktree(dir: &Path, cache: &str) -> bool {
    let cache = crate::config::expand_tilde(cache);
    let cache = if cache.is_absolute() {
        cache
    } else {
        dir.join(cache)
    };
    crate::paths::canonicalize_lenient(&cache).starts_with(crate::paths::canonicalize_lenient(dir))
}

/// Locate `name` on PATH. Windows package managers ship as `.cmd` shims
/// (pnpm/npm/yarn) that `Command::new("pnpm")` would not find; resolving the
/// concrete file also gives one uniform missing-binary signal everywhere.
pub fn find_program(name: &str) -> Option<PathBuf> {
    find_program_in(name, &std::env::var_os("PATH")?)
}

fn find_program_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| find_in_dir(&dir, name))
}

#[cfg(windows)]
fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    ["exe", "cmd", "bat"]
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|c| c.is_file())
}

#[cfg(not(windows))]
fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidate = dir.join(name);
    (candidate.is_file()
        && std::fs::metadata(&candidate).is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
    .then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "").unwrap();
    }

    #[test]
    fn detect_from_lockfiles() {
        let cases: &[(&str, PackageManager)] = &[
            ("pnpm-lock.yaml", PackageManager::Pnpm),
            ("bun.lock", PackageManager::Bun),
            ("bun.lockb", PackageManager::Bun),
            ("yarn.lock", PackageManager::YarnClassic),
            ("package-lock.json", PackageManager::Npm),
            ("npm-shrinkwrap.json", PackageManager::Npm),
            ("Cargo.lock", PackageManager::Cargo),
            ("uv.lock", PackageManager::Uv),
        ];
        for (lockfile, expected) in cases {
            let tmp = tempfile::tempdir().unwrap();
            touch(tmp.path(), lockfile);
            assert_eq!(detect(tmp.path()), vec![*expected], "lockfile {lockfile}");
        }
    }

    #[test]
    fn detect_js_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "yarn.lock");
        touch(tmp.path(), "package-lock.json");
        touch(tmp.path(), "pnpm-lock.yaml");
        assert_eq!(detect(tmp.path()), vec![PackageManager::Pnpm]);
    }

    #[test]
    fn detect_package_manager_field_wins() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"packageManager": "pnpm@9.1.0"}"#,
        )
        .unwrap();
        touch(tmp.path(), "package-lock.json");
        assert_eq!(detect(tmp.path()), vec![PackageManager::Pnpm]);
    }

    #[test]
    fn detect_bare_package_json_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), r#"{"name": "x"}"#).unwrap();
        assert_eq!(detect(tmp.path()), Vec::<PackageManager>::new());
    }

    #[test]
    fn detect_malformed_package_json_falls_back_to_lockfiles() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "not json").unwrap();
        touch(tmp.path(), "yarn.lock");
        assert_eq!(detect(tmp.path()), vec![PackageManager::YarnClassic]);
    }

    #[test]
    fn detect_yarn_flavor() {
        // yarn.lock alone: classic.
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "yarn.lock");
        assert_eq!(detect(tmp.path()), vec![PackageManager::YarnClassic]);
        // .yarnrc.yml: berry.
        touch(tmp.path(), ".yarnrc.yml");
        assert_eq!(detect(tmp.path()), vec![PackageManager::YarnBerry]);

        // packageManager version wins over .yarnrc.yml.
        let cases: &[(&str, PackageManager)] = &[
            ("yarn@4.0.0", PackageManager::YarnBerry),
            ("yarn@1.22.19", PackageManager::YarnClassic),
        ];
        for (field, expected) in cases {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(
                tmp.path().join("package.json"),
                format!(r#"{{"packageManager": "{field}"}}"#),
            )
            .unwrap();
            touch(tmp.path(), ".yarnrc.yml");
            assert_eq!(detect(tmp.path()), vec![*expected], "field {field}");
        }
    }

    #[test]
    fn detect_multiple_ecosystems() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "Cargo.lock");
        touch(tmp.path(), "uv.lock");
        touch(tmp.path(), "pnpm-lock.yaml");
        assert_eq!(
            detect(tmp.path()),
            vec![
                PackageManager::Pnpm,
                PackageManager::Cargo,
                PackageManager::Uv
            ]
        );
    }

    #[test]
    fn install_args_per_pm() {
        let cases: &[(PackageManager, &str, &[&str])] = &[
            (
                PackageManager::Pnpm,
                "pnpm",
                &["install", "--frozen-lockfile", "--prefer-offline"],
            ),
            (
                PackageManager::Npm,
                "npm",
                &["ci", "--prefer-offline", "--no-audit", "--no-fund"],
            ),
            (
                PackageManager::YarnClassic,
                "yarn",
                &["install", "--frozen-lockfile", "--prefer-offline"],
            ),
            (
                PackageManager::YarnBerry,
                "yarn",
                &["install", "--immutable"],
            ),
            (
                PackageManager::Bun,
                "bun",
                &["install", "--frozen-lockfile"],
            ),
            (PackageManager::Cargo, "cargo", &["fetch", "--locked"]),
            (PackageManager::Uv, "uv", &["sync", "--frozen"]),
        ];
        for (pm, program, args) in cases {
            assert_eq!(pm.program(), *program);
            assert_eq!(pm.args(), *args);
        }
    }

    #[test]
    fn pnpm_warns_until_global_virtual_store_is_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "pnpm-lock.yaml");
        let warnings = worktree_warnings(tmp.path());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].docs, PNPM_WORKTREE_DOCS);
        assert!(warnings[0].message.contains("virtualStoreType: global"));

        std::fs::write(
            tmp.path().join("pnpm-workspace.yaml"),
            "packages: ['packages/*']\nvirtualStoreType: global\n",
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path()).is_empty());

        std::fs::write(
            tmp.path().join("pnpm-workspace.yaml"),
            "enableGlobalVirtualStore: true\n",
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path()).is_empty());
    }

    #[test]
    fn pnpm_warning_is_version_aware() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "pnpm-lock.yaml");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"packageManager":"pnpm@10.11.0"}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("pnpm-workspace.yaml"),
            "virtualStoreType: global\n",
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path())[0].message.contains("upgrade"));

        std::fs::write(tmp.path().join("pnpm-workspace.yaml"), "packages: []\n").unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"packageManager":"pnpm@10.12.1"}"#,
        )
        .unwrap();
        assert!(
            worktree_warnings(tmp.path())[0]
                .message
                .contains("enableGlobalVirtualStore")
        );
    }

    #[test]
    fn bun_requires_isolated_global_store() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "bun.lock");
        assert_eq!(worktree_warnings(tmp.path())[0].docs, BUN_WORKTREE_DOCS);

        std::fs::write(
            tmp.path().join("bunfig.toml"),
            "[install]\nlinker = \"isolated\"\nglobalStore = true\n",
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path()).is_empty());

        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"packageManager":"bun@1.3.0"}"#,
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path())[0].message.contains("upgrade"));
    }

    #[test]
    fn yarn_berry_checks_version_cache_and_linker() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "yarn.lock");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"packageManager":"yarn@3.8.7"}"#,
        )
        .unwrap();
        assert!(
            worktree_warnings(tmp.path())[0]
                .message
                .contains("enableGlobalCache")
        );

        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"packageManager":"yarn@4.9.2"}"#,
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path()).is_empty());

        std::fs::write(tmp.path().join(".yarnrc.yml"), "nodeLinker: node-modules\n").unwrap();
        assert!(
            worktree_warnings(tmp.path())[0]
                .message
                .contains("hardlinks-global")
        );

        std::fs::write(
            tmp.path().join(".yarnrc.yml"),
            "nodeLinker: node-modules\nnmMode: hardlinks-global\n",
        )
        .unwrap();
        assert!(worktree_warnings(tmp.path()).is_empty());
    }

    #[test]
    fn uv_warns_only_for_an_explicitly_unshared_cache() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "uv.lock");
        assert!(worktree_warnings(tmp.path()).is_empty());

        std::fs::write(tmp.path().join("uv.toml"), "no-cache = true\n").unwrap();
        assert_eq!(worktree_warnings(tmp.path())[0].docs, UV_CACHE_DOCS);

        std::fs::write(tmp.path().join("uv.toml"), "cache-dir = \".uv-cache\"\n").unwrap();
        assert!(
            worktree_warnings(tmp.path())[0]
                .message
                .contains("inside the worktree")
        );
    }

    #[test]
    fn package_managers_with_shared_default_caches_do_not_warn() {
        for lockfile in ["package-lock.json", "yarn.lock", "Cargo.lock"] {
            let tmp = tempfile::tempdir().unwrap();
            touch(tmp.path(), lockfile);
            assert!(worktree_warnings(tmp.path()).is_empty(), "{lockfile}");
        }
    }

    #[test]
    fn find_program_resolves_and_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "fakepm.cmd"
        } else {
            "fakepm"
        };
        std::fs::write(tmp.path().join(name), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                tmp.path().join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let path = std::env::join_paths([tmp.path()]).unwrap();
        assert_eq!(
            find_program_in("fakepm", &path),
            Some(tmp.path().join(name))
        );
        assert_eq!(find_program_in("missing", &path), None);
    }
}
