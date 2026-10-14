//! Copy-on-write seeding of per-worktree build output. Cargo keeps compiled
//! dependencies in a `target/` directory inside each checkout, so every new
//! worktree would otherwise rebuild and store all of them again. Cloning the
//! directory from an existing checkout shares its blocks until either side
//! rewrites them: dependencies start fresh, and only the workspace's own
//! crates rebuild.

use std::path::{Path, PathBuf};

/// Clone Cargo's per-checkout build directories into the new worktree from
/// the first source checkout that has them. Shared or custom locations outside
/// the worktree (`CARGO_TARGET_DIR`, `build.target-dir`) are left alone, and a
/// filesystem without copy-on-write support never falls back to a full copy.
pub fn seed_cargo(sources: &[PathBuf], dest: &Path) {
    if !dest.join("Cargo.toml").is_file() {
        return;
    }
    let Some(cargo) = crate::pm::find_program_at("cargo", dest) else {
        return;
    };
    let Ok(dest) = std::fs::canonicalize(dest) else {
        return;
    };
    for dir in cargo_build_dirs(&cargo, &dest) {
        let Ok(rel) = dir.strip_prefix(&dest) else {
            continue;
        };
        if rel.as_os_str().is_empty() || std::fs::symlink_metadata(&dir).is_ok() {
            continue;
        }
        let Some(source) = sources
            .iter()
            .map(|root| root.join(rel))
            .find(|path| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()))
        else {
            continue;
        };
        if let Err(error) = crate::paths::ensure_contained(&dir, &dest) {
            eprintln!(
                "bonsai: [cargo] refusing to seed {}: {error:#}",
                dir.display()
            );
            continue;
        }
        if let Some(parent) = dir.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            eprintln!("bonsai: [cargo] failed to seed {}: {error}", rel.display());
            continue;
        }
        match clone_tree(&source, &dir) {
            Ok(true) => eprintln!(
                "bonsai: [cargo] cloned {} from {} (copy-on-write)",
                rel.display(),
                source.display()
            ),
            Ok(false) => eprintln!(
                "bonsai: [cargo] {} not seeded: copy-on-write cloning is unavailable here",
                rel.display()
            ),
            Err(error) => {
                eprintln!("bonsai: [cargo] failed to clone {}: {error}", rel.display())
            }
        }
    }
}

/// Target and build directories as Cargo resolves them for `dir`, parents
/// first so a nested build directory is covered by its cloned target.
/// Toolchain auto-installation is disabled: setup belongs to the installer.
fn cargo_build_dirs(cargo: &Path, dir: &Path) -> Vec<PathBuf> {
    let Ok(output) = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(dir)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_build_dirs(&output.stdout)
}

fn parse_build_dirs(metadata: &[u8]) -> Vec<PathBuf> {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(metadata) else {
        return Vec::new();
    };
    let mut dirs = ["target_directory", "build_directory"]
        .iter()
        .filter_map(|key| json.get(key)?.as_str())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Clone `src` to the absent `dst`. `Ok(false)` means copy-on-write is not
/// available for this pair of paths; nothing is left behind in that case.
#[cfg(target_os = "macos")]
fn clone_tree(src: &Path, dst: &Path) -> std::io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const CLONE_NOFOLLOW: u32 = 0x0001;
    let src = CString::new(src.as_os_str().as_bytes())?;
    let dst = CString::new(dst.as_os_str().as_bytes())?;
    // clonefile(2) clones a whole directory hierarchy, preserving metadata.
    if unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EXDEV) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn clone_tree(src: &Path, dst: &Path) -> std::io::Result<bool> {
    match reflink_dir(src, dst) {
        Ok(()) => Ok(true),
        Err(error) => {
            let _ = std::fs::remove_dir_all(dst);
            match error.raw_os_error() {
                Some(libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL | libc::ENOTTY) => Ok(false),
                _ => Err(error),
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn reflink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            reflink_dir(&from, &to)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else if kind.is_file() {
            reflink_file(&from, &to)?;
        }
    }
    Ok(())
}

/// Cargo's freshness checks compare modification times, so they are carried
/// over along with permissions.
#[cfg(target_os = "linux")]
fn reflink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let source = std::fs::File::open(src)?;
    let metadata = source.metadata()?;
    let target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    if unsafe { libc::ioctl(target.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    target.set_permissions(metadata.permissions())?;
    target.set_times(
        std::fs::FileTimes::new()
            .set_accessed(metadata.accessed()?)
            .set_modified(metadata.modified()?),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_tree(_src: &Path, _dst: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dirs_are_deduplicated_parents_first() {
        let metadata =
            br#"{"target_directory":"/w/target","build_directory":"/w/target","workspace_root":"/w"}"#;
        assert_eq!(parse_build_dirs(metadata), vec![PathBuf::from("/w/target")]);

        let metadata = br#"{"target_directory":"/w/target","build_directory":"/w/.build"}"#;
        assert_eq!(
            parse_build_dirs(metadata),
            vec![PathBuf::from("/w/.build"), PathBuf::from("/w/target")]
        );
        assert!(parse_build_dirs(b"not json").is_empty());
    }

    #[test]
    fn clone_tree_preserves_content_and_mtimes_or_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("debug/deps")).unwrap();
        std::fs::write(src.join("CACHEDIR.TAG"), "tag").unwrap();
        std::fs::write(src.join("debug/deps/libdep.rlib"), "rlib").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(src.join("debug/deps/libdep.rlib"))
            .unwrap()
            .set_modified(old)
            .unwrap();

        let dst = tmp.path().join("dst");
        if clone_tree(&src, &dst).unwrap() {
            let cloned = dst.join("debug/deps/libdep.rlib");
            assert_eq!(std::fs::read_to_string(&cloned).unwrap(), "rlib");
            assert_eq!(
                std::fs::read_to_string(dst.join("CACHEDIR.TAG")).unwrap(),
                "tag"
            );
            assert_eq!(std::fs::metadata(&cloned).unwrap().modified().unwrap(), old);
            std::fs::write(&cloned, "rebuilt").unwrap();
            assert_eq!(
                std::fs::read_to_string(src.join("debug/deps/libdep.rlib")).unwrap(),
                "rlib"
            );
        } else {
            assert!(!dst.exists());
        }
    }
}
