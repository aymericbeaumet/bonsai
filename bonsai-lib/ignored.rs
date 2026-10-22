//! Git-ignored content of a checkout (node_modules, target, .venv, ...):
//! what `trim` reclaims and what removal deletes concurrently before git
//! unregisters a worktree. Tree walks and deletions fan out with rayon.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use crate::config::Config;
use crate::git::Git;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Relative to the worktree.
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepReason {
    /// Matches `[add] copy` or `[trim] keep`: local config, not artifacts.
    Protected,
    /// A nested repository, which may hold unpushed work of its own.
    Repository,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kept {
    pub path: PathBuf,
    pub reason: KeepReason,
}

/// Ignored paths to delete, outermost first, and the ignored paths kept.
#[derive(Debug, Default, Serialize)]
pub struct Plan {
    pub entries: Vec<Entry>,
    pub kept: Vec<Kept>,
}

impl Plan {
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }

    fn merge(&mut self, other: Plan) {
        self.entries.extend(other.entries);
        self.kept.extend(other.kept);
    }
}

/// Ignored local state that no reinstall or rebuild restores: always kept,
/// on top of `[add] copy` and `[trim] keep`.
pub const LOCAL_STATE: &[&str] = &[
    ".env",
    ".env.*",
    ".envrc",
    "*.local",
    "*.local.*",
    "*.sqlite",
    "*.sqlite3",
    "*.db",
    "*.tfstate",
    "*.tfstate.*",
    "*.tfvars",
    "*.key",
    "*.pem",
    "*.p12",
    ".idea",
    ".vscode",
];

/// Paths `trim` never deletes. Like gitignore patterns, a glob without `/`
/// matches a file or directory name at any depth (`.env.*` keeps
/// `apps/web/.env.local`); one with `/` matches from the worktree root, and
/// `*` never crosses `/`. Everything below a kept directory is kept.
pub struct Keep {
    anywhere: Vec<glob::Pattern>,
    anchored: Vec<glob::Pattern>,
}

impl Keep {
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            LOCAL_STATE
                .iter()
                .copied()
                .chain(config.add.copy.iter().map(String::as_str))
                .chain(config.trim.keep.iter().map(String::as_str)),
        )
    }

    fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        let mut keep = Self {
            anywhere: Vec::new(),
            anchored: Vec::new(),
        };
        for raw in patterns {
            let pattern = raw.trim_end_matches('/');
            let (list, pattern) = match pattern.strip_prefix('/') {
                Some(rooted) => (&mut keep.anchored, rooted),
                None if pattern.contains('/') => (&mut keep.anchored, pattern),
                None => (&mut keep.anywhere, pattern),
            };
            match glob::Pattern::new(pattern) {
                Ok(pattern) => list.push(pattern),
                Err(error) => eprintln!("bonsai: invalid keep glob '{raw}': {error}"),
            }
        }
        keep
    }

    pub fn protects(&self, rel: &Path) -> bool {
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        rel.ancestors()
            .filter(|path| !path.as_os_str().is_empty())
            .any(|path| {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default();
                let full = path.to_string_lossy().replace('\\', "/");
                self.anywhere
                    .iter()
                    .any(|pattern| pattern.matches_with(&name, options))
                    || self
                        .anchored
                        .iter()
                        .any(|pattern| pattern.matches_with(&full, options))
            })
    }
}

/// Outermost ignored paths: wholly ignored directories are reported once,
/// without descending (git's `--directory`).
pub fn roots(worktree: &Path) -> Result<Vec<PathBuf>> {
    let output = Git::at(worktree).out_bytes(&[
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ])?;
    let paths = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(path_from_git)
        .collect();
    Ok(outermost(paths))
}

/// What `trim` would delete from `worktree`, with disk usage. Protected
/// paths and nested repositories are kept; ignored directories holding them
/// are split so everything else inside still goes.
pub fn plan(worktree: &Path, keep: &Keep) -> Result<Plan> {
    let visits: Vec<(PathBuf, Visit)> = roots(worktree)?
        .into_par_iter()
        .map(|rel| {
            let visit = visit(worktree, &rel, keep);
            (rel, visit)
        })
        .collect();
    let mut plan = collect(visits);
    plan.entries
        .sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    plan.kept.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(plan)
}

/// A visited path is either deletable as a whole (with its disk usage) or
/// partially kept, in which case its deletable parts are listed separately.
enum Visit {
    Whole(u64),
    Partial(Plan),
}

