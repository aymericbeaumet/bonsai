use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::git::Git;
use crate::parallel;
use crate::paths::canonicalize_or_self;
use crate::repo::{Repo, WorktreeKind};
use crate::scope::candidates;
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
    /// The checkout directory is gone. Git status is not run for it.
    pub missing: bool,
    #[serde(rename = "lastActivity")]
    pub last_activity: Option<u64>,
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

pub fn snapshot(config: &Config, initial_repo: Option<&Path>, all: bool) -> Result<Snapshot> {
    let mut snapshot = discover(config, initial_repo, all)?;
    fill_status(&mut snapshot);
    Ok(snapshot)
}

/// Inventory without git status, so the terminal list can paint before status
/// probes finish. [`snapshot`] waits for those probes.
/// Without `all`, only the project containing `initial_repo` is discovered.
pub fn discover(config: &Config, initial_repo: Option<&Path>, all: bool) -> Result<Snapshot> {
    let root = config.root_dir();
    let mut warnings = Vec::new();
    let mut paths = if all {
        candidates(&root, &mut warnings)
    } else {
        Default::default()
    };
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
    // One pool for repository metadata. Status uses its own pool afterwards
    // so the two never nest.
    let repositories: Vec<(PathBuf, PathBuf)> = repositories.into_iter().collect();
    let built = parallel::map_ordered(&repositories, |(common, source)| {
        project(config, common, source)
    });
    for ((_, source), built) in repositories.into_iter().zip(built) {
        match built {
            Ok(project) => projects.push(project),
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
    for project in &projects {
        for worktree in &project.worktrees {
            if worktree.missing {
                let warning = format!("{}: missing checkout", worktree.path.display());
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
        }
    }
    Ok(Snapshot {
        root,
        projects,
        warnings,
    })
}

fn project(config: &Config, common: &Path, source: &Path) -> Result<Project> {
    let git = Git::at(source);
    let registered =
        Worktree::parse_list(&git.out_bytes(&["worktree", "list", "--porcelain", "-z"])?);
    let main = registered
        .first()
        .ok_or_else(|| anyhow::anyhow!("no registered worktrees"))?;
    let main_is_bare = main.is_bare;
    let repo = Repo::from_worktrees(git, registered, Some(source))?;
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
    let worktrees = worktrees
        .into_iter()
        .map(|wt| entry(wt.worktree, wt.kind, None))
        .collect();
    Ok(Project {
        id: common.to_string_lossy().into_owned(),
        name,
        path: action_path,
        remote,
        worktrees,
    })
}

fn entry(wt: Worktree, kind: WorktreeKind, status: Option<Status>) -> WorktreeEntry {
    let dirty = status.as_ref().map(Status::dirty);
    let status = status.unwrap_or_default();
    let path = canonicalize_or_self(&wt.path);
    WorktreeEntry {
        last_activity: crate::worktree::last_activity(&path)
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs()),
        missing: !path.exists(),
        path,
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

fn fill_status(snapshot: &mut Snapshot) {
    let paths = status_paths(snapshot);
    let probed = parallel::map_ordered(&paths, |path| probe_status(path));
    for (path, probed) in paths.iter().zip(probed) {
        record_status(snapshot, path, probed);
    }
}

/// Worktrees whose git status is worth probing. Broken checkouts are omitted.
pub fn status_paths(snapshot: &Snapshot) -> Vec<PathBuf> {
    snapshot
        .projects
        .iter()
        .filter(|project| !project.id.starts_with("unavailable:"))
        .flat_map(|project| {
            project
                .worktrees
                .iter()
                .filter(|worktree| !worktree.missing)
                .map(|worktree| worktree.path.clone())
        })
        .collect()
}

fn probe_status(path: &Path) -> Result<Status, String> {
    if !path.exists() {
        return Err(format!("{}: missing checkout", path.display()));
    }
    Git::at(path)
        .out_bytes(&[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ])
        .map(|output| parse_status(&output))
        .map_err(|error| {
            let detail = if error.stderr.is_empty() {
                match error.status {
                    Some(code) => format!("exit {code}"),
                    None => "git failed".to_string(),
                }
            } else {
                error
                    .stderr
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            format!("{}: status unavailable: {detail}", path.display())
        })
}

fn record_status(snapshot: &mut Snapshot, path: &Path, probed: Result<Status, String>) {
    let status = match probed {
        Ok(status) => Some(status),
        Err(warning) => {
            snapshot.warnings.push(warning);
            None
        }
    };
    let Some(status) = status else {
        return;
    };
    for project in &mut snapshot.projects {
        for worktree in &mut project.worktrees {
            if worktree.path == path {
                assign_status(worktree, &status);
            }
        }
    }
}

fn assign_status(entry: &mut WorktreeEntry, status: &Status) {
    entry.dirty = Some(status.dirty());
    entry.added = status.added;
    entry.modified = status.modified;
    entry.deleted = status.deleted;
    entry.untracked = status.untracked;
    entry.ahead = status.ahead;
    entry.behind = status.behind;
}

pub struct StatusUpdate {
    pub generation: u64,
    pub path: PathBuf,
    pub dirty: Option<bool>,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub untracked: usize,
    pub ahead: usize,
    pub behind: usize,
    pub warning: Option<String>,
}

pub struct StatusJob {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<StatusUpdate>,
}

impl StatusJob {
    pub fn try_recv(&self) -> Option<StatusUpdate> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for StatusJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start_status(paths: Vec<PathBuf>, generation: u64) -> StatusJob {
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    if !paths.is_empty() {
        let cancel_worker = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let next = AtomicUsize::new(0);
            let workers = paths.len().clamp(1, 8);
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    let sender = sender.clone();
                    let cancel = Arc::clone(&cancel_worker);
                    let paths = &paths;
                    let next = &next;
                    scope.spawn(move || {
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(path) = paths.get(index) else {
                                break;
                            };
                            let update = match probe_status(path) {
                                Ok(status) => StatusUpdate {
                                    generation,
                                    path: path.clone(),
                                    dirty: Some(status.dirty()),
                                    added: status.added,
                                    modified: status.modified,
                                    deleted: status.deleted,
                                    untracked: status.untracked,
                                    ahead: status.ahead,
                                    behind: status.behind,
                                    warning: None,
                                },
                                Err(warning) => StatusUpdate {
                                    generation,
                                    path: path.clone(),
                                    dirty: None,
                                    added: 0,
                                    modified: 0,
                                    deleted: 0,
                                    untracked: 0,
                                    ahead: 0,
                                    behind: 0,
                                    warning: Some(warning),
                                },
                            };
                            if sender.send(update).is_err() {
                                break;
                            }
                        }
                    });
                }
            });
        });
    }
    StatusJob { cancel, receiver }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

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

    #[test]
    fn scan_discovers_external_checkouts_from_preserved_jsonc_workspaces() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("managed");
        let checkout = directory.path().join("external checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let contents = r#"{
            // Keep my editor settings and custom folder name.
            "folders": [{"name": "My project", "path": "../external checkout",},],
            "settings": {"example.documentation": "https://example.test/docs",},
        }"#;
        let workspace = root.join("bonsai.code-workspace");
        std::fs::write(&workspace, contents).unwrap();

        let mut warnings = Vec::new();
        assert_eq!(
            candidates(&root, &mut warnings),
            BTreeSet::from([canonicalize_or_self(&checkout)])
        );
        assert!(warnings.is_empty());
        assert_eq!(std::fs::read_to_string(&workspace).unwrap(), contents);
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

    fn blank_entry(path: &str) -> WorktreeEntry {
        WorktreeEntry {
            path: path.into(),
            branch: None,
            head: String::new(),
            main: false,
            external: false,
            locked: false,
            prunable: false,
            missing: false,
            dirty: None,
            added: 0,
            modified: 0,
            deleted: 0,
            untracked: 0,
            ahead: 0,
            behind: 0,
            last_activity: None,
        }
    }

    #[test]
    fn status_paths_skip_unavailable_projects() {
        let snapshot = Snapshot {
            root: "/trees".into(),
            warnings: Vec::new(),
            projects: vec![
                Project {
                    id: "unavailable:lost".to_string(),
                    name: "lost".to_string(),
                    path: "/trees/lost".into(),
                    remote: None,
                    worktrees: vec![blank_entry("/trees/lost")],
                },
                Project {
                    id: "/repo/.git".to_string(),
                    name: "repo".to_string(),
                    path: "/trees/repo".into(),
                    remote: None,
                    worktrees: vec![
                        blank_entry("/trees/repo"),
                        WorktreeEntry {
                            missing: true,
                            ..blank_entry("/trees/gone")
                        },
                    ],
                },
            ],
        };
        assert_eq!(status_paths(&snapshot), vec![PathBuf::from("/trees/repo")]);
    }

    #[test]
    fn missing_checkout_does_not_invoke_git() {
        let missing = std::env::temp_dir().join(format!(
            "bonsai-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let error = probe_status(&missing).unwrap_err();
        assert!(error.contains("missing checkout"), "{error}");
        assert!(!error.contains("git "), "{error}");
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
