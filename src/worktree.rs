use std::path::{Path, PathBuf};

/// One entry of `git worktree list --porcelain -z`.
#[derive(Debug, Clone, Default)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_locked: bool,
    pub is_prunable: bool,
}

impl Worktree {
    /// The main worktree is always the first entry emitted by git.
    pub fn parse_list(bytes: &[u8]) -> Vec<Worktree> {
        let mut worktrees = Vec::new();
        let mut current: Option<Worktree> = None;
        for field in bytes.split(|&b| b == 0) {
            let field = String::from_utf8_lossy(field);
            if field.is_empty() {
                if let Some(wt) = current.take() {
                    worktrees.push(wt);
                }
                continue;
            }
            let (key, value) = match field.split_once(' ') {
                Some((k, v)) => (k, v),
                None => (field.as_ref(), ""),
            };
            match key {
                "worktree" => {
                    if let Some(wt) = current.take() {
                        worktrees.push(wt);
                    }
                    current = Some(Worktree {
                        path: PathBuf::from(value),
                        ..Default::default()
                    });
                }
                _ => {
                    let Some(wt) = current.as_mut() else { continue };
                    match key {
                        "HEAD" => wt.head = Some(value.to_string()),
                        "branch" => {
                            wt.branch = Some(
                                value
                                    .strip_prefix("refs/heads/")
                                    .unwrap_or(value)
                                    .to_string(),
                            );
                        }
                        "bare" => wt.is_bare = true,
                        "detached" => wt.is_detached = true,
                        "locked" => wt.is_locked = true,
                        "prunable" => wt.is_prunable = true,
                        _ => {}
                    }
                }
            }
        }
        if let Some(wt) = current.take() {
            worktrees.push(wt);
        }
        worktrees
    }
}

/// Where the worktree for a normalized `branch` lives. Slash-separated branch
/// segments map verbatim to nested directories; git forbids ref conflicts
/// (`foo` vs `foo/bar`), so this is collision-free with respect to branches.
pub fn path_for_branch(repo_dir: &Path, branch: &str) -> PathBuf {
    let mut path = repo_dir.to_path_buf();
    for segment in branch.split('/') {
        path.push(segment);
    }
    path
}

/// A directory is a linked worktree checkout iff it contains a `.git` *file*
/// (the main worktree has a `.git` directory; intermediate dirs have neither).
pub fn is_worktree_dir(path: &Path) -> bool {
    path.join(".git").is_file()
}

/// After removing `removed`, delete now-empty parent directories walking up
/// until (and excluding) `root`. Also removes `removed` itself if it is an
/// empty leftover directory.
pub fn cleanup_empty_dirs(removed: &Path, root: &Path) {
    let mut dir = removed.to_path_buf();
    loop {
        if !dir.starts_with(root) || dir == root {
            break;
        }
        match std::fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(_) => break, // non-empty or already gone with non-empty parent
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
}

/// The repo-id is the worktree's path relative to the root, minus the branch
/// segments (`<root>/<repo-id>/<branch dirs...>`).
pub fn repo_id_of(root: &Path, path: &Path, branch: Option<&str>) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let segments: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let branch_segments = branch.map_or(1, |b| b.split('/').count());
    let keep = segments.len().checked_sub(branch_segments)?;
    if keep == 0 {
        return None;
    }
    Some(segments[..keep].join("/"))
}

/// Recursively find worktree checkout dirs (dirs containing a `.git` file)
/// under `root`, without descending into checkouts themselves.
pub fn find_worktree_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if is_worktree_dir(&path) {
                found.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_z_output() {
        let bytes = b"worktree /repo\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0worktree /b/feat\0HEAD 2222222222222222222222222222222222222222\0branch refs/heads/feature/login\0locked reason\0\0worktree /b/det\0HEAD 3333333333333333333333333333333333333333\0detached\0prunable gitdir file points to non-existent location\0\0";
        let wts = Worktree::parse_list(bytes);
        assert_eq!(wts.len(), 3);
        assert_eq!(wts[0].path, PathBuf::from("/repo"));
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(!wts[0].is_locked);
        assert_eq!(wts[1].branch.as_deref(), Some("feature/login"));
        assert!(wts[1].is_locked);
        assert!(wts[2].is_detached);
        assert!(wts[2].is_prunable);
        assert_eq!(wts[2].branch, None);
    }

    #[test]
    fn parses_bare_entry() {
        let bytes = b"worktree /repo.git\0bare\0\0";
        let wts = Worktree::parse_list(bytes);
        assert_eq!(wts.len(), 1);
        assert!(wts[0].is_bare);
    }

    #[test]
    fn parses_empty_input() {
        assert!(Worktree::parse_list(b"").is_empty());
    }

    #[test]
    fn repo_id_strips_branch_segments() {
        let root = Path::new("/b");
        let cases = [
            (
                "/b/github.com/o/r/feat",
                Some("feat"),
                Some("github.com/o/r"),
            ),
            (
                "/b/github.com/o/r/feature/login",
                Some("feature/login"),
                Some("github.com/o/r"),
            ),
            ("/b/local/x-1234/feat", None, Some("local/x-1234")),
            ("/b/feat", Some("feat"), None),
            ("/elsewhere/feat", Some("feat"), None),
        ];
        for (path, branch, expected) in cases {
            assert_eq!(
                repo_id_of(root, Path::new(path), branch).as_deref(),
                expected,
                "path: {path}"
            );
        }
    }

    #[test]
    fn branch_path_nests_slashes() {
        assert_eq!(
            path_for_branch(Path::new("/b/gh/o/r"), "feature/login"),
            PathBuf::from("/b/gh/o/r/feature/login")
        );
    }

    #[test]
    fn cleanup_stops_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let leaf = root.join("gh/o/r/feature/login");
        std::fs::create_dir_all(&leaf).unwrap();
        cleanup_empty_dirs(&leaf, &root);
        assert!(!root.join("gh").exists());
        assert!(root.exists());
    }

    #[test]
    fn cleanup_keeps_non_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("gh/o/r/a")).unwrap();
        std::fs::create_dir_all(root.join("gh/o/r/b")).unwrap();
        cleanup_empty_dirs(&root.join("gh/o/r/a"), &root);
        assert!(!root.join("gh/o/r/a").exists());
        assert!(root.join("gh/o/r/b").exists());
    }
}