fn visit(root: &Path, rel: &Path, keep: &Keep) -> Visit {
    if keep.protects(rel) {
        return kept(rel, KeepReason::Protected);
    }
    let full = root.join(rel);
    let Ok(meta) = fs::symlink_metadata(&full) else {
        return Visit::Whole(0); // vanished meanwhile
    };
    if !meta.is_dir() {
        return Visit::Whole(disk_usage(&meta));
    }
    if fs::symlink_metadata(full.join(".git")).is_ok() {
        return kept(rel, KeepReason::Repository);
    }
    let Ok(children) = fs::read_dir(&full) else {
        return Visit::Partial(Plan::default()); // unreadable: leave it
    };
    let children: Vec<PathBuf> = children
        .flatten()
        .map(|child| rel.join(child.file_name()))
        .collect();
    let visits: Vec<(PathBuf, Visit)> = children
        .into_par_iter()
        .map(|child| {
            let visit = visit(root, &child, keep);
            (child, visit)
        })
        .collect();
    if visits
        .iter()
        .all(|(_, visit)| matches!(visit, Visit::Whole(_)))
    {
        let total = visits
            .iter()
            .map(|(_, visit)| match visit {
                Visit::Whole(bytes) => *bytes,
                Visit::Partial(_) => 0,
            })
            .sum::<u64>();
        Visit::Whole(disk_usage(&meta) + total)
    } else {
        Visit::Partial(collect(visits))
    }
}

fn kept(rel: &Path, reason: KeepReason) -> Visit {
    Visit::Partial(Plan {
        entries: Vec::new(),
        kept: vec![Kept {
            path: rel.to_path_buf(),
            reason,
        }],
    })
}

fn collect(visits: Vec<(PathBuf, Visit)>) -> Plan {
    let mut plan = Plan::default();
    for (path, visit) in visits {
        match visit {
            Visit::Whole(bytes) => plan.entries.push(Entry { path, bytes }),
            Visit::Partial(partial) => plan.merge(partial),
        }
    }
    plan
}

/// Keep only paths without a listed ancestor (git may list both `dir/` and
/// `dir/file`), sorted.
fn outermost(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let set: std::collections::HashSet<PathBuf> = paths.iter().cloned().collect();
    paths.retain(|path| {
        !path
            .ancestors()
            .skip(1)
            .any(|ancestor| !ancestor.as_os_str().is_empty() && set.contains(ancestor))
    });
    paths.sort();
    paths.dedup();
    paths
}

fn path_from_git(field: &[u8]) -> PathBuf {
    let field = field.strip_suffix(b"/").unwrap_or(field);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(field))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(field).into_owned())
    }
}

/// Allocated size, like `du` (sparse files and filesystem blocks count as
/// they do on disk); the apparent length where blocks are unavailable.
fn disk_usage(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Levels of a tree deleted with explicit fan-out before handing subtrees to
/// `remove_dir_all`: wide enough to spread node_modules/<pkg> and
/// target/<profile>/<kind>/* across cores.
const FAN_OUT_DEPTH: usize = 3;

/// `rm -rf` spread across cores. Symlinks are removed, never followed, and
/// a path that is already gone counts as removed.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    remove_fanned(path, FAN_OUT_DEPTH)
}

