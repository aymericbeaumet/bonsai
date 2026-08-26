use std::path::{Path, PathBuf};

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
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        // \\?\C:\x -> C:\x, but leave \\?\UNC\server\share alone.
        if !rest.starts_with("UNC") {
            return PathBuf::from(rest);
        }
    }
    path
}

#[cfg(not(windows))]
fn simplify(path: PathBuf) -> PathBuf {
    path
}
