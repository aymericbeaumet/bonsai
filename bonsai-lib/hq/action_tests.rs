use super::rows::{Depth, Info};
use super::tests::{arguments, background, press, sample};
use super::*;

const DESTRUCTIVE: [Cmd; 5] = [Cmd::Remove, Cmd::Delete, Cmd::Clean, Cmd::Prune, Cmd::Trim];

#[test]
fn command_shortcuts_are_mnemonic_and_destructive_shortcuts_are_uppercase() {
    let mut keys = std::collections::HashSet::new();
    for spec in COMMANDS {
        assert!(keys.insert(spec.key), "duplicate shortcut: {}", spec.key);
        if spec.cmd == Cmd::Help {
            continue;
        }
        let letter = spec.key.strip_prefix("ctrl-").unwrap_or(spec.key);
        assert_eq!(letter.len(), 1, "{} needs one command initial", spec.name);
        assert!(
            letter.eq_ignore_ascii_case(&spec.name[..1]),
            "{} must start with {}",
            spec.key,
            spec.name
        );
        if DESTRUCTIVE.contains(&spec.cmd) {
            assert!(letter.chars().all(|key| key.is_ascii_uppercase()));
        }
        let event = KeyEvent::new(
            KeyCode::Char(letter.chars().next().unwrap()),
            if spec.key.starts_with("ctrl-") {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
        );
        assert_eq!(shortcut(&event), Some(spec.cmd), "{}", spec.name);
    }
}

#[test]
fn shifted_command_initials_are_normalized_without_modified_deletion_shortcuts() {
    for (letter, command) in [
        ('r', Cmd::Remove),
        ('d', Cmd::Delete),
        ('c', Cmd::Clean),
        ('p', Cmd::Prune),
        ('t', Cmd::Trim),
    ] {
        assert_eq!(
            shortcut(&KeyEvent::new(KeyCode::Char(letter), KeyModifiers::SHIFT)),
            Some(command)
        );
        assert_eq!(
            shortcut(&KeyEvent::new(
                KeyCode::Char(letter.to_ascii_uppercase()),
                KeyModifiers::ALT
            )),
            None
        );
        assert!(
            !shortcut(&KeyEvent::new(
                KeyCode::Char(letter.to_ascii_uppercase()),
                KeyModifiers::CONTROL
            ))
            .is_some_and(|command| DESTRUCTIVE.contains(&command))
        );
    }
}

#[test]
fn lowercase_old_destructive_shortcuts_cannot_offer_deletion() {
    for input in ["dd", "x", "t"] {
        let mut model = fixture();
        for key in input.chars() {
            assert!(matches!(
                model.on_key(press(KeyCode::Char(key))),
                Effect::None
            ));
        }
        assert!(model.confirm.is_none());
        assert!(model.ready_plan.is_none());
    }
}

#[test]
fn uppercase_command_initials_offer_the_correct_destructive_action() {
    for (key, command) in [
        ('R', Cmd::Remove),
        ('D', Cmd::Delete),
        ('C', Cmd::Clean),
        ('P', Cmd::Prune),
        ('T', Cmd::Trim),
    ] {
        let mut model = fixture();
        let expected = destructive(&mut fixture(), command);
        let effect = model.on_key(press(KeyCode::Char(key)));
        let actual = if matches!(command, Cmd::Remove | Cmd::Delete) {
            assert!(matches!(effect, Effect::None));
            background(model.on_key(press(KeyCode::Char('y'))))
        } else {
            background(effect)
        };
        assert_eq!(arguments(&actual), arguments(&expected), "{key}");
    }
}

fn linked(path: &str) -> WorktreeEntry {
    WorktreeEntry {
        path: path.into(),
        branch: Some("feature".to_string()),
        head: "abc".to_string(),
        main: false,
        external: true,
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
    let mut snapshot = sample();
    snapshot.warnings.clear();
    snapshot.projects[0]
        .worktrees
        .push(linked("/external/bonsai/second"));
    snapshot.projects[1]
        .worktrees
        .push(linked("/external/other/feature"));
    for (id, remote, path) in [
        (
            "same-owner",
            "git@github.com:aymericbeaumet/another.git",
            "/external/same-owner/feature",
        ),
        (
            "other-host",
            "git@gitlab.com:aymericbeaumet/bonsai.git",
            "/external/other-host/feature",
        ),
    ] {
        snapshot.projects.push(Project {
            id: id.to_string(),
            name: "bonsai".to_string(),
            path: format!("/repos/{id}").into(),
            remote: Some(remote.to_string()),
            worktrees: vec![linked(path)],
        });
    }
    Model::from_snapshot(snapshot)
}

fn select(model: &mut Model, predicate: impl Fn(&Row) -> bool) {
    model.rows = rows::visible_rows(model, 0);
    model.cursor = model.rows.iter().position(predicate).unwrap();
}

fn targets(spec: &jobs::Spec) -> Vec<String> {
    arguments(spec)
        .into_iter()
        .skip(2)
        .filter(|arg| arg.starts_with('/'))
        .collect()
}

fn destructive(model: &mut Model, cmd: Cmd) -> jobs::Spec {
    let effect = model.command(cmd, "");
    if matches!(cmd, Cmd::Remove | Cmd::Delete) {
        assert!(matches!(effect, Effect::None), "{cmd:?} must ask in hq");
        let confirm = model.confirm.as_ref().expect("a removal confirmation");
        let Action::Background { spec, .. } = &confirm.action else {
            panic!("{cmd:?} must stay in hq");
        };
        let body = confirm
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for target in targets(spec) {
            assert!(body.contains(&target), "confirmation is missing {target}");
        }
        let spec = background(model.on_key(press(KeyCode::Char('y'))));
        assert!(!arguments(&spec).iter().any(|arg| arg == "--force"));
        spec
    } else {
        let spec = background(effect);
        assert!(arguments(&spec).contains(&"--dry-run".to_string()));
        assert!(arguments(&spec).contains(&"--json".to_string()));
        assert!(!arguments(&spec).contains(&"--yes".to_string()));
        spec
    }
}

#[test]
fn every_destructive_action_uses_the_same_project_scope_in_every_grouping() {
    for grouping in Grouping::ALL {
        for cmd in DESTRUCTIVE {
            let mut model = fixture();
            model.grouping = grouping;
            select(&mut model, |row| {
                row.project_id == "bonsai" && matches!(row.info, Info::Group(_))
            });
            let spec = destructive(&mut model, cmd);
            assert_eq!(
                targets(&spec),
                ["/trees/bonsai/ab/dev", "/external/bonsai/second"],
                "{grouping:?} {cmd:?}"
            );
        }
    }
}

#[test]
fn every_destructive_action_uses_exact_checkout_scope_in_tree_detail_and_search() {
    for grouping in Grouping::ALL {
        for (detail, filter) in [(false, ""), (true, ""), (false, "second"), (true, "second")] {
            if grouping == Grouping::Table && !detail && filter.is_empty() {
                continue;
            }
            for cmd in DESTRUCTIVE {
                let mut model = fixture();
                model.grouping = grouping;
                if detail {
                    model.view = View::Detail {
                        project: "bonsai".to_string(),
                    };
                }
                model.filter.set(filter);
                select(&mut model, |row| {
                    matches!(row.info, Info::Checkout(_))
                        && row.path.as_deref() == Some(Path::new("/external/bonsai/second"))
                });
                assert_eq!(
                    targets(&destructive(&mut model, cmd)),
                    ["/external/bonsai/second"],
                    "{grouping:?} {detail} {filter} {cmd:?}"
                );
            }
        }
    }
}

#[test]
fn every_destructive_action_limits_host_and_owner_scope_to_matching_projects() {
    for (depth, expected) in [
        (
            Depth::Host,
            vec![
                "/trees/bonsai/ab/dev",
                "/external/bonsai/second",
                "/external/other/feature",
                "/external/same-owner/feature",
            ],
        ),
        (
            Depth::Owner,
            vec![
                "/trees/bonsai/ab/dev",
                "/external/bonsai/second",
                "/external/same-owner/feature",
            ],
        ),
    ] {
        for cmd in DESTRUCTIVE {
            let mut model = fixture();
            model.grouping = Grouping::Hosts;
            select(&mut model, |row| {
                row.depth == depth
                    && row.host == "github.com"
                    && (depth == Depth::Host || row.owner == "aymericbeaumet")
            });
            let spec = destructive(&mut model, cmd);
            assert_eq!(targets(&spec), expected, "{depth:?} {cmd:?}");
            assert!(arguments(&spec).contains(&"-a".to_string()));
        }
    }
}

#[test]
fn protected_main_checkout_never_expands_destructive_scope_to_its_project() {
    for cmd in DESTRUCTIVE {
        let mut model = fixture();
        model.view = View::Detail {
            project: "bonsai".to_string(),
        };
        select(&mut model, |row| {
            row.main && matches!(row.info, Info::Checkout(_))
        });
        assert!(matches!(model.command(cmd, ""), Effect::None), "{cmd:?}");
        assert!(model.confirm.is_none(), "{cmd:?}");
    }
}

#[test]
fn prune_reviews_deleted_registrations_and_orphans_then_runs_only_reviewed_paths() {
    let mut model = fixture();
    model.snapshot.projects[0].worktrees[0].missing = true;
    select(&mut model, |row| {
        row.project_id == "bonsai" && matches!(row.info, Info::Group(_))
    });
    let spec = background(model.command(Cmd::Prune, ""));
    let jobs::After::PrunePlan {
        project,
        targets: planned,
    } = spec.after
    else {
        panic!("prune must preview in the background")
    };
    assert_eq!(
        planned,
        [
            PathBuf::from("/trees/bonsai/ab/dev"),
            PathBuf::from("/external/bonsai/second")
        ]
    );
    model.open_prune_plan(
        &project,
        &planned,
        r#"{"stale":["/trees/bonsai/ab/dev"],"orphans":["/external/bonsai/orphan"]}"#,
        None,
    );
    let confirm = model.confirm.as_ref().expect("prune asks in hq");
    let text = confirm
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("/trees/bonsai/ab/dev"));
    assert!(text.contains("/external/bonsai/orphan"));
    assert!(text.contains("registrations"));
    assert!(text.contains("everything in them"));
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    assert_eq!(
        targets(&spec),
        ["/trees/bonsai/ab/dev", "/external/bonsai/orphan"]
    );
    assert!(arguments(&spec).contains(&"--yes".to_string()));
    assert!(!arguments(&spec).contains(&"--dry-run".to_string()));
    assert!(!arguments(&spec).contains(&"--force".to_string()));
    assert_eq!(spec.after, jobs::After::Refresh);
    assert!(model.output.is_none());
}

#[test]
fn clean_and_trim_confirmations_show_each_full_checkout_and_ignored_path() {
    let mut model = fixture();
    let project = Path::new("/trees/bonsai");
    let paths = [
        PathBuf::from("/trees/bonsai/ab/dev"),
        PathBuf::from("/external/bonsai/second"),
    ];
    let clean = serde_json::json!({ "planned": paths.iter().map(|path| serde_json::json!({"branch":"feature", "reason":"merged", "path":path})).collect::<Vec<_>>() }).to_string();
    model.open_clean_plan(project, &clean, None);
    let confirm = model.confirm.as_ref().expect("clean confirmation");
    let text = confirm
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for path in &paths {
        assert!(text.contains(&path.display().to_string()));
    }
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    let jobs::After::CleanVerify { expected, .. } = spec.after else {
        panic!("clean must recheck reviewed targets")
    };
    let Some(Action::Background { spec, .. }) = model.verified_clean(project, &expected, &clean)
    else {
        panic!("matching clean plan must run in the background")
    };
    let mut actual = targets(&spec);
    actual.sort();
    let mut wanted = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    wanted.sort();
    assert_eq!(actual, wanted);
    let trim = serde_json::json!({ "bytes":20, "planned": paths.iter().map(|path| serde_json::json!({"branch":"feature", "path":path, "bytes":10, "entries":[{"path":path.join("target"), "bytes":10}]})).collect::<Vec<_>>() }).to_string();
    model.open_trim_plan(project, &paths, &trim, None);
    let confirm = model.confirm.as_ref().expect("trim confirmation");
    let text = confirm
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for path in &paths {
        assert!(text.contains(&path.display().to_string()));
        assert!(text.contains(&path.join("target").display().to_string()));
    }
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    assert_eq!(
        targets(&spec),
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
    );
    assert!(arguments(&spec).contains(&"--yes".to_string()));
    assert_eq!(spec.reviewed.as_deref(), Some(trim.as_str()));
}

#[test]
fn grouped_plans_keep_global_scope_when_only_a_non_anchor_project_has_work() {
    let mut model = fixture();
    let anchor = Path::new("/trees/bonsai");
    let target = PathBuf::from("/external/other/feature");
    let clean =
        serde_json::json!([{"planned":[{"branch":"feature", "reason":"merged", "path":target}]}])
            .to_string();
    model.open_clean_plan(anchor, &clean, None);
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    assert!(arguments(&spec).contains(&"-a".to_string()));
    assert!(spec.projects.contains(&PathBuf::from("/trees/other")));
    let Some(Action::Background { spec, .. }) =
        model.verified_clean(anchor, std::slice::from_ref(&target), &clean)
    else {
        panic!("clean must execute the reviewed project");
    };
    assert!(arguments(&spec).contains(&"-a".to_string()));

    let trim = serde_json::json!({"planned":[{"branch":"feature", "path":target, "bytes":0, "entries":[{"path":"target", "bytes":0}]}]}).to_string();
    model.open_trim_plan(anchor, std::slice::from_ref(&target), &trim, None);
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    assert!(arguments(&spec).contains(&"-a".to_string()));
    assert_eq!(spec.reviewed.as_deref(), Some(trim.as_str()));

    let prune = serde_json::json!({"stale":[target], "orphans":[]}).to_string();
    model.open_prune_plan(anchor, std::slice::from_ref(&target), &prune, None);
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    assert!(arguments(&spec).contains(&"-a".to_string()));
    assert_eq!(spec.reviewed.as_deref(), Some(prune.as_str()));
}

#[test]
fn prune_on_a_directory_deleted_checkout_previews_only_its_registration() {
    let mut model = fixture();
    model.snapshot.projects[0].worktrees[0].missing = true;
    model.view = View::Detail {
        project: "bonsai".to_string(),
    };
    select(&mut model, |row| {
        matches!(row.info, Info::Checkout(_))
            && row.path.as_deref() == Some(Path::new("/trees/bonsai/ab/dev"))
    });
    let spec = background(model.on_key(press(KeyCode::Char('P'))));
    assert_eq!(targets(&spec), ["/trees/bonsai/ab/dev"]);
    let jobs::After::PrunePlan { project, targets } = spec.after else {
        panic!("a deleted checkout must still offer a prune preview")
    };
    model.open_prune_plan(
        &project,
        &targets,
        r#"{"stale":["/trees/bonsai/ab/dev"],"orphans":[]}"#,
        None,
    );
    let spec = background(model.on_key(press(KeyCode::Char('y'))));
    assert_eq!(self::targets(&spec), ["/trees/bonsai/ab/dev"]);
    assert!(arguments(&spec).contains(&"--yes".to_string()));
}

#[test]
fn a_pending_clean_plan_is_not_reused_after_selecting_a_narrower_scope() {
    let mut model = fixture();
    let original = [
        PathBuf::from("/trees/bonsai/ab/dev"),
        PathBuf::from("/external/bonsai/second"),
    ];
    let report = serde_json::json!({
        "planned": original.iter().map(|path| serde_json::json!({
            "branch": "feature", "reason": "merged", "path": path,
        })).collect::<Vec<_>>(),
    })
    .to_string();
    model.filtering = true;
    model.offer_clean_plan(Path::new("/trees/bonsai"), &original, &report);
    assert!(model.ready_plan.is_some());
    model.filtering = false;
    model.view = View::Detail {
        project: "bonsai".to_string(),
    };
    select(&mut model, |row| {
        matches!(row.info, Info::Checkout(_))
            && row.path.as_deref() == Some(Path::new("/external/bonsai/second"))
    });
    let spec = background(model.command(Cmd::Clean, ""));
    assert_eq!(targets(&spec), ["/external/bonsai/second"]);
    assert!(model.confirm.is_none());
    assert!(model.ready_plan.is_none());
}
