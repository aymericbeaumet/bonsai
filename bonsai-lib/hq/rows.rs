//! Rows of the headquarters list and the facts their columns show.
//!
//! The home list has one row per project, summing its checkouts. Inside a
//! project, and in search results, each checkout is a row. The tree nests
//! host, owner, repository, then checkouts, and every group sums what it
//! contains, so a size is always available at each level.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use super::footprint::{Dependency, Footprint};
use super::inventory::{Project, WorktreeEntry};
use super::owner;
use super::{Grouping, Model, Sort, View};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Depth {
    Flat,
    Host,
    Owner,
    Repo,
    Branch,
}

pub(super) struct Row {
    pub(super) host: String,
    pub(super) owner: String,
    pub(super) project: String,
    /// Branch label on checkout rows.
    pub(super) title: String,
    /// Tree elbow, `├ ` or `└ `. Empty on a banner and on table rows.
    pub(super) guide: &'static str,
    /// Ancestor rails drawn before the elbow.
    pub(super) prefix: String,
    pub(super) depth: Depth,
    pub(super) age: Option<Duration>,
    pub(super) main: bool,
    /// The selectable checkout. Project rows point at their first checkout.
    pub(super) path: Option<PathBuf>,
    /// Owning project. Empty on host and owner rows.
    pub(super) project_id: String,
    /// The label spells out its ancestors: `host/owner` on an owner row,
    /// `host/owner/project` on a repository row.
    pub(super) joined: bool,
    pub(super) info: Info,
}

pub(super) enum Info {
    /// A project, repository, owner, or host: totals over its checkouts.
    Group(Totals),
    Checkout(Checkout),
}

#[derive(Default, Clone, Debug, PartialEq)]
pub(super) struct Totals {
    pub(super) checkouts: usize,
    /// Checkouts with uncommitted changes.
    pub(super) changed: usize,
    /// Checkouts whose directory was deleted.
    pub(super) missing: usize,
    pub(super) bytes: u64,
    /// Checkouts whose size is known, possibly from an earlier session.
    pub(super) sized: usize,
    /// Some size is still being measured, so `bytes` may grow.
    pub(super) measuring: bool,
    pub(super) stack: Vec<String>,
    pub(super) agents: Vec<String>,
    pub(super) unavailable: bool,
}

impl Totals {
    fn add(&mut self, checkout: &Checkout) {
        self.checkouts += 1;
        match checkout.status {
            Status::Changed { .. } => self.changed += 1,
            Status::Missing => self.missing += 1,
            _ => {}
        }
        if let Some(bytes) = checkout.bytes {
            self.bytes = self.bytes.saturating_add(bytes);
            self.sized += 1;
        }
        self.measuring |= checkout.measuring;
        merge_names(&mut self.stack, &checkout.stack);
        merge_names(&mut self.agents, &checkout.agents);
    }

    fn merge(&mut self, other: &Totals) {
        self.checkouts += other.checkouts;
        self.changed += other.changed;
        self.missing += other.missing;
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.sized += other.sized;
        self.measuring |= other.measuring;
        self.unavailable |= other.unavailable;
        merge_names(&mut self.stack, &other.stack);
        merge_names(&mut self.agents, &other.agents);
    }
}

