use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::git::Git;
use crate::parallel;
use crate::paths::canonicalize_or_self;
use crate::repo::{Repo, WorktreeKind};
use crate::worktree::{Worktree, current_branch};

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub root: PathBuf,
    pub projects: Vec<Project>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub remote: Option<String>,
    pub worktrees: Vec<WorktreeEntry>,
}

#[derive(Debug, Serialize)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub main: bool,
    pub external: bool,
    pub locked: bool,
    pub prunable: bool,
    pub dirty: Option<bool>,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub untracked: usize,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Default, PartialEq)]
struct Status {
    added: usize,
    modified: usize,
    deleted: usize,
    untracked: usize,
    ahead: usize,
    behind: usize,
}

impl Status {
    fn dirty(&self) -> bool {
        self.added + self.modified + self.deleted + self.untracked > 0
    }
}

pub fn snapshot(config: &Config, initial_repo: Option<&Path>) -> Result<Snapshot> {
    let root = config.root_dir();
    let mut warnings = Vec::new();
    let mut paths = candidates(&root, &mut warnings);
    if let Some(path) = initial_repo {
        paths.insert(canonicalize_or_self(path));
    }
    let paths: Vec<_> = paths.into_iter().collect();
    let discovered = parallel::map_ordered(&paths, |path| {
        Git::at(path)
            .out(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map(|common| canonicalize_or_self(Path::new(&common)))
    });
    // The common Git directory identifies a clone, even when multiple clones
    // have the same remote or their managed paths predate a remote rename.
    let mut repositories = BTreeMap::new();
    let mut orphans: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (path, discovery) in paths.into_iter().zip(discovered) {
        match discovery {
            Ok(common) => {
                repositories.entry(common).or_insert(path);
            }
            Err(error) if path.join(".git").is_file() => {
                warnings.push(format!("{}: {error}", path.display()));
                let id = orphan_repository(&path).unwrap_or_else(|| "Unavailable checkouts".into());
                orphans.entry(id).or_default().push(path);
            }
            Err(_) => {}
        }
    }
    let mut projects = Vec::new();
    // Projects are processed sequentially; per-worktree status probes share
    // the existing bounded worker pool instead of nesting pools per project.
    for (common, source) in repositories {
        match project(config, &common, &source) {
            Ok((project, messages)) => {
                projects.push(project);
                warnings.extend(messages);
            }
            Err(error) => warnings.push(format!("{}: {error:#}", source.display())),
        }
    }
    for (id, paths) in orphans {
        let path = paths[0].clone();
        projects.push(Project {
            id: format!("unavailable:{id}"),
            name: Path::new(&id)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
            remote: None,
            worktrees: paths
                .into_iter()
                .map(|path| {
                    let kind = if path.starts_with(&root) {
                        WorktreeKind::Managed
                    } else {
                        WorktreeKind::External
                    };
                    entry(
                        Worktree {
                            branch: current_branch(&path),
                            path,
                            is_prunable: true,
                            ..Default::default()
                        },
                        kind,
                        None,
                    )
                })
                .collect(),
        });
    }
    projects.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(Snapshot {
        root,
        projects,
        warnings,
    })
}

fn project(config: &Config, common: &Path, source: &Path) -> Result<(Project, Vec<String>)> {
    let git = Git::at(source);
    let registered =
        Worktree::parse_list(&git.out_bytes(&["worktree", "list", "--porcelain", "-z"])?);
    let main = registered
        .first()
        .ok_or_else(|| anyhow::anyhow!("no registered worktrees"))?;
    let main_is_bare = main.is_bare;
    let repo = Repo {
        main_root: main.path.clone(),
        git,
    };
    let remote = repo
        .remote_name(config)
        .and_then(|name| repo.git.out(&["remote", "get-url", &name]).ok());
    let name = if remote.is_some() {
        repo.id(config)
            .rsplit('/')
            .next()
            .unwrap_or("project")
            .to_string()
    } else {
        repo.main_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let worktrees: Vec<_> = repo
        .project_worktrees(config)?
        .into_iter()
        .filter(|wt| !wt.worktree.is_bare)
        .collect();
    let action_path = if !main_is_bare && repo.main_root.is_dir() {
        repo.main_root.clone()
    } else {
        worktrees
            .iter()
            .find(|wt| wt.worktree.path.is_dir())
            .map(|wt| wt.worktree.path.clone())
            .unwrap_or_else(|| source.to_path_buf())
    };
    let statuses = parallel::map_ordered(&worktrees, |wt| {
        Git::at(&wt.worktree.path)
            .out_bytes(&[
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=normal",
            ])
            .map(|output| parse_status(&output))
    });
    let mut warnings = Vec::new();
    let worktrees = worktrees
        .into_iter()
        .zip(statuses)
        .map(|(wt, status)| {
            let status = match status {
                Ok(status) => Some(status),
                Err(error) => {
                    warnings.push(format!(
                        "{}: status unavailable: {error}",
                        wt.worktree.path.display()
                    ));
                    None
                }
            };
            entry(wt.worktree, wt.kind, status)
        })
        .collect();
    Ok((
        Project {
            id: common.to_string_lossy().into_owned(),
            name,
            path: action_path,
            remote,
            worktrees,
        },
        warnings,
    ))
}

fn entry(wt: Worktree, kind: WorktreeKind, status: Option<Status>) -> WorktreeEntry {
    let dirty = status.as_ref().map(Status::dirty);
    let status = status.unwrap_or_default();
    WorktreeEntry {
        path: canonicalize_or_self(&wt.path),
        branch: wt.branch,
        head: wt.head.unwrap_or_default(),
        main: kind == WorktreeKind::Main,
        external: kind == WorktreeKind::External,
        locked: wt.is_locked,
        prunable: wt.is_prunable,
        dirty,
        added: status.added,
        modified: status.modified,
        deleted: status.deleted,
        untracked: status.untracked,
        ahead: status.ahead,
        behind: status.behind,
    }
}

fn candidates(root: &Path, warnings: &mut Vec<String>) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if !visited.insert(canonicalize_or_self(&dir)) {
            continue;
        }
        if dir.join(".git").exists() {
            found.insert(canonicalize_or_self(&dir));
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(format!("{}: {error}", dir.display()));
                continue;
            }
        };
        for item in entries.flatten() {
            let path = item.path();
            let Ok(kind) = item.file_type() else { continue };
            if (kind.is_dir() || (kind.is_symlink() && path.is_dir())) && item.file_name() != ".git"
            {
                stack.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "code-workspace")
            {
                // Workspace files retain references to main/external checkouts
                // that a scan of managed directories alone cannot discover.
                if item.metadata().is_ok_and(|m| m.len() <= 2 * 1024 * 1024)
                    && let Ok(contents) = std::fs::read(&path)
                    && let Ok(workspace) = serde_json::from_slice::<serde_json::Value>(&contents)
                    && let Some(folders) = workspace.get("folders").and_then(|f| f.as_array())
                {
                    for folder in folders {
                        if let Some(folder) = folder.get("path").and_then(|p| p.as_str()) {
                            found.insert(canonicalize_or_self(&dir.join(folder)));
                        }
                    }
                }
            }
        }
    }
    found
}

