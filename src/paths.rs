use std::path::{Path, PathBuf};

/// Lock files are persistent, empty coordination artifacts. Never use a link
/// to some other file as one, even though acquiring a lock does not alter bytes.
pub fn open_lock_file(path: &Path) -> anyhow::Result<std::fs::File> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(
            metadata.file_type().is_file(),
            "invalid lock file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let path_metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        path_metadata.file_type().is_file(),
        "lock file changed: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened_metadata = file.metadata()?;
        anyhow::ensure!(
            opened_metadata.nlink() == 1
                && opened_metadata.dev() == path_metadata.dev()
                && opened_metadata.ino() == path_metadata.ino(),
            "lock file is linked or changed: {}",
            path.display()
        );
    }
    Ok(file)
}

/// Ordinary repository mutations share this lock; a whole-root prune needs
/// exclusive access so it cannot mistake an in-progress add for an orphan.
pub fn lock_root(root: &Path, exclusive: bool) -> anyhow::Result<std::fs::File> {
    use anyhow::Context;
    std::fs::create_dir_all(root)?;
    let directory = root.join(".locks");
    ensure_contained(&directory, root)?;
    std::fs::create_dir_all(&directory)?;
    ensure_contained(&directory, root)?;
    let path = directory.join("mutations.lock");
    let file = open_lock_file(&path)?;
    ensure_contained(&path, root)?;
    if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    }
    .context("another bonsai operation is using this root; retry when it finishes")?;
    Ok(file)
}

/// Resolve existing ancestors without treating inaccessible paths as missing.
/// Both explicit and inferred destinations must remain inside the managed root.
pub fn ensure_contained(path: &Path, root: &Path) -> anyhow::Result<PathBuf> {
    fn resolve(path: &Path) -> std::io::Result<PathBuf> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => std::fs::canonicalize(path).map(simplify),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = path.parent().ok_or(error)?;
                let name = path
                    .file_name()
                    .ok_or_else(|| std::io::Error::other("path has no final component"))?;
                Ok(resolve(parent)?.join(name))
            }
            Err(error) => Err(error),
        }
    }
    anyhow::ensure!(
        !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir)),
        "path must not contain '..': {}",
        path.display()
    );
    let root = resolve(root)?;
    let destination = resolve(path)?;
    anyhow::ensure!(
        destination.starts_with(&root) && destination != root,
        "path {} must be inside the bonsai root {}",
        path.display(),
        root.display()
    );
    Ok(destination)
}

/// Canonicalize for comparison and storage. On Windows,
/// `std::fs::canonicalize` returns verbatim paths (`\\?\C:\...`) that git
/// neither emits nor accepts, so the prefix is stripped back off — otherwise
/// prefix comparisons against git-reported paths always fail.
pub fn canonicalize_ok(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().map(simplify)
}

/// Like `canonicalize_ok`, falling back to the original path.
pub fn canonicalize_or_self(path: &Path) -> PathBuf {
    canonicalize_ok(path).unwrap_or_else(|| path.to_path_buf())
}

/// Canonicalize as much of the path as exists, keeping the rest verbatim
/// (used for roots that may not have been created yet).
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Some(canonical) = canonicalize_ok(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonicalize_lenient(parent).join(name),
        _ => path.to_path_buf(),
    }
}

#[cfg(windows)]
fn simplify(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    // \\?\C:\x -> C:\x, but leave \\?\UNC\server\share alone.
    if let Some(rest) = s.strip_prefix(r"\\?\")
        && !rest.starts_with("UNC")
    {
        return PathBuf::from(rest);
    }
    path
}

#[cfg(not(windows))]
fn simplify(path: PathBuf) -> PathBuf {
    path
}