fn merge_names(into: &mut Vec<String>, from: &[String]) {
    let mut names: BTreeSet<String> = into.drain(..).collect();
    names.extend(from.iter().cloned());
    into.extend(names);
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Checkout {
    pub(super) status: Status,
    pub(super) sync: Sync,
    pub(super) upstream: Option<String>,
    pub(super) bytes: Option<u64>,
    pub(super) measuring: bool,
    pub(super) dependencies: Vec<Dependency>,
    pub(super) stack: Vec<String>,
    pub(super) agents: Vec<String>,
    pub(super) notes: Vec<Note>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    /// Git status has not answered yet.
    Checking,
    Clean,
    Changed {
        added: usize,
        modified: usize,
        deleted: usize,
        untracked: usize,
    },
    /// Git status failed; the warning explains why.
    Unavailable,
    /// The directory was deleted while Git still lists the worktree.
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sync {
    Unknown,
    Detached,
    NoUpstream,
    Gone,
    Tracking { ahead: usize, behind: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Note {
    Main,
    External,
    Locked,
    Prunable,
}

pub(super) fn checkout(model: &Model, worktree: &WorktreeEntry) -> Checkout {
    let pending = model.pending.contains(&worktree.path);
    let status = if worktree.missing {
        Status::Missing
    } else if pending {
        Status::Checking
    } else {
        match worktree.dirty {
            None => Status::Unavailable,
            Some(false) => Status::Clean,
            Some(true) => Status::Changed {
                added: worktree.added,
                modified: worktree.modified,
                deleted: worktree.deleted,
                untracked: worktree.untracked,
            },
        }
    };
    let sync = if worktree.missing || pending || worktree.dirty.is_none() {
        Sync::Unknown
    } else if worktree.branch.is_none() {
        Sync::Detached
    } else if worktree.upstream_gone {
        Sync::Gone
    } else if worktree.upstream.is_none() {
        Sync::NoUpstream
    } else {
        Sync::Tracking {
            ahead: worktree.ahead,
            behind: worktree.behind,
        }
    };
    // A deleted checkout's remembered size no longer takes any space.
    let footprint = (!worktree.missing)
        .then(|| model.footprints.get(&worktree.path))
        .flatten();
    let mut notes = Vec::new();
    if worktree.main {
        notes.push(Note::Main);
    }
    if worktree.external {
        notes.push(Note::External);
    }
    if worktree.locked {
        notes.push(Note::Locked);
    }
    if worktree.prunable && !worktree.missing {
        notes.push(Note::Prunable);
    }
    Checkout {
        status,
        sync,
        upstream: worktree.upstream.clone(),
        bytes: footprint.map(|item| item.bytes),
        measuring: !worktree.missing && model.is_measuring(&worktree.path),
        dependencies: footprint
            .map(|item| item.dependencies.clone())
            .unwrap_or_default(),
        stack: footprint
            .map(|item| item.ecosystems.clone())
            .unwrap_or_default(),
        agents: footprint
            .map(|item| item.harnesses.clone())
            .unwrap_or_default(),
        notes,
    }
}

pub(super) fn project_totals(model: &Model, project: &Project) -> Totals {
    let mut totals = Totals {
        unavailable: project.id.starts_with("unavailable:"),
        ..Totals::default()
    };
    for worktree in &project.worktrees {
        totals.add(&checkout(model, worktree));
    }
    totals
}

pub(super) fn visible_rows(model: &Model, now: u64) -> Vec<Row> {
    let query = owner::parse_query(model.filter.text());
    let list = matches!(model.view, View::List) && query_is_empty(&query);
    let overview = list && !model.grouping.is_tree();
    let tree = list && model.grouping.is_tree();
    let mut projects: Vec<&Project> = model.snapshot.projects.iter().collect();
    sort_projects(model, &mut projects);
    if let View::Detail { project } = &model.view {
        projects.retain(|item| &item.id == project);
    }
    let mut shown = Vec::new();
    for project in projects {
        let id = project.remote.as_deref().and_then(owner::owner_from_remote);
        let host = project
            .remote
            .as_deref()
            .and_then(owner::host_from_remote)
            .unwrap_or_default();
        let owner_name = id
            .as_ref()
            .map(|owner| owner.name.clone())
            .unwrap_or_default();
        let mut worktrees: Vec<&WorktreeEntry> = project.worktrees.iter().collect();
        sort_worktrees(model, &mut worktrees);
        let matched: Vec<&WorktreeEntry> = worktrees
            .into_iter()
            .filter(|worktree| {
                matches_query(
                    project,
                    worktree,
                    id.as_ref(),
                    owner_kind(&model.owners, id.as_ref()),
                    model.footprints.get(&worktree.path),
                    &query,
                )
            })
            .collect();
        if matched.is_empty() {
            continue;
        }
        shown.push(Shown {
            host,
            owner: owner_name,
            project,
            matched,
            totals: project_totals(model, project),
        });
    }
    let context = Context { model, now };
    if tree {
        match model.grouping {
            Grouping::Projects => project_rows(&shown, &context),
            Grouping::Owners => owner_rows(&shown, &context),
            Grouping::Hosts | Grouping::Table => tree_rows(&shown, &context),
        }
    } else {
        list_rows(&shown, overview, &context)
    }
}

struct Context<'a> {
    model: &'a Model,
    now: u64,
}

struct Shown<'a> {
    host: String,
    owner: String,
    project: &'a Project,
    matched: Vec<&'a WorktreeEntry>,
    totals: Totals,
}

fn list_rows(shown: &[Shown<'_>], overview: bool, context: &Context<'_>) -> Vec<Row> {
    let detail = matches!(context.model.view, View::Detail { .. });
    let mut rows = Vec::new();
    for item in shown {
        if overview {
            let mut row = empty_row();
            row.host.clone_from(&item.host);
            row.owner.clone_from(&item.owner);
            row.project.clone_from(&item.project.name);
            row.project_id.clone_from(&item.project.id);
            row.age = age_elapsed(
                item.project
                    .worktrees
                    .iter()
                    .filter_map(|worktree| worktree.last_activity)
                    .max(),
                context.now,
            );
            row.path = item.matched.first().map(|worktree| worktree.path.clone());
            row.info = Info::Group(item.totals.clone());
            rows.push(row);
            continue;
        }
        for worktree in &item.matched {
            let mut row = checkout_row(worktree, context);
            row.project_id.clone_from(&item.project.id);
            if !detail {
                row.host.clone_from(&item.host);
                row.owner.clone_from(&item.owner);
                row.project.clone_from(&item.project.name);
            }
            rows.push(row);
        }
    }
    rows
}

fn checkout_row(worktree: &WorktreeEntry, context: &Context<'_>) -> Row {
    let mut row = empty_row();
    row.title = branch_label(worktree);
    row.age = age_elapsed(worktree.last_activity, context.now);
    row.main = worktree.main;
    row.path = Some(worktree.path.clone());
    row.info = Info::Checkout(checkout(context.model, worktree));
    row
}

/// Inset and continuing bar under a host or repository banner.
pub(super) const TREE_RAIL: &str = " │  ";

fn tree_elbow(continued: bool) -> &'static str {
    if continued { "├ " } else { "└ " }
}

fn tree_rail(continued: bool) -> &'static str {
    if continued { "│  " } else { "   " }
}

pub(super) fn child_prefix(prefix: &str, continued: bool) -> String {
    format!("{prefix}{}", tree_rail(continued))
}

enum Child<'a> {
    Owner {
        name: String,
        repos: Vec<&'a Shown<'a>>,
    },
    Repo(&'a Shown<'a>),
}