fn orphan_repository(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path.join(".git")).ok()?;
    let target = path.join(contents.strip_prefix("gitdir:")?.trim());
    let worktrees = target.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees.parent()?;
    let project = if common.file_name()? == ".git" {
        common.parent()?
    } else {
        common
    };
    Some(
        crate::paths::canonicalize_lenient(project)
            .display()
            .to_string(),
    )
}

fn parse_status(bytes: &[u8]) -> Status {
    let mut status = Status::default();
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if let Some(tracking) = record.strip_prefix(b"# branch.ab ") {
            let tracking = String::from_utf8_lossy(tracking);
            let mut counts = tracking.split_whitespace();
            status.ahead = counts
                .next()
                .and_then(|n| n.strip_prefix('+'))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            status.behind = counts
                .next()
                .and_then(|n| n.strip_prefix('-'))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if record.starts_with(b"? ") {
            status.untracked += 1;
        } else if matches!(record.first(), Some(b'1' | b'2' | b'u')) {
            let xy = record.get(2..4).unwrap_or_default();
            if record.starts_with(b"u ") {
                status.modified += 1;
            } else if xy.contains(&b'A') {
                status.added += 1;
            } else if xy.contains(&b'D') {
                status.deleted += 1;
            } else {
                status.modified += 1;
            }
            if record.starts_with(b"2 ") {
                records.next();
            }
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_counts_changes_tracking_and_skips_rename_source() {
        let status = parse_status(
            b"# branch.oid abc\0# branch.ab +12 -3\0\
            1 A. N... 000000 100644 100644 a b added file\0\
            1 .M N... 100644 100644 100644 a b modified\0\
            1 D. N... 100644 000000 000000 a b deleted\0\
            2 R. N... 100644 100644 100644 a b R100 renamed\0? fake untracked\0\
            u UU N... 100644 100644 100644 100644 a b c conflict\0\
            ? file\nwith newline\0! ignored\0",
        );
        assert_eq!(
            status,
            Status {
                added: 1,
                modified: 3,
                deleted: 1,
                untracked: 1,
                ahead: 12,
                behind: 3
            }
        );
        assert!(status.dirty());
    }

    #[test]
    fn tracking_headers_do_not_make_a_clean_checkout_dirty() {
        let status = parse_status(b"# branch.oid abc\0# branch.head main\0# branch.ab +1 -2\0");
        assert!(!status.dirty());
        assert_eq!((status.ahead, status.behind), (1, 2));
    }

    #[test]
    fn scan_includes_workspace_folders_and_stops_at_checkouts() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("host/owner/repo/ab/feature");
        std::fs::create_dir_all(checkout.join("nested/.git")).unwrap();
        std::fs::write(checkout.join(".git"), "gitdir: missing").unwrap();
        std::fs::write(
            dir.path().join("project.code-workspace"),
            r#"{"folders":[{"path":"../main-checkout"}]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let candidates = candidates(dir.path(), &mut warnings);
        assert!(candidates.contains(&canonicalize_or_self(&checkout)));
        assert!(candidates.contains(&canonicalize_or_self(&dir.path().join("../main-checkout"))));
        assert_eq!(candidates.len(), 2);
        assert!(warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_directory_symlink_cycles() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        assert!(candidates(dir.path(), &mut Vec::new()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_includes_linked_project_directories_once() {
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::create_dir(external.path().join(".git")).unwrap();
        for name in ["first", "second"] {
            std::os::unix::fs::symlink(external.path(), dir.path().join(name)).unwrap();
        }
        let paths = candidates(dir.path(), &mut Vec::new());
        assert_eq!(
            paths,
            BTreeSet::from([canonicalize_or_self(external.path())])
        );
    }

    #[test]
    fn orphan_identity_uses_missing_shared_metadata_instead_of_branch_segments() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("missing-project");
        std::fs::write(
            dir.path().join(".git"),
            format!(
                "gitdir: {}\n",
                project.join(".git/worktrees/feature").display()
            ),
        )
        .unwrap();
        assert_eq!(
            orphan_repository(dir.path()),
            Some(
                crate::paths::canonicalize_lenient(&project)
                    .display()
                    .to_string()
            )
        );
    }
}
