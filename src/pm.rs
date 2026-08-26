use std::path::{Path, PathBuf};

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
                    Some(m) if m >= 2 => PackageManager::YarnBerry,
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

/// (name, major version) from package.json's `packageManager` field:
/// "pnpm@9.1.0+sha256..." -> ("pnpm", Some(9)). Malformed JSON or a missing
/// field falls back to lockfile detection.
fn package_manager_field(dir: &Path) -> Option<(String, Option<u64>)> {
    let raw = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let field = json.get("packageManager")?.as_str()?;
    let (name, version) = match field.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (field, None),
    };
    let major = version.and_then(|v| v.split(['.', '+']).next()?.parse().ok());
    Some((name.to_string(), major))
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
