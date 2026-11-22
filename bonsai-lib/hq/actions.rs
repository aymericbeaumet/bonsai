use super::*;

impl Model {
    pub(super) fn fetch_scope(&mut self) -> Effect {
        let mut actions: Vec<_> = scope::resolve(self)
            .projects
            .into_iter()
            .map(|selected| {
                let project = selected.project;
                Action::Background {
                    spec: Box::new(jobs::Spec::git(
                        &project.path,
                        &["fetch", "--prune"],
                        jobs::After::Refresh,
                    )),
                    project: Some(project.id.clone()),
                }
            })
            .collect();
        match actions.len() {
            0 => self.say("select a scope first"),
            1 => Effect::Run(actions.pop().expect("one action")),
            _ => Effect::Run(Action::Batch(actions)),
        }
    }
    pub(super) fn confirm_remove_scope(&mut self, delete_branch: bool) -> Effect {
        let selection = scope::resolve(self);
        let label = selection.label;
        let targets: Vec<_> = selection
            .projects
            .iter()
            .flat_map(|selection| &selection.worktrees)
            .filter(|worktree| !worktree.main && (!delete_branch || worktree.branch.is_some()))
            .collect();
        if targets.is_empty() {
            return self.say("no removable worktrees in this scope");
        }
        let Some((anchor, project)) = self.scope_anchor() else {
            return self.say("select a scope first");
        };
        let mut lines = vec![Line::raw(if delete_branch {
            "Delete these worktrees and their merged branches:"
        } else {
            "Delete these worktrees; keep their branches:"
        })];
        let mut paths = Vec::new();
        for worktree in targets {
            paths.push(worktree.path.clone());
            lines.push(Line::styled(
                format!("  {}", rows::branch_label(worktree)),
                Style::new().add_modifier(ratatui::style::Modifier::BOLD),
            ));
            lines.push(Line::raw(format!("    {}", worktree.path.display())));
            if worktree.missing {
                lines.push(Line::raw("    directory deleted: remove its registration"));
            }
            if worktree.dirty == Some(true) {
                lines.push(Line::styled(
                    "    uncommitted changes: removal will be refused",
                    Style::new().fg(Color::Red),
                ));
            }
            if worktree.locked {
                lines.push(Line::styled(
                    "    locked: removal will be refused",
                    Style::new().fg(Color::Red),
                ));
            }
            let busy = self.busy(&worktree.path);
            if !busy.is_empty() {
                lines.push(Line::styled(
                    format!("    {}: removal will be refused", agents::describe(&busy)),
                    Style::new().fg(Color::Red),
                ));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(
            "Main checkout stays. Dirty, locked, or busy worktrees are never forced.",
        ));
        let args: &[&str] = if delete_branch {
            &["remove", "-d"]
        } else {
            &["remove"]
        };
        let spec = match self.paths_spec(&anchor, args, &paths, jobs::After::Refresh) {
            Ok(spec) => spec,
            Err(error) => return self.say(&format!("{error:#}")),
        };
        self.confirm = Some(Confirm {
            title: format!(
                "{} in {label}?",
                if delete_branch {
                    "Delete worktrees and branches"
                } else {
                    "Remove worktrees"
                }
            ),
            lines,
            yes: if delete_branch { "delete" } else { "remove" },
            tone: Color::Yellow,
            action: Action::Background {
                spec: Box::new(spec),
                project,
            },
            ready_at: None,
            pager: keys::Pager::default(),
        });
        Effect::None
    }
    pub(super) fn linked_targets(&self, whole: bool) -> Vec<PathBuf> {
        if !whole {
            return scope::resolve(self)
                .paths()
                .into_iter()
                .filter(|path| {
                    self.snapshot
                        .projects
                        .iter()
                        .flat_map(|project| &project.worktrees)
                        .any(|worktree| &worktree.path == path && !worktree.main)
                })
                .collect();
        }
        scope::resolve(self)
            .projects
            .iter()
            .flat_map(|selection| {
                let worktrees: Vec<_> = if whole {
                    selection.project.worktrees.iter().collect()
                } else {
                    selection.worktrees.clone()
                };
                worktrees
                    .into_iter()
                    .filter(|worktree| !worktree.main)
                    .map(|worktree| worktree.path.clone())
            })
            .collect()
    }

    pub(super) fn scope_anchor(&self) -> Option<(PathBuf, Option<String>)> {
        let selection = scope::resolve(self);
        let first = selection.projects.first()?;
        let anchor = if first
            .project
            .worktrees
            .iter()
            .any(|worktree| worktree.main && !worktree.missing)
        {
            first.project.path.clone()
        } else {
            first
                .project
                .worktrees
                .iter()
                .find(|worktree| !worktree.missing)
                .map_or_else(
                    || self.snapshot.root.clone(),
                    |worktree| worktree.path.clone(),
                )
        };
        Some((
            anchor,
            (selection.projects.len() == 1).then(|| first.project.id.clone()),
        ))
    }

    pub(super) fn paths_spec(
        &self,
        anchor: &Path,
        args: &[&str],
        paths: &[PathBuf],
        after: jobs::After,
    ) -> Result<jobs::Spec> {
        let anchor_project = self.snapshot.projects.iter().find(|project| {
            project.path == anchor
                || project
                    .worktrees
                    .iter()
                    .any(|worktree| worktree.path == anchor)
        });
        let all = self
            .snapshot
            .projects
            .iter()
            .filter(|project| {
                project
                    .worktrees
                    .iter()
                    .any(|worktree| paths.contains(&worktree.path))
            })
            .any(|project| anchor_project.is_none_or(|anchor| project.id != anchor.id));
        let mut arguments: Vec<OsString> = args.iter().map(OsString::from).collect();
        if all && !args.contains(&"-a") {
            arguments.push("-a".into());
        }
        arguments.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        let mut spec = jobs::Spec::bonsai(anchor, &self.flags, &[], after)?;
        spec.projects = self
            .snapshot
            .projects
            .iter()
            .map(|project| project.path.clone())
            .collect();
        spec.args.extend(arguments);
        spec.label = format!("bonsai {}", args.join(" "));
        Ok(spec)
    }

    pub(super) fn scoped_background(
        &mut self,
        args: &[&str],
        targets: &[PathBuf],
        after: impl FnOnce(&Path) -> jobs::After,
    ) -> Effect {
        let Some((anchor, project)) = self.scope_anchor() else {
            return self.say("select a scope first");
        };
        if targets.is_empty() {
            return self.say("no linked worktrees in this scope");
        }
        match self.paths_spec(&anchor, args, targets, after(&anchor)) {
            Ok(spec) => Effect::Run(Action::Background {
                spec: Box::new(spec),
                project,
            }),
            Err(error) => self.say(&format!("{error:#}")),
        }
    }

    pub(super) fn prune(&mut self) -> Effect {
        let targets = self.linked_targets(false);
        let kind = PlanKind::Prune {
            targets: targets.clone(),
        };
        if let Some((project, report)) = self.take_ready(&kind) {
            self.open_prune_plan(&project, &targets, &report, None);
            return Effect::None;
        }
        self.scoped_background(&["prune", "--dry-run", "--json"], &targets, |project| {
            jobs::After::PrunePlan {
                project: project.to_path_buf(),
                targets: targets.clone(),
            }
        })
    }

    pub(super) fn open_prune_plan(
        &mut self,
        project: &Path,
        targets: &[PathBuf],
        report: &str,
        ready_at: Option<Instant>,
    ) {
        let plan: PrunePlan = match serde_json::from_str(report) {
            Ok(plan) => plan,
            Err(error) => {
                self.notice = Some(format!("cannot read the prune plan: {error}"));
                return;
            }
        };
        let paths: Vec<_> = plan.stale.iter().chain(&plan.orphans).cloned().collect();
        if paths.is_empty() {
            self.notice = Some("nothing to prune in this scope".into());
            return;
        }
        let mut lines = vec![Line::raw(
            "Remove these registrations; their directories are already deleted:",
        )];
        lines.extend(
            plan.stale
                .iter()
                .map(|path| Line::raw(format!("  {}", path.display()))),
        );
        if !plan.orphans.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Delete these orphan directories and everything in them:",
                Style::new().fg(Color::Yellow),
            ));
            lines.extend(
                plan.orphans
                    .iter()
                    .map(|path| Line::raw(format!("  {}", path.display()))),
            );
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(
            "Branches and existing registered checkouts are kept.",
        ));
        let mut spec =
            match self.paths_spec(project, &["prune", "--yes"], &paths, jobs::After::Refresh) {
                Ok(spec) => spec,
                Err(error) => {
                    self.notice = Some(format!("{error:#}"));
                    return;
                }
            };
        spec.reviewed = Some(report.to_string());
        self.confirm = Some(Confirm {
            title: format!("Prune {}?", view::plural(paths.len(), "worktree")),
            lines,
            yes: "prune",
            tone: Color::Yellow,
            action: Action::Background {
                spec: Box::new(spec),
                project: self.project_for_paths(targets),
            },
            ready_at,
            pager: keys::Pager::default(),
        });
    }

    pub(super) fn project_for_paths(&self, paths: &[PathBuf]) -> Option<String> {
        let mut found = self.snapshot.projects.iter().filter(|project| {
            project
                .worktrees
                .iter()
                .any(|worktree| paths.contains(&worktree.path))
        });
        let first = found.next()?;
        found.next().is_none().then(|| first.id.clone())
    }
}

#[derive(serde::Deserialize)]
struct PrunePlan {
    #[serde(default)]
    stale: Vec<PathBuf>,
    #[serde(default)]
    orphans: Vec<PathBuf>,
}