fn remove_fanned(path: &Path, depth: usize) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let result = if !meta.is_dir() {
        match fs::remove_file(path) {
            // Windows removes directory symlinks and junctions as directories.
            Err(_) if meta.file_type().is_symlink() => fs::remove_dir(path),
            result => result,
        }
    } else if depth == 0 {
        fs::remove_dir_all(path)
    } else {
        let children = fs::read_dir(path).and_then(|children| {
            children
                .map(|child| child.map(|child| child.path()))
                .collect::<io::Result<Vec<_>>>()
        });
        let children = match children {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        // Delete everything possible, then report the first failure.
        let results: Vec<io::Result<()>> = children
            .par_iter()
            .map(|child| remove_fanned(child, depth - 1))
            .collect();
        results
            .into_iter()
            .collect::<io::Result<()>>()
            .and_then(|()| fs::remove_dir(path))
    };
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Delete the ignored paths of each checkout concurrently, right before
/// Git removes it: usually the bulk of a checkout, so Git is left with
/// little to do. Kept paths and nested repositories are left for Git's own
/// removal, so a worktree Git then refuses to remove keeps them; this skips
/// the deep walk `plan` does, deleting whole ignored directories at once.
pub fn purge(worktrees: &[(PathBuf, &Keep)]) {
    let work: Vec<PathBuf> = worktrees
        .par_iter()
        // Submodule checkouts stay for git's own removal checks.
        .filter(|(worktree, _)| !worktree.join(".gitmodules").exists())
        .flat_map_iter(|(worktree, keep)| {
            roots(worktree)
                .unwrap_or_default()
                .into_iter()
                .filter(|rel| !keep.protects(rel))
                .map(|rel| worktree.join(rel))
                .filter(|path| fs::symlink_metadata(path.join(".git")).is_err())
                .collect::<Vec<_>>()
        })
        .collect();
    work.par_iter().for_each(|path| {
        let _ = remove_tree(path); // best effort: git removes what remains
    });
}

/// Compact binary size: 512B, 12K, 3.4M, 1.2G.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else if value < 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keep(patterns: &[&str]) -> Keep {
        Keep::new(patterns.iter().copied())
    }

    #[test]
    fn outermost_drops_nested_entries_but_not_siblings_sharing_a_prefix() {
        let paths = ["src/x.log", "src", "src-other", "node_modules"]
            .map(PathBuf::from)
            .to_vec();
        assert_eq!(
            outermost(paths),
            ["node_modules", "src", "src-other"].map(PathBuf::from)
        );
    }

    #[test]
    fn keep_globs_match_like_gitignore_and_cover_everything_below() {
        let keep = keep(&[
            ".env.*",
            ".claude/settings.local.json",
            "data/",
            "/root-only",
        ]);
        // Name globs match at any depth.
        assert!(keep.protects(Path::new(".env.local")));
        assert!(keep.protects(Path::new("apps/web/.env.local")));
        // Path globs are anchored, and `*` does not cross `/`.
        assert!(keep.protects(Path::new(".claude/settings.local.json")));
        assert!(!keep.protects(Path::new(".claude")));
        assert!(keep.protects(Path::new("root-only")));
        assert!(
            keep.protects(Path::new("data/db.sqlite3")),
            "below a kept directory"
        );
        assert!(!keep.protects(Path::new("node_modules/x")));

        let defaults = Keep::new(LOCAL_STATE.iter().copied());
        for path in [
            "db.sqlite3",
            "infra/terraform.tfstate",
            "config/master.key",
            ".idea/workspace.xml",
        ] {
            assert!(defaults.protects(Path::new(path)), "{path}");
        }
        for path in [
            "node_modules/react/index.js",
            "target/debug/app",
            ".venv/bin/python",
        ] {
            assert!(!defaults.protects(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn visit_splits_directories_holding_kept_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("cache/deep")).unwrap();
        std::fs::write(root.join("cache/deep/blob"), "x").unwrap();
        std::fs::write(root.join("cache/.env"), "SECRET=1").unwrap();
        std::fs::create_dir_all(root.join("cache/vendored/.git")).unwrap();
        std::fs::create_dir_all(root.join("artifacts")).unwrap();
        std::fs::write(root.join("artifacts/out"), "y").unwrap();

        let keep = keep(&["cache/.env"]);
        let visits = ["artifacts", "cache"]
            .map(PathBuf::from)
            .into_iter()
            .map(|rel| {
                let visit = visit(root, &rel, &keep);
                (rel, visit)
            })
            .collect();
        let plan = collect(visits);
        let mut entries: Vec<_> = plan.entries.iter().map(|e| e.path.clone()).collect();
        entries.sort();
        assert_eq!(
            entries,
            ["artifacts", "cache/deep"].map(PathBuf::from).to_vec()
        );
        let mut kept: Vec<_> = plan
            .kept
            .iter()
            .map(|k| (k.path.clone(), k.reason))
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            vec![
                (PathBuf::from("cache/.env"), KeepReason::Protected),
                (PathBuf::from("cache/vendored"), KeepReason::Repository),
            ]
        );
    }

    #[test]
    fn remove_tree_deletes_deep_trees_and_links_without_following_them() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious"), "keep").unwrap();
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("a/b/c/d/e")).unwrap();
        std::fs::write(tree.join("a/b/c/d/e/f"), "x").unwrap();
        std::fs::write(tree.join("top"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, tree.join("a/link")).unwrap();

        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
        assert!(outside.join("precious").exists());
        remove_tree(&tree).unwrap(); // already gone
    }

    #[test]
    fn bytes_format_compactly() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(12 * 1024), "12K");
        assert_eq!(format_bytes(3_565_158), "3.4M");
        assert_eq!(format_bytes(1_288_490_189), "1.2G");
    }
}
