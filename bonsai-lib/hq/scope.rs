//! Resolve command targets from the selected row, independently of its view.

use std::path::PathBuf;

use super::Model;
use super::inventory::{Project, WorktreeEntry};
use super::owner;
use super::rows::{Depth, Info};

pub(super) struct Selection<'a> {
    pub(super) label: String,
    pub(super) projects: Vec<ProjectSelection<'a>>,
}

pub(super) struct ProjectSelection<'a> {
    pub(super) project: &'a Project,
    pub(super) worktrees: Vec<&'a WorktreeEntry>,
}

impl Selection<'_> {
    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.projects
            .iter()
            .flat_map(|project| &project.worktrees)
            .map(|worktree| worktree.path.clone())
            .collect()
    }
}

pub(super) fn resolve(model: &Model) -> Selection<'_> {
    let Some(row) = model.rows.get(model.cursor) else {
        return Selection {
            label: "selection".to_string(),
            projects: Vec::new(),
        };
    };

    let label = match row.depth {
        Depth::Host => row.host.clone(),
        Depth::Owner => format!("{}/{}", row.host, row.owner),
        _ if matches!(row.info, Info::Checkout(_)) => row
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| row.title.clone()),
        _ => row.project.clone(),
    };
    let projects = model
        .snapshot
        .projects
        .iter()
        .filter_map(|project| {
            let worktrees = if matches!(row.info, Info::Checkout(_)) {
                if project.id != row.project_id {
                    return None;
                }
                project
                    .worktrees
                    .iter()
                    .filter(|worktree| row.path.as_ref() == Some(&worktree.path))
                    .collect::<Vec<_>>()
            } else {
                let matches = match row.depth {
                    Depth::Host | Depth::Owner => {
                        let remote = project.remote.as_deref();
                        let host = remote.and_then(owner::host_from_remote).unwrap_or_default();
                        let owner = remote
                            .and_then(owner::owner_from_remote)
                            .map(|owner| owner.name)
                            .unwrap_or_default();
                        host == row.host && (row.depth == Depth::Host || owner == row.owner)
                    }
                    _ => project.id == row.project_id,
                };
                if !matches {
                    return None;
                }
                project.worktrees.iter().collect()
            };
            (!worktrees.is_empty()).then_some(ProjectSelection { project, worktrees })
        })
        .collect();
    Selection { label, projects }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hq::inventory::Snapshot;
    use crate::hq::{Grouping, View, rows};

    fn worktree(path: &str, main: bool) -> WorktreeEntry {
        WorktreeEntry {
            path: path.into(),
            branch: Some(if main { "main" } else { "feature" }.to_string()),
            head: "abc".to_string(),
            main,
            external: !main,
            locked: false,
            prunable: false,
            dirty: Some(false),
            added: 0,
            modified: 0,
            deleted: 0,
            untracked: 0,
            ahead: 0,
            behind: 0,
            upstream: None,
            upstream_gone: false,
            missing: false,
            last_activity: None,
        }
    }

    fn fixture() -> Model {
        let projects = [
            ("github.com/alice/tool", "git@github.com:alice/tool.git"),
            ("github.com/alice/other", "https://github.com/alice/other"),
            ("github.com/bob/tool", "git@github.com:bob/tool.git"),
            ("gitlab.com/alice/tool", "git@gitlab.com:alice/tool.git"),
            ("local/tool", ""),
        ]
        .into_iter()
        .map(|(id, remote)| Project {
            id: id.to_string(),
            name: id.rsplit('/').next().unwrap().to_string(),
            path: format!("/repos/{id}").into(),
            remote: (!remote.is_empty()).then(|| remote.to_string()),
            worktrees: vec![
                worktree(&format!("/repos/{id}"), true),
                worktree(&format!("/external/{id}"), false),
            ],
        })
        .collect();
        Model::from_snapshot(Snapshot {
            root: "/trees".into(),
            projects,
            warnings: Vec::new(),
        })
    }

    fn rebuild(model: &mut Model) {
        model.rows = rows::visible_rows(model, 0);
        model.cursor = 0;
    }

    fn select(model: &mut Model, predicate: impl Fn(&super::super::Row) -> bool) {
        model.cursor = model.rows.iter().position(predicate).unwrap();
    }

    fn ids(selection: &Selection<'_>) -> Vec<String> {
        selection
            .projects
            .iter()
            .map(|project| project.project.id.clone())
            .collect()
    }

    #[test]
    fn project_scope_is_the_same_in_every_grouping() {
        let mut model = fixture();
        for grouping in Grouping::ALL {
            model.grouping = grouping;
            rebuild(&mut model);
            select(&mut model, |row| {
                row.project_id == "github.com/alice/tool" && matches!(row.info, Info::Group(_))
            });
            let selection = resolve(&model);
            assert_eq!(ids(&selection), ["github.com/alice/tool"]);
            assert_eq!(
                selection.paths(),
                [
                    PathBuf::from("/repos/github.com/alice/tool"),
                    PathBuf::from("/external/github.com/alice/tool"),
                ]
            );
        }
    }

    #[test]
    fn checkout_scope_is_the_same_in_tree_detail_and_search() {
        let mut model = fixture();
        let target = PathBuf::from("/external/github.com/alice/tool");
        for grouping in Grouping::ALL {
            for (detail, filter) in [(false, ""), (true, ""), (false, "feature"), (true, "tool")] {
                if grouping == Grouping::Table && !detail && filter.is_empty() {
                    continue;
                }
                model.grouping = grouping;
                model.view = if detail {
                    View::Detail {
                        project: "github.com/alice/tool".to_string(),
                    }
                } else {
                    View::List
                };
                model.filter.set(filter);
                rebuild(&mut model);
                select(&mut model, |row| {
                    row.path.as_ref() == Some(&target) && matches!(row.info, Info::Checkout(_))
                });
                let selection = resolve(&model);
                assert_eq!(ids(&selection), ["github.com/alice/tool"]);
                assert_eq!(selection.paths().as_slice(), std::slice::from_ref(&target));
                assert_eq!(selection.label, target.display().to_string());
            }
        }
    }

    #[test]
    fn host_and_owner_scope_use_the_full_remote_identity() {
        let mut model = fixture();
        model.grouping = Grouping::Hosts;
        rebuild(&mut model);
        select(&mut model, |row| {
            row.depth == Depth::Host && row.host == "github.com"
        });
        let selection = resolve(&model);
        assert_eq!(
            ids(&selection),
            [
                "github.com/alice/tool",
                "github.com/alice/other",
                "github.com/bob/tool"
            ]
        );
        assert_eq!(selection.paths().len(), 6);
        assert_eq!(selection.label, "github.com");
        for grouping in [Grouping::Owners, Grouping::Hosts] {
            model.grouping = grouping;
            rebuild(&mut model);
            select(&mut model, |row| {
                row.depth == Depth::Owner && row.host == "github.com" && row.owner == "alice"
            });
            let selection = resolve(&model);
            assert_eq!(
                ids(&selection),
                ["github.com/alice/tool", "github.com/alice/other"]
            );
            assert_eq!(selection.paths().len(), 4);
            assert_eq!(selection.label, "github.com/alice");
        }
    }

    #[test]
    fn main_checkout_and_missing_checkout_remain_exact_targets() {
        let mut model = fixture();
        model.snapshot.projects[0].worktrees[1].missing = true;
        model.view = View::Detail {
            project: "github.com/alice/tool".to_string(),
        };
        rebuild(&mut model);
        for target in [
            "/repos/github.com/alice/tool",
            "/external/github.com/alice/tool",
        ] {
            select(&mut model, |row| {
                row.path.as_deref() == Some(std::path::Path::new(target))
            });
            assert_eq!(resolve(&model).paths(), [PathBuf::from(target)]);
        }
    }

    #[test]
    fn unknown_or_empty_selection_never_falls_back_to_a_project() {
        let mut model = fixture();
        model.view = View::Detail {
            project: "github.com/alice/tool".to_string(),
        };
        rebuild(&mut model);
        model.rows[0].path = Some("/unknown".into());
        assert!(resolve(&model).projects.is_empty());
        model.rows.clear();
        assert!(resolve(&model).projects.is_empty());
    }
}
