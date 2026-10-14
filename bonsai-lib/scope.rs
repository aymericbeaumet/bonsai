//! Which projects a command acts on: the current repository by default, or
//! every project under the Bonsai root with `-a/--all` and outside a
//! repository.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::git::Git;
use crate::parallel;
use crate::paths::canonicalize_or_self;
use crate::picker;
use crate::repo::Repo;
use crate::worktree::Worktree;

/// A repository with the configuration that applies to it.
pub struct Project {
    pub repo: Repo,
    pub config: Config,
}

impl Project {
    pub fn id(&self) -> String {
        self.repo.id(&self.config)
    }
}

pub enum Scope {
    Current(Box<Project>),
    All(Vec<Project>),
}

impl Scope {
    /// Inside a repository the scope is that repository unless `all` widens
    /// it; outside one it is every project.
    pub fn resolve(
        config: &Config,
        current: Option<Project>,
        all: bool,
        remote: Option<&str>,
    ) -> Scope {
        match current {
            Some(project) if !all => Scope::Current(Box::new(project)),
            current => Scope::All(discover(config, current, remote)),
        }
    }

    pub fn projects(&self) -> &[Project] {
        match self {
            Scope::Current(project) => std::slice::from_ref(project),
            Scope::All(projects) => projects,
        }
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Scope::All(_))
    }

    /// The single project a command needs: the current one, or a picked one
    /// when every project is in scope.
    pub fn into_project(self) -> Result<Project> {
        let mut projects = match self {
            Scope::Current(project) => return Ok(*project),
            Scope::All(projects) => projects,
        };
        if projects.is_empty() {
            bail!("no projects found; run inside a git repository or pass -C <dir>");
        }
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            bail!("not inside a git repository; run from a project or pass -C <dir>");
        }
        let rows = projects
            .iter()
            .map(|project| {
                let id = project.id();
                let path = project.repo.main_root.display().to_string();
                picker::RecentRow {
                    search: format!("{id} {path}"),
                    columns: vec![id, path],
                    last_change: None,
                }
            })
            .collect::<Vec<_>>();
        let picked = picker::select_styled("Project:", picker::recent_options(&rows), None)?;
        Ok(projects.swap_remove(picked))
    }
}

/// Every project with a checkout under the root (or referenced by a
/// workspace file there), plus the current one, ordered by id. Clones are
/// identified by their common Git directory.
fn discover(config: &Config, current: Option<Project>, remote: Option<&str>) -> Vec<Project> {
    let root = config.root_dir();
    let common_dir = |git: &Git| {
        git.out(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map(|common| canonicalize_or_self(Path::new(&common)))
    };
    let mut known = BTreeSet::new();
    if let Some(project) = &current
        && let Ok(common) = common_dir(&project.repo.git)
    {
        known.insert(common);
    }
    let paths = candidates(&root, &mut Vec::new())
        .into_iter()
        .collect::<Vec<_>>();
    let commons = parallel::map_ordered(&paths, |path| common_dir(&Git::at(path)));
    let mut sources = BTreeMap::new();
    for (path, common) in paths.into_iter().zip(commons) {
        if let Ok(common) = common
            && !known.contains(&common)
        {
            sources.entry(common).or_insert(path);
        }
    }
    let sources = sources.into_values().collect::<Vec<_>>();
    let cwd = std::env::current_dir().ok();
    let built = parallel::map_ordered(&sources, |source| {
        open(config, source, cwd.as_deref(), remote)
            .with_context(|| format!("skipping {}", source.display()))
    });
    let mut projects = current.into_iter().collect::<Vec<_>>();
    for project in built {
        match project {
            Ok(project) => projects.push(project),
            Err(error) => eprintln!("bonsai: {error:#}"),
        }
    }
    let mut keyed = projects
        .into_iter()
        .map(|project| (project.id(), project))
        .collect::<Vec<_>>();
    keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
    keyed.into_iter().map(|(_, project)| project).collect()
}

/// Load a project from any of its checkouts with its own configuration.
/// The root stays the invocation's so every project shares one layout.
fn open(
    config: &Config,
    source: &Path,
    cwd: Option<&Path>,
    remote: Option<&str>,
) -> Result<Project> {
    let registered = Worktree::parse_list(&Git::at(source).out_bytes(&[
        "worktree",
        "list",
        "--porcelain",
        "-z",
    ])?);
    let main = registered.first().context("no registered worktrees")?;
    let anchor = if main.path.is_dir() {
        main.path.clone()
    } else {
        source.to_path_buf()
    };
    let toml_dir = (!main.is_bare && main.path.is_dir()).then(|| main.path.clone());
    let git = Git::at(&anchor);
    let mut project_config = Config::load(toml_dir.as_deref(), &git)?;
    project_config.root = config.root.clone();
    if let Some(remote) = remote {
        project_config.remote = Some(remote.to_string());
    }
    Ok(Project {
        repo: Repo::from_worktrees(git, registered, cwd)?,
        config: project_config,
    })
}

/// Checkouts under `root`, plus folders referenced by workspace files there.
pub fn candidates(root: &Path, warnings: &mut Vec<String>) -> BTreeSet<PathBuf> {
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
                    && let Ok(contents) = std::fs::read_to_string(&path)
                    && let Ok(workspace) = jsonc_parser::parse_to_serde_value::<serde_json::Value>(
                        &contents,
                        &jsonc_parser::ParseOptions {
                            allow_comments: true,
                            allow_trailing_commas: true,
                            allow_loose_object_property_names: false,
                            allow_missing_commas: false,
                            allow_single_quoted_strings: false,
                            allow_hexadecimal_numbers: false,
                            allow_unary_plus_numbers: false,
                            allow_bare_decimal_point_numbers: false,
                            allow_non_finite_numbers: false,
                            allow_extended_string_escapes: false,
                        },
                    )
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