fn tree_rows(shown: &[Shown<'_>], context: &Context<'_>) -> Vec<Row> {
    let mut groups: Vec<(&str, Vec<&Shown<'_>>)> = Vec::new();
    for item in shown {
        if let Some(index) = groups.iter().position(|group| group.0 == item.host) {
            groups[index].1.push(item);
        } else {
            groups.push((item.host.as_str(), vec![item]));
        }
    }
    let mut rows = Vec::new();
    for (host, items) in groups {
        if host.is_empty() {
            for item in items {
                push_repo_banner(&mut rows, item, context);
            }
            continue;
        }
        push_host(&mut rows, host, &items);
        let children = host_children(&items);
        let last = children.len().saturating_sub(1);
        for (index, child) in children.into_iter().enumerate() {
            let continued = index != last;
            match child {
                Child::Owner { name, repos } => {
                    push_owner(&mut rows, host, &name, continued, &repos);
                    let prefix = child_prefix(TREE_RAIL, continued);
                    let repo_last = repos.len().saturating_sub(1);
                    for (repo_index, repo) in repos.iter().enumerate() {
                        push_repo(&mut rows, repo, &prefix, repo_index != repo_last, context);
                    }
                }
                Child::Repo(item) => push_repo(&mut rows, item, TREE_RAIL, continued, context),
            }
        }
    }
    rows
}

/// `host/owner` banners, each listing its projects.
fn owner_rows(shown: &[Shown<'_>], context: &Context<'_>) -> Vec<Row> {
    let mut groups: Vec<((&str, &str), Vec<&Shown<'_>>)> = Vec::new();
    for item in shown {
        let key = (item.host.as_str(), item.owner.as_str());
        if let Some(index) = groups.iter().position(|group| group.0 == key) {
            groups[index].1.push(item);
        } else {
            groups.push((key, vec![item]));
        }
    }
    let mut rows = Vec::new();
    for ((host, owner), items) in groups {
        if host.is_empty() && owner.is_empty() {
            for item in items {
                push_repo_banner(&mut rows, item, context);
            }
            continue;
        }
        let mut row = empty_row();
        row.depth = Depth::Owner;
        row.host = host.to_string();
        row.owner = owner.to_string();
        row.joined = true;
        row.info = Info::Group(sum(&items));
        rows.push(row);
        let last = items.len().saturating_sub(1);
        for (index, item) in items.iter().enumerate() {
            push_repo(&mut rows, item, TREE_RAIL, index != last, context);
        }
    }
    rows
}

/// One `host/owner/project` banner per project, listing its checkouts.
fn project_rows(shown: &[Shown<'_>], context: &Context<'_>) -> Vec<Row> {
    let mut rows = Vec::new();
    for item in shown {
        let mut row = repo_row(item, context);
        row.host.clone_from(&item.host);
        row.owner.clone_from(&item.owner);
        row.joined = true;
        rows.push(row);
        push_branches(&mut rows, item, TREE_RAIL, context);
    }
    rows
}

fn host_children<'a>(items: &[&'a Shown<'a>]) -> Vec<Child<'a>> {
    let mut children = Vec::new();
    for item in items {
        if item.owner.is_empty() {
            children.push(Child::Repo(item));
            continue;
        }
        if let Some(repos) = children.iter_mut().find_map(|child| match child {
            Child::Owner { name, repos } if name == &item.owner => Some(repos),
            _ => None,
        }) {
            repos.push(item);
            continue;
        }
        children.push(Child::Owner {
            name: item.owner.clone(),
            repos: vec![item],
        });
    }
    children
}

fn sum(items: &[&Shown<'_>]) -> Totals {
    let mut totals = Totals::default();
    for item in items {
        totals.merge(&item.totals);
    }
    totals
}

fn group_age(items: &[&Shown<'_>], now: u64) -> Option<Duration> {
    age_elapsed(
        items
            .iter()
            .flat_map(|item| &item.project.worktrees)
            .filter_map(|worktree| worktree.last_activity)
            .max(),
        now,
    )
}

fn push_host(rows: &mut Vec<Row>, host: &str, items: &[&Shown<'_>]) {
    let mut row = empty_row();
    row.depth = Depth::Host;
    row.host = host.to_string();
    row.info = Info::Group(sum(items));
    rows.push(row);
}

fn push_owner(rows: &mut Vec<Row>, host: &str, owner: &str, continued: bool, repos: &[&Shown<'_>]) {
    let mut row = empty_row();
    row.depth = Depth::Owner;
    row.host = host.to_string();
    row.owner = owner.to_string();
    row.prefix = TREE_RAIL.to_string();
    row.guide = tree_elbow(continued);
    row.info = Info::Group(sum(repos));
    rows.push(row);
}

fn repo_row(item: &Shown<'_>, context: &Context<'_>) -> Row {
    let mut row = empty_row();
    row.depth = Depth::Repo;
    row.project.clone_from(&item.project.name);
    row.project_id.clone_from(&item.project.id);
    row.age = group_age(&[item], context.now);
    row.info = Info::Group(item.totals.clone());
    row
}

fn push_repo_banner(rows: &mut Vec<Row>, item: &Shown<'_>, context: &Context<'_>) {
    rows.push(repo_row(item, context));
    push_branches(rows, item, TREE_RAIL, context);
}

fn push_repo(
    rows: &mut Vec<Row>,
    item: &Shown<'_>,
    prefix: &str,
    continued: bool,
    context: &Context<'_>,
) {
    let mut row = repo_row(item, context);
    row.host.clone_from(&item.host);
    row.owner.clone_from(&item.owner);
    row.prefix = prefix.to_string();
    row.guide = tree_elbow(continued);
    rows.push(row);
    push_branches(rows, item, &child_prefix(prefix, continued), context);
}

fn push_branches(rows: &mut Vec<Row>, item: &Shown<'_>, prefix: &str, context: &Context<'_>) {
    let last = item.matched.len().saturating_sub(1);
    for (index, worktree) in item.matched.iter().enumerate() {
        let mut row = checkout_row(worktree, context);
        row.depth = Depth::Branch;
        row.guide = tree_elbow(index != last);
        row.prefix = prefix.to_string();
        row.project_id.clone_from(&item.project.id);
        rows.push(row);
    }
}

pub(super) fn empty_row() -> Row {
    Row {
        host: String::new(),
        owner: String::new(),
        project: String::new(),
        title: String::new(),
        guide: "",
        prefix: String::new(),
        depth: Depth::Flat,
        age: None,
        main: false,
        path: None,
        project_id: String::new(),
        joined: false,
        info: Info::Group(Totals::default()),
    }
}

fn query_is_empty(query: &owner::Query) -> bool {
    query.free.is_empty() && query.user.is_empty() && query.organisation.is_empty()
}

fn owner_kind(
    known: &BTreeMap<String, owner::OwnerKind>,
    owner: Option<&owner::OwnerId>,
) -> Option<owner::OwnerKind> {
    let owner = owner?;
    if owner.host != "github.com" {
        return None;
    }
    known.get(&owner::cache_key(owner)).copied()
}

fn activity(worktree: &WorktreeEntry) -> u64 {
    worktree.last_activity.unwrap_or(0)
}

fn project_activity(project: &Project) -> u64 {
    project.worktrees.iter().map(activity).max().unwrap_or(0)
}

fn host_key(project: &Project) -> String {
    project
        .remote
        .as_deref()
        .and_then(owner::host_from_remote)
        .unwrap_or_default()
}

fn owner_key(project: &Project) -> String {
    project
        .remote
        .as_deref()
        .and_then(owner::owner_from_remote)
        .map(|owner| owner.name.to_lowercase())
        .unwrap_or_default()
}

fn by_name(left: &Project, right: &Project) -> Ordering {
    host_key(left)
        .cmp(&host_key(right))
        .then_with(|| owner_key(left).cmp(&owner_key(right)))
        .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        .then_with(|| left.id.cmp(&right.id))
}

fn sort_projects(model: &Model, projects: &mut [&Project]) {
    match model.sort {
        Sort::Recent => projects.sort_by(|left, right| {
            project_activity(right)
                .cmp(&project_activity(left))
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        }),
        Sort::Name => projects.sort_by(|left, right| by_name(left, right)),
        Sort::Size => {
            let size = |project: &Project| -> u64 {
                project
                    .worktrees
                    .iter()
                    .filter_map(|worktree| model.footprints.get(&worktree.path))
                    .map(|item| item.bytes)
                    .sum()
            };
            projects.sort_by(|left, right| {
                size(right)
                    .cmp(&size(left))
                    .then_with(|| by_name(left, right))
            });
        }
    }
}

fn sort_worktrees(model: &Model, worktrees: &mut [&WorktreeEntry]) {
    sort_by_order(model, worktrees);
    // Checkouts deleted by hand go last so the ones that exist stay in view.
    worktrees.sort_by_key(|worktree| worktree.missing);
}

fn sort_by_order(model: &Model, worktrees: &mut [&WorktreeEntry]) {
    match model.sort {
        Sort::Recent => worktrees.sort_by(|left, right| {
            activity(right)
                .cmp(&activity(left))
                .then_with(|| branch_order(&left.branch, &right.branch))
                .then_with(|| left.path.cmp(&right.path))
        }),
        Sort::Name => worktrees.sort_by(|left, right| {
            branch_order(&left.branch, &right.branch).then_with(|| left.path.cmp(&right.path))
        }),
        Sort::Size => {
            let size = |worktree: &WorktreeEntry| {
                model
                    .footprints
                    .get(&worktree.path)
                    .map_or(0, |item| item.bytes)
            };
            worktrees.sort_by(|left, right| {
                size(right)
                    .cmp(&size(left))
                    .then_with(|| branch_order(&left.branch, &right.branch))
                    .then_with(|| left.path.cmp(&right.path))
            });
        }
    }
}

fn age_elapsed(activity: Option<u64>, now: u64) -> Option<Duration> {
    activity.map(|activity| Duration::from_secs(now.saturating_sub(activity)))
}

fn branch_order(left: &Option<String>, right: &Option<String>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.to_lowercase().cmp(&right.to_lowercase()),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn matches_query(
    project: &Project,
    worktree: &WorktreeEntry,
    id: Option<&owner::OwnerId>,
    kind: Option<owner::OwnerKind>,
    footprint: Option<&Footprint>,
    query: &owner::Query,
) -> bool {
    let host = project
        .remote
        .as_deref()
        .and_then(owner::host_from_remote)
        .unwrap_or_default();
    let mut haystack = format!(
        "{} {} {} {} {}",
        host,
        id.map(|owner| owner.name.to_lowercase())
            .unwrap_or_default(),
        project.name.to_lowercase(),
        worktree.branch.as_deref().unwrap_or("").to_lowercase(),
        worktree.path.to_string_lossy().to_lowercase(),
    );
    if let Some(item) = footprint {
        haystack.push(' ');
        haystack.push_str(&item.summary().to_lowercase());
    }
    owner::matches(query, id, kind, &haystack)
}

pub(super) fn branch_label(worktree: &WorktreeEntry) -> String {
    if let Some(branch) = &worktree.branch {
        return branch.clone();
    }
    match worktree.head.chars().take(7).collect::<String>() {
        short if short.is_empty() => "(detached)".to_string(),
        short => format!("(detached {short})"),
    }
}

/// Paths of every checkout whose size is worth measuring, the selected
/// project's first.
pub(super) fn measure_order(model: &Model, first: Option<&str>) -> Vec<PathBuf> {
    let mut projects: Vec<&Project> = model.snapshot.projects.iter().collect();
    projects.sort_by_key(|project| Some(project.id.as_str()) != first);
    let mut seen = HashSet::new();
    projects
        .into_iter()
        .flat_map(|project| &project.worktrees)
        .filter(|worktree| !worktree.missing && seen.insert(worktree.path.clone()))
        .map(|worktree| worktree.path.clone())
        .collect()
}
