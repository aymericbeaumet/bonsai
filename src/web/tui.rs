use std::collections::BTreeSet;
use std::io::{Write, stdout};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde::Deserialize;

use super::{
    AppState, CreateTerminal, open_browser, snapshot_value, spawn_terminal, terminal,
    validate_bonsai_args,
};

const INK: Color = Color::Rgb(219, 230, 228);
const MUTED: Color = Color::Rgb(116, 140, 141);
const GREEN: Color = Color::Rgb(137, 219, 178);
const AMBER: Color = Color::Rgb(235, 193, 121);
const RED: Color = Color::Rgb(239, 137, 137);
const SURFACE: Color = Color::Rgb(15, 23, 27);
const SELECTED: Color = Color::Rgb(30, 57, 52);

#[derive(Default, Deserialize)]
struct Workspace {
    root: PathBuf,
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    terminals: Vec<Session>,
    #[serde(default)]
    tmux: Tmux,
    #[serde(default)]
    agents: Vec<Agent>,
    #[serde(default)]
    attention: Vec<Attention>,
    #[serde(default)]
    quotas: Vec<Quota>,
    #[serde(default)]
    integrations: Vec<Integration>,
}

#[derive(Deserialize)]
struct Project {
    name: String,
    worktrees: Vec<Worktree>,
}

#[derive(Clone, Deserialize)]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
    #[serde(default)]
    main: bool,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    prunable: bool,
    dirty: Option<bool>,
    #[serde(default)]
    added: usize,
    #[serde(default)]
    modified: usize,
    #[serde(default)]
    deleted: usize,
    #[serde(default)]
    untracked: usize,
    #[serde(default)]
    ahead: usize,
    #[serde(default)]
    behind: usize,
    #[serde(default)]
    activity: WorktreeActivity,
    #[serde(default, rename = "lastActivity")]
    last_activity: Option<u64>,
    #[serde(default)]
    priority: String,
    #[serde(default, rename = "agentIds")]
    agent_ids: Vec<String>,
}

#[derive(Clone, Default, Deserialize)]
struct WorktreeActivity {
    #[serde(default)]
    terminals: Vec<ActivityTerminal>,
    #[serde(default)]
    tmux: Vec<ActivityPane>,
}

#[derive(Clone, Deserialize)]
struct ActivityTerminal {
    id: String,
    title: String,
    kind: String,
    exited: bool,
}

#[derive(Clone, Deserialize)]
struct ActivityPane {
    session: String,
    window: String,
    pane: String,
    command: String,
    active: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    title: String,
    path: String,
    kind: String,
    exited: bool,
    exit_code: Option<u32>,
}

#[derive(Default, Deserialize)]
struct Tmux {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    sessions: Vec<TmuxSession>,
}

#[derive(Deserialize)]
struct TmuxSession {
    name: String,
    path: String,
    windows: u32,
    attached: bool,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Agent {
    id: String,
    provider: String,
    session_id: Option<String>,
    parent_id: Option<String>,
    worktree_path: Option<PathBuf>,
    #[serde(default)]
    cwd: PathBuf,
    title: String,
    model: Option<String>,
    state: String,
    waiting_reason: Option<String>,
    updated_at: Option<u64>,
    #[serde(default)]
    observed_at: u64,
    #[serde(default)]
    live: bool,
    #[serde(default)]
    stale: bool,
    target: Option<AgentTarget>,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentTarget {
    terminal_id: Option<String>,
    tmux_session: Option<String>,
    tmux_pane: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attention {
    id: String,
    agent_id: String,
    kind: String,
    summary: String,
    created_at: u64,
    request_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quota {
    id: String,
    provider: String,
    account_label: Option<String>,
    label: String,
    used_percent: Option<f64>,
    resets_at: Option<u64>,
    observed_at: u64,
    #[serde(default)]
    stale: bool,
    unavailable_reason: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Integration {
    provider: String,
    status: String,
    message: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum HomeRow {
    Section(&'static str),
    Tree(usize),
    Agent(usize, usize),
}

struct TreeRow {
    project: String,
    worktree: Worktree,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum View {
    #[default]
    Worktrees,
    Sessions,
    Integrations,
}

#[derive(Debug, Default, PartialEq)]
enum Mode {
    #[default]
    Browse,
    Search,
    Command {
        line: String,
        path: PathBuf,
    },
    Actions {
        selected: usize,
        context: ActionContext,
    },
    Activity {
        selected: usize,
        path: PathBuf,
        choices: Vec<ActivityChoice>,
    },
    Details {
        title: String,
        lines: Vec<String>,
        scroll: u16,
    },
    Reply {
        agent_id: String,
        request_id: Option<String>,
        line: String,
        path: PathBuf,
    },
    ConfirmAgent {
        id: String,
        action: String,
        request_id: Option<String>,
    },
    ConfirmQuit,
    ConfirmClose(String),
    ConfirmRemove {
        path: PathBuf,
        args: Vec<String>,
    },
}

#[derive(Debug, PartialEq)]
struct ActionContext {
    path: PathBuf,
    removable: bool,
    activity: Vec<ActivityChoice>,
}

#[derive(Debug, PartialEq)]
struct ActivityChoice {
    title: String,
    detail: String,
    action: Action,
}

#[derive(Debug, PartialEq)]
enum Action {
    Shell,
    Command(Vec<String>),
    NewTmux,
    Attach(String),
    AttachTmux {
        name: String,
        path: String,
        pane: Option<String>,
    },
    Agent {
        id: String,
        action: String,
        text: Option<String>,
        request_id: Option<String>,
    },
    Acknowledge(String),
    Integration {
        provider: String,
        action: String,
    },
    Close(String),
    Browser,
    Refresh,
    RequestQuit,
    Quit,
    AtPath {
        path: PathBuf,
        action: Box<Action>,
    },
}

const ACTIONS: &[(&str, &str)] = &[
    ("Open shell", "A full terminal in this worktree"),
    ("Start coding session", "Choose Claude, Codex, or OpenCode"),
    (
        "Resume coding session",
        "Search this project's saved sessions",
    ),
    ("Add worktree", "Pick a branch or type a new name"),
    ("Remove worktree", "Run Bonsai's normal removal checks"),
    (
        "Preview cleanup",
        "Show merged worktrees eligible for cleanup",
    ),
    (
        "Clean merged worktrees",
        "Review Bonsai's interactive cleanup plan",
    ),
    (
        "Prune workspace",
        "Review orphaned paths and stale registrations",
    ),
    (
        "Open persistent tmux shell",
        "Keep this shell across HQ restarts",
    ),
    (
        "Run a Bonsai command",
        "Enter any command and its arguments",
    ),
    (
        "Attach worktree activity",
        "Open this worktree's HQ terminals or tmux sessions",
    ),
];

struct Model {
    workspace: Workspace,
    rows: Vec<TreeRow>,
    filtered: Vec<usize>,
    home_rows: Vec<HomeRow>,
    expanded: BTreeSet<PathBuf>,
    collapsed_agents: BTreeSet<String>,
    older_open: bool,
    session_filtered: Vec<usize>,
    query: String,
    view: View,
    mode: Mode,
    worktree_state: ListState,
    session_state: ListState,
    integration_state: ListState,
    small: bool,
    message: String,
    busy: Option<String>,
    loaded: bool,
}

impl Model {
    fn new(root: PathBuf) -> Self {
        Self {
            workspace: Workspace {
                root,
                ..Workspace::default()
            },
            rows: Vec::new(),
            filtered: Vec::new(),
            home_rows: Vec::new(),
            expanded: BTreeSet::new(),
            collapsed_agents: BTreeSet::new(),
            older_open: false,
            session_filtered: Vec::new(),
            query: String::new(),
            view: View::Worktrees,
            mode: Mode::Browse,
            worktree_state: ListState::default(),
            session_state: ListState::default(),
            integration_state: ListState::default(),
            small: false,
            message: "Discovering your workspace…".into(),
            busy: None,
            loaded: false,
        }
    }

    fn apply(&mut self, workspace: Workspace) {
        let active_view = self.view;
        let selections = [View::Worktrees, View::Sessions, View::Integrations].map(|view| {
            self.view = view;
            self.selection_id()
        });
        self.view = active_view;
        self.rows = workspace
            .projects
            .iter()
            .flat_map(|project| {
                project.worktrees.iter().cloned().map(|worktree| TreeRow {
                    project: project.name.clone(),
                    worktree,
                })
            })
            .collect();
        self.workspace = workspace;
        if let Some(selected) = &selections[0] {
            let path = selected
                .strip_prefix("tree:")
                .map(PathBuf::from)
                .or_else(|| {
                    selected
                        .strip_prefix("agent:")
                        .and_then(|id| self.workspace.agents.iter().find(|agent| agent.id == id))
                        .and_then(|agent| agent.worktree_path.clone())
                });
            if let Some(path) = path
                && self
                    .rows
                    .iter()
                    .any(|row| row.worktree.path == path && row.worktree.priority == "older")
            {
                self.older_open = true;
            }
        }
        self.refilter();
        for (view, selected) in [View::Worktrees, View::Sessions, View::Integrations]
            .into_iter()
            .zip(selections)
        {
            self.view = view;
            self.restore_selection(selected.as_deref());
        }
        self.view = active_view;
        if !self.loaded {
            self.message = "Enter expand · [] needs you · Space actions".into();
        }
        self.loaded = true;
    }

    fn priority(&self, index: usize) -> &'static str {
        let tree = &self.rows[index].worktree;
        match tree.priority.as_str() {
            "needs-you" => "Needs you",
            "working" => "Working",
            "older" => "Older",
            _ => "Recent",
        }
    }

    fn tree_agents(&self, index: usize) -> Vec<usize> {
        let tree = &self.rows[index].worktree;
        self.workspace
            .agents
            .iter()
            .enumerate()
            .filter_map(|(index, agent)| {
                (agent.worktree_path.as_ref() == Some(&tree.path)
                    || tree.agent_ids.contains(&agent.id))
                .then_some(index)
            })
            .collect()
    }

    fn agent_search(&self, index: usize) -> String {
        let agent = &self.workspace.agents[index];
        format!(
            "{} {} {} {} {} {}",
            agent.provider,
            agent.title,
            agent.id,
            agent.model.as_deref().unwrap_or(""),
            agent.cwd.display(),
            agent.state
        )
    }

    fn append_agents(
        &mut self,
        indices: &[usize],
        parent: Option<&str>,
        depth: usize,
        visited: &mut BTreeSet<String>,
    ) {
        for &index in indices {
            let agent = &self.workspace.agents[index];
            let parent_exists = agent.parent_id.as_ref().is_some_and(|parent| {
                indices
                    .iter()
                    .any(|&index| self.workspace.agents[index].id == *parent)
            });
            let is_child = match parent {
                Some(parent) => agent.parent_id.as_deref() == Some(parent),
                None => !parent_exists,
            };
            if !is_child || !visited.insert(agent.id.clone()) {
                continue;
            }
            let id = agent.id.clone();
            self.home_rows.push(HomeRow::Agent(index, depth));
            if !self.collapsed_agents.contains(&id) || !self.query.is_empty() {
                self.append_agents(indices, Some(&id), depth + 1, visited);
            } else {
                let mut descendants = BTreeSet::from([id]);
                loop {
                    let before = descendants.len();
                    for &index in indices {
                        let child = &self.workspace.agents[index];
                        if child
                            .parent_id
                            .as_ref()
                            .is_some_and(|parent| descendants.contains(parent))
                        {
                            descendants.insert(child.id.clone());
                        }
                    }
                    if descendants.len() == before {
                        break;
                    }
                }
                visited.extend(descendants);
            }
        }
    }

    fn refilter(&mut self) {
        self.filtered = ranked(
            &self.query,
            self.rows.iter().enumerate().map(|(index, row)| {
                let agents = self
                    .tree_agents(index)
                    .into_iter()
                    .map(|index| self.agent_search(index))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    "{} {} {} {agents}",
                    row.project,
                    row.worktree.branch.as_deref().unwrap_or("detached"),
                    row.worktree.path.display()
                )
            }),
        );
        let rows = &self.rows;
        self.filtered.sort_by_key(|&index| {
            let priority = match rows[index].worktree.priority.as_str() {
                "needs-you" => 0,
                "working" => 1,
                "older" => 3,
                _ => 2,
            };
            (
                priority,
                std::cmp::Reverse(rows[index].worktree.last_activity),
                index,
            )
        });
        self.home_rows.clear();
        for section in ["Needs you", "Working", "Recent", "Older"] {
            let indices = self
                .filtered
                .iter()
                .copied()
                .filter(|&index| self.priority(index) == section)
                .collect::<Vec<_>>();
            if indices.is_empty() {
                continue;
            }
            if self.query.is_empty() {
                self.home_rows.push(HomeRow::Section(section));
            }
            if section == "Older" && !self.older_open && self.query.is_empty() {
                continue;
            }
            for index in indices {
                self.home_rows.push(HomeRow::Tree(index));
                if self.expanded.contains(&self.rows[index].worktree.path) || !self.query.is_empty()
                {
                    let agents = self.tree_agents(index);
                    let mut visited = BTreeSet::new();
                    self.append_agents(&agents, None, 1, &mut visited);
                    // Invalid or cyclic provider parent links must not hide sessions.
                    for index in agents {
                        if visited.insert(self.workspace.agents[index].id.clone()) {
                            self.home_rows.push(HomeRow::Agent(index, 1));
                        }
                    }
                }
            }
        }
        let unattached = self
            .workspace
            .agents
            .iter()
            .enumerate()
            .filter_map(|(index, agent)| {
                (!self.rows.iter().any(|row| {
                    agent.worktree_path.as_ref() == Some(&row.worktree.path)
                        || row.worktree.agent_ids.contains(&agent.id)
                }) && fuzzy_score(&self.query, &self.agent_search(index)).is_some())
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if !unattached.is_empty() {
            self.home_rows.push(HomeRow::Section("Other agents"));
            let mut visited = BTreeSet::new();
            self.append_agents(&unattached, None, 0, &mut visited);
            for index in unattached {
                if visited.insert(self.workspace.agents[index].id.clone()) {
                    self.home_rows.push(HomeRow::Agent(index, 0));
                }
            }
        }
        let sessions = self
            .workspace
            .terminals
            .iter()
            .map(|session| format!("{} {} {}", session.title, session.kind, session.path));
        let tmux = self
            .workspace
            .tmux
            .sessions
            .iter()
            .map(|session| format!("tmux {} {}", session.name, session.path));
        self.session_filtered = ranked(&self.query, sessions.chain(tmux));
        clamp_selection(&mut self.worktree_state, self.home_rows.len());
        if self
            .selected_home()
            .is_some_and(|row| matches!(row, HomeRow::Section(section) if *section != "Older"))
        {
            self.move_home(1);
        }
        clamp_selection(&mut self.session_state, self.session_filtered.len());
        clamp_selection(
            &mut self.integration_state,
            self.workspace.integrations.len(),
        );
    }

    fn selected_home(&self) -> Option<&HomeRow> {
        self.home_rows.get(self.worktree_state.selected()?)
    }

    fn agent(&self) -> Option<&Agent> {
        if self.view != View::Worktrees {
            return None;
        }
        match self.selected_home()? {
            HomeRow::Agent(index, _) => self.workspace.agents.get(*index),
            _ => None,
        }
    }

    fn worktree(&self) -> Option<&TreeRow> {
        let index = match self.selected_home()? {
            HomeRow::Tree(index) => *index,
            HomeRow::Agent(index, _) => self.rows.iter().position(|row| {
                self.workspace.agents[*index].worktree_path.as_ref() == Some(&row.worktree.path)
            })?,
            _ => return None,
        };
        self.rows.get(index)
    }

    fn raw_session_path(&self) -> Option<PathBuf> {
        self.session_filtered
            .get(self.session_state.selected().unwrap_or(0))
            .and_then(|&index| {
                self.workspace
                    .terminals
                    .get(index)
                    .map(|session| PathBuf::from(&session.path))
                    .or_else(|| {
                        self.workspace
                            .tmux
                            .sessions
                            .get(index.saturating_sub(self.workspace.terminals.len()))
                            .map(|session| PathBuf::from(&session.path))
                    })
            })
    }

    fn path(&self) -> PathBuf {
        let path = match self.view {
            View::Sessions => self.raw_session_path(),
            View::Worktrees => self
                .agent()
                .map(|agent| {
                    agent
                        .worktree_path
                        .clone()
                        .unwrap_or_else(|| agent.cwd.clone())
                })
                .or_else(|| self.worktree().map(|row| row.worktree.path.clone())),
            View::Integrations => None,
        }
        .unwrap_or_else(|| self.workspace.root.clone());
        self.rows
            .iter()
            .filter(|row| path.starts_with(&row.worktree.path))
            .max_by_key(|row| row.worktree.path.components().count())
            .map(|row| row.worktree.path.clone())
            .unwrap_or(path)
    }

    fn has_worktree(&self) -> bool {
        let path = self.path();
        self.rows.iter().any(|row| row.worktree.path == path)
    }

    fn home_id(&self, row: &HomeRow) -> String {
        match row {
            HomeRow::Section(section) => format!("section:{section}"),
            HomeRow::Tree(index) => format!("tree:{}", self.rows[*index].worktree.path.display()),
            HomeRow::Agent(index, _) => format!("agent:{}", self.workspace.agents[*index].id),
        }
    }

    fn selection_id(&self) -> Option<String> {
        match self.view {
            View::Worktrees => self.selected_home().map(|row| self.home_id(row)),
            View::Sessions => self
                .session_filtered
                .get(self.session_state.selected()?)
                .and_then(|&index| self.session_action(index))
                .map(|action| format!("{action:?}")),
            View::Integrations => self
                .workspace
                .integrations
                .get(self.integration_state.selected()?)
                .map(|integration| integration.provider.clone()),
        }
    }

    fn restore_selection(&mut self, selected: Option<&str>) {
        let Some(selected) = selected else {
            return;
        };
        let index = match self.view {
            View::Worktrees => self
                .home_rows
                .iter()
                .position(|row| self.home_id(row) == selected),
            View::Sessions => self.session_filtered.iter().position(|&index| {
                self.session_action(index)
                    .is_some_and(|action| format!("{action:?}") == selected)
            }),
            View::Integrations => self
                .workspace
                .integrations
                .iter()
                .position(|integration| integration.provider == selected),
        };
        if let Some(index) = index {
            match self.view {
                View::Worktrees => self.worktree_state.select(Some(index)),
                View::Sessions => self.session_state.select(Some(index)),
                View::Integrations => self.integration_state.select(Some(index)),
            }
        }
    }

    fn move_home(&mut self, direction: isize) {
        let len = self.home_rows.len();
        if len == 0 {
            return;
        }
        let start = self.worktree_state.selected().unwrap_or(0);
        let mut index = start.saturating_add_signed(direction).min(len - 1);
        while matches!(&self.home_rows[index], HomeRow::Section(section) if *section != "Older") {
            let next = index
                .saturating_add_signed(if direction < 0 { -1 } else { 1 })
                .min(len - 1);
            if next == index {
                let mut fallback = self.home_rows.iter().enumerate().filter(
                    |(_, row)| !matches!(row, HomeRow::Section(section) if *section != "Older"),
                );
                if let Some((index, _)) = if direction < 0 {
                    fallback.next()
                } else {
                    fallback.next_back()
                } {
                    self.worktree_state.select(Some(index));
                }
                return;
            }
            index = next;
        }
        self.worktree_state.select(Some(index));
    }

    fn move_selection(&mut self, direction: isize) {
        if self.view == View::Worktrees {
            self.move_home(direction);
            return;
        }
        let (selection, len) = match self.view {
            View::Sessions => (&mut self.session_state, self.session_filtered.len()),
            View::Integrations => (
                &mut self.integration_state,
                self.workspace.integrations.len(),
            ),
            View::Worktrees => unreachable!(),
        };
        if len > 0 {
            selection.select(Some(
                selection
                    .selected()
                    .unwrap_or(0)
                    .saturating_add_signed(direction)
                    .min(len - 1),
            ));
        }
    }

    fn toggle(&mut self, expand: Option<bool>) {
        let selected = self.selection_id();
        match self.selected_home().cloned() {
            Some(HomeRow::Section("Older")) => self.older_open = expand.unwrap_or(!self.older_open),
            Some(HomeRow::Tree(index)) => {
                let path = self.rows[index].worktree.path.clone();
                if expand.unwrap_or(!self.expanded.contains(&path)) {
                    self.expanded.insert(path);
                } else {
                    self.expanded.remove(&path);
                }
            }
            Some(HomeRow::Agent(index, _)) => {
                let agent = &self.workspace.agents[index];
                let has_children = self
                    .workspace
                    .agents
                    .iter()
                    .any(|child| child.parent_id.as_deref() == Some(&agent.id));
                if has_children {
                    if expand.unwrap_or(self.collapsed_agents.contains(&agent.id)) {
                        self.collapsed_agents.remove(&agent.id);
                    } else {
                        self.collapsed_agents.insert(agent.id.clone());
                    }
                } else if expand == Some(false) {
                    if let Some(parent) = &agent.parent_id {
                        let parent = format!("agent:{parent}");
                        self.restore_selection(Some(&parent));
                        return;
                    }
                    if let Some(path) = &agent.worktree_path {
                        let parent = format!("tree:{}", path.display());
                        self.restore_selection(Some(&parent));
                        return;
                    }
                }
            }
            _ => {}
        }
        self.refilter();
        self.restore_selection(selected.as_deref());
    }

    fn activate(&mut self) -> Option<Action> {
        match self.view {
            View::Worktrees => {
                if let Some(agent) = self.agent() {
                    let action = agent_attach(agent);
                    if action.is_none() {
                        self.message = "This agent has no attachment or resumable session. i shows details; 3 opens integrations.".into();
                    }
                    return action;
                }
                self.toggle(None);
                None
            }
            View::Sessions => self
                .session_filtered
                .get(self.session_state.selected().unwrap_or(0))
                .and_then(|&index| self.session_action(index)),
            View::Integrations => {
                self.open_integration_actions();
                None
            }
        }
    }

    fn session_action(&self, index: usize) -> Option<Action> {
        if let Some(session) = self.workspace.terminals.get(index) {
            Some(Action::Attach(session.id.clone()))
        } else {
            let session = self
                .workspace
                .tmux
                .sessions
                .get(index - self.workspace.terminals.len())?;
            Some(Action::AttachTmux {
                name: session.name.clone(),
                path: session.path.clone(),
                pane: None,
            })
        }
    }

    fn next_attention(&mut self, direction: isize) {
        let attention = &self.workspace.attention;
        if attention.is_empty() {
            self.message = "Nothing needs your attention.".into();
            return;
        }
        let current = self
            .agent()
            .and_then(|agent| attention.iter().position(|item| item.agent_id == agent.id));
        let index = match current {
            Some(index) => {
                (index as isize + direction).rem_euclid(attention.len() as isize) as usize
            }
            None if direction < 0 => attention.len() - 1,
            None => 0,
        };
        let item = attention[index].clone();
        if let Some(agent) = self
            .workspace
            .agents
            .iter()
            .find(|agent| agent.id == item.agent_id)
        {
            self.view = View::Worktrees;
            self.query.clear();
            self.older_open = true;
            if let Some(path) = &agent.worktree_path {
                self.expanded.insert(path.clone());
            }
            self.collapsed_agents.clear();
            self.refilter();
            self.restore_selection(Some(&format!("agent:{}", item.agent_id)));
            self.message = item.summary;
        }
    }

    fn open_agent_actions(&mut self) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        let request_id = self
            .workspace
            .attention
            .iter()
            .filter(|item| {
                item.agent_id == agent.id && matches!(item.kind.as_str(), "question" | "approval")
            })
            .max_by_key(|item| item.created_at)
            .and_then(|item| item.request_id.clone());
        let mut choices = Vec::new();
        if agent.target.is_some()
            && let Some(action) = agent_attach(&agent)
        {
            choices.push(ActivityChoice {
                title: "Attach agent".into(),
                detail: "Open its exact terminal or tmux pane".into(),
                action,
            });
        }
        if agent
            .capabilities
            .iter()
            .any(|capability| capability == "resume")
            && let Some(id) = &agent.session_id
        {
            choices.push(ActivityChoice {
                title: "Resume session".into(),
                detail: format!("{} · {id}", agent.provider),
                action: at_path(
                    Action::Command(vec![
                        "resume".into(),
                        "--provider".into(),
                        agent.provider.clone(),
                        "--session".into(),
                        id.clone(),
                    ]),
                    self.path(),
                ),
            });
        }
        for capability in ["reply", "interrupt", "approve", "reject"] {
            if !agent.stale
                && agent.capabilities.iter().any(|value| value == capability)
                && (!matches!(capability, "approve" | "reject") || request_id.is_some())
            {
                choices.push(ActivityChoice {
                    title: capability.to_string(),
                    detail: agent
                        .waiting_reason
                        .clone()
                        .unwrap_or_else(|| agent.title.clone()),
                    action: Action::Agent {
                        id: agent.id.clone(),
                        action: capability.into(),
                        text: None,
                        request_id: request_id.clone(),
                    },
                });
            }
        }
        for item in self
            .workspace
            .attention
            .iter()
            .filter(|item| item.agent_id == agent.id && item.kind == "completed")
        {
            choices.push(ActivityChoice {
                title: "Mark reviewed".into(),
                detail: item.summary.clone(),
                action: Action::Acknowledge(item.id.clone()),
            });
        }
        if choices.is_empty() {
            self.message =
                "No controls available. Open details with i for integration status.".into();
            return;
        }
        self.mode = Mode::Activity {
            selected: 0,
            path: self.path(),
            choices,
        };
    }

    fn open_integration_actions(&mut self) {
        let Some(integration) = self
            .workspace
            .integrations
            .get(self.integration_state.selected().unwrap_or(0))
        else {
            return;
        };
        let choices = ["install", "repair", "disable", "uninstall"]
            .into_iter()
            .map(|action| ActivityChoice {
                title: format!(
                    "{} {} integration",
                    match action {
                        "install" => "Install",
                        "repair" => "Repair",
                        "disable" => "Disable",
                        _ => "Remove",
                    },
                    integration.provider
                ),
                detail: integration.message.clone(),
                action: Action::Integration {
                    provider: integration.provider.clone(),
                    action: action.into(),
                },
            })
            .collect();
        self.mode = Mode::Activity {
            selected: 0,
            path: self.workspace.root.clone(),
            choices,
        };
    }

    fn details(&mut self) {
        let (title, mut lines) = if let Some(agent) = self.agent() {
            let mut lines = vec![
                format!(
                    "{} · {}{}",
                    agent.provider,
                    agent.state,
                    if agent.stale { " · stale" } else { "" }
                ),
                agent.title.clone(),
                format!("Directory: {}", agent.cwd.display()),
                format!("Model: {}", agent.model.as_deref().unwrap_or("unknown")),
                format!(
                    "Session: {}",
                    agent.session_id.as_deref().unwrap_or("unknown")
                ),
                format!(
                    "Updated: {} · observed: {}",
                    age(agent.updated_at),
                    age(Some(agent.observed_at))
                ),
                format!(
                    "Live: {} · controls: {}",
                    agent.live,
                    agent.capabilities.join(", ")
                ),
            ];
            if let Some(reason) = &agent.waiting_reason {
                lines.push(reason.clone());
            }
            if let Some(parent) = &agent.parent_id {
                lines.push(format!("Child of {parent}"));
            }
            lines.extend(
                self.workspace
                    .attention
                    .iter()
                    .filter(|item| item.agent_id == agent.id)
                    .map(|item| {
                        format!(
                            "{} · {} · {}",
                            item.kind,
                            age(Some(item.created_at)),
                            item.summary
                        )
                    }),
            );
            (agent.title.clone(), lines)
        } else if self.view == View::Integrations {
            let lines = self
                .workspace
                .integrations
                .iter()
                .flat_map(|integration| {
                    [
                        format!("{} · {}", integration.provider, integration.status),
                        integration.message.clone(),
                        format!("Capabilities: {}", integration.capabilities.join(", ")),
                    ]
                })
                .collect();
            ("Integrations and quotas".into(), lines)
        } else if let Some(row) = self.worktree().filter(|_| self.view == View::Worktrees) {
            let tree = &row.worktree;
            let mut lines = vec![
                format!(
                    "{} · {}",
                    row.project,
                    tree.branch.as_deref().unwrap_or("detached")
                ),
                tree.path.display().to_string(),
                format!("Last activity: {}", age(tree.last_activity)),
                format!(
                    "Changes: +{} ~{} −{} ?{} · ↑{} ↓{}",
                    tree.added,
                    tree.modified,
                    tree.deleted,
                    tree.untracked,
                    tree.ahead,
                    tree.behind
                ),
                format!(
                    "Main: {} · external: {} · locked: {} · unavailable: {}",
                    tree.main, tree.external, tree.locked, tree.prunable
                ),
            ];
            lines.extend(
                activity_choices(row)
                    .into_iter()
                    .map(|choice| format!("{} · {}", choice.title, choice.detail)),
            );
            ("Worktree details".into(), lines)
        } else {
            (
                "Terminal details".into(),
                vec![
                    self.raw_session_path()
                        .unwrap_or_else(|| self.path())
                        .display()
                        .to_string(),
                    format!(
                        "Command worktree: {}",
                        if self.has_worktree() {
                            self.path().display().to_string()
                        } else {
                            "unavailable".into()
                        }
                    ),
                    "Ctrl+] returns to HQ without closing the terminal.".into(),
                ],
            )
        };
        if self.view == View::Integrations {
            for quota in &self.workspace.quotas {
                lines.push(format!(
                    "{} · {} · {} · {}",
                    quota.provider,
                    quota.account_label.as_deref().unwrap_or("account unknown"),
                    quota.label,
                    quota_value(quota)
                ));
                lines.push(format!(
                    "{} · observed {} · reset {}",
                    quota.id,
                    age(Some(quota.observed_at)),
                    quota
                        .resets_at
                        .map_or_else(|| "unknown".into(), |time| time.to_string())
                ));
                if let Some(reason) = &quota.unavailable_reason {
                    lines.push(reason.clone());
                }
            }
        }
        lines.extend(self.workspace.warnings.iter().cloned());
        self.mode = Mode::Details {
            title,
            lines,
            scroll: 0,
        };
    }

    fn request_remove(&mut self) -> Option<Action> {
        self.request_remove_at(self.action_context())
    }

    fn action_context(&self) -> ActionContext {
        let path = self.path();
        let tree = self
            .rows
            .iter()
            .filter(|row| path.starts_with(&row.worktree.path))
            .max_by_key(|row| row.worktree.path.components().count());
        ActionContext {
            path,
            removable: tree.is_some_and(|row| !row.worktree.main && !row.worktree.external),
            activity: tree.map(activity_choices).unwrap_or_default(),
        }
    }

    fn request_activity(&mut self, context: ActionContext) -> Option<Action> {
        if context.activity.is_empty() {
            self.message = "No terminal activity in this worktree. Press t to open a shell.".into();
        } else {
            self.mode = Mode::Activity {
                selected: 0,
                path: context.path,
                choices: context.activity,
            };
        }
        None
    }

    fn request_remove_at(&mut self, context: ActionContext) -> Option<Action> {
        if !context.removable {
            self.message = "Select a managed worktree to remove.".into();
            return None;
        }
        let path = context.path;
        let target = path.display().to_string();
        self.mode = Mode::ConfirmRemove {
            path,
            args: vec!["remove".into(), "--".into(), target],
        };
        None
    }

    fn menu_action(&mut self, selected: usize, context: ActionContext) -> Option<Action> {
        let action = match selected {
            0 => Action::Shell,
            1 => command("start"),
            2 => command("resume"),
            3 => command("add"),
            4 => return self.request_remove_at(context),
            5 => Action::Command(vec!["clean".into(), "--dry-run".into()]),
            6 => command("clean"),
            7 => command("prune"),
            8 => Action::NewTmux,
            10 => return self.request_activity(context),
            _ => {
                self.mode = Mode::Command {
                    line: String::new(),
                    path: context.path,
                };
                return None;
            }
        };
        Some(at_path(action, context.path))
    }

    fn key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        if self.small {
            if matches!(self.mode, Mode::ConfirmQuit) {
                return match key.code {
                    KeyCode::Enter | KeyCode::Char('q' | 'y') => Some(Action::Quit),
                    KeyCode::Esc | KeyCode::Char('n') => {
                        self.mode = Mode::Browse;
                        None
                    }
                    _ => None,
                };
            }
            return match key.code {
                KeyCode::Char('b') => Some(Action::Browser),
                KeyCode::Char('q') => Some(Action::RequestQuit),
                _ => None,
            };
        }
        let mode = std::mem::take(&mut self.mode);
        match mode {
            Mode::Details {
                title,
                lines,
                mut scroll,
            } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('i') => return None,
                    KeyCode::Down | KeyCode::Char('j') => {
                        scroll = scroll
                            .saturating_add(1)
                            .min(lines.len().saturating_sub(1) as u16)
                    }
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    _ => {}
                }
                self.mode = Mode::Details {
                    title,
                    lines,
                    scroll,
                };
            }
            Mode::Reply {
                agent_id,
                request_id,
                mut line,
                path,
            } => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter if !line.trim().is_empty() => {
                        return Some(Action::Agent {
                            id: agent_id,
                            action: "reply".into(),
                            text: Some(line),
                            request_id,
                        });
                    }
                    KeyCode::Backspace => {
                        line.pop();
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        line.push(ch)
                    }
                    _ => {}
                }
                self.mode = Mode::Reply {
                    agent_id,
                    request_id,
                    line,
                    path,
                };
            }
            Mode::ConfirmAgent {
                id,
                action,
                request_id,
            } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    return Some(Action::Agent {
                        id,
                        action,
                        text: None,
                        request_id,
                    });
                }
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => {
                    self.mode = Mode::ConfirmAgent {
                        id,
                        action,
                        request_id,
                    }
                }
            },
            Mode::Search => {
                let previous = self.query.clone();
                match key.code {
                    KeyCode::Enter => return None,
                    KeyCode::Esc => {
                        self.query.clear();
                        self.refilter();
                        return None;
                    }
                    KeyCode::Backspace => {
                        self.query.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.query.clear()
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.query.push(ch)
                    }
                    KeyCode::Down => self.move_selection(1),
                    KeyCode::Up => self.move_selection(-1),
                    _ => {}
                }
                if self.query != previous {
                    self.worktree_state.select(Some(0));
                    self.session_state.select(Some(0));
                }
                self.refilter();
                self.mode = Mode::Search;
            }
            Mode::Command { mut line, path } => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => match parse_command(&line) {
                        Ok(args) => return Some(at_path(Action::Command(args), path)),
                        Err(error) => self.message = error,
                    },
                    KeyCode::Backspace => {
                        line.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        line.clear()
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        line.push(ch)
                    }
                    _ => {}
                }
                self.mode = Mode::Command { line, path };
            }
            Mode::Actions { selected, context } => {
                let next = match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => return self.menu_action(selected, context),
                    KeyCode::Down | KeyCode::Char('j') => (selected + 1).min(ACTIONS.len() - 1),
                    KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
                    _ => selected,
                };
                self.mode = Mode::Actions {
                    selected: next,
                    context,
                };
            }
            Mode::Activity {
                selected,
                path,
                mut choices,
            } => {
                let next = match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter if !choices.is_empty() => {
                        let action = choices.remove(selected).action;
                        return match action {
                            Action::Agent {
                                id,
                                action,
                                request_id,
                                ..
                            } if action == "reply" => {
                                self.mode = Mode::Reply {
                                    agent_id: id,
                                    request_id,
                                    line: String::new(),
                                    path,
                                };
                                None
                            }
                            Action::Agent {
                                id,
                                action,
                                request_id,
                                ..
                            } if matches!(action.as_str(), "interrupt" | "approve" | "reject") => {
                                self.mode = Mode::ConfirmAgent {
                                    id,
                                    action,
                                    request_id,
                                };
                                None
                            }
                            action => Some(action),
                        };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        (selected + 1).min(choices.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
                    _ => selected,
                };
                self.mode = Mode::Activity {
                    selected: next,
                    path,
                    choices,
                };
            }
            Mode::ConfirmQuit => match key.code {
                KeyCode::Char('q' | 'y') | KeyCode::Enter => return Some(Action::Quit),
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.mode = Mode::ConfirmQuit,
            },
            Mode::ConfirmClose(id) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => return Some(Action::Close(id)),
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.mode = Mode::ConfirmClose(id),
            },
            Mode::ConfirmRemove { path, args } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    return Some(at_path(Action::Command(args), path));
                }
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.mode = Mode::ConfirmRemove { path, args },
            },
            Mode::Browse => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::PageDown => self.move_selection(10),
                KeyCode::PageUp => self.move_selection(-10),
                KeyCode::Char('g') | KeyCode::Home => self.move_selection(isize::MIN),
                KeyCode::Char('G') | KeyCode::End => self.move_selection(isize::MAX),
                KeyCode::Tab | KeyCode::BackTab => {
                    self.view = match self.view {
                        View::Worktrees => View::Sessions,
                        View::Sessions => View::Integrations,
                        View::Integrations => View::Worktrees,
                    }
                }
                KeyCode::Char('1') => self.view = View::Worktrees,
                KeyCode::Char('2') => self.view = View::Sessions,
                KeyCode::Char('3') => self.view = View::Integrations,
                KeyCode::Char('[') => self.next_attention(-1),
                KeyCode::Char(']') => self.next_attention(1),
                KeyCode::Left | KeyCode::Char('h') if self.view == View::Worktrees => {
                    self.toggle(Some(false))
                }
                KeyCode::Right | KeyCode::Char('l') if self.view == View::Worktrees => {
                    self.toggle(Some(true))
                }
                KeyCode::Char('i') => self.details(),
                KeyCode::Char('/') => self.mode = Mode::Search,
                KeyCode::Char(':') => {
                    self.mode = Mode::Command {
                        line: String::new(),
                        path: self.path(),
                    }
                }
                KeyCode::Char(' ') if self.agent().is_some() => self.open_agent_actions(),
                KeyCode::Char(' ') if self.view == View::Integrations => {
                    self.open_integration_actions()
                }
                KeyCode::Char(' ') => {
                    self.mode = Mode::Actions {
                        selected: 0,
                        context: self.action_context(),
                    }
                }
                KeyCode::Char('q') => return Some(Action::RequestQuit),
                KeyCode::Char('b') => return Some(Action::Browser),
                KeyCode::Char('R') => return Some(Action::Refresh),
                KeyCode::Enter => return self.activate(),
                KeyCode::Char('t') if !self.has_worktree() => {
                    return Some(at_path(Action::Shell, self.workspace.root.clone()));
                }
                KeyCode::Char('t') => return Some(Action::Shell),
                KeyCode::Char('s' | 'r' | 'c' | 'p' | 'd') if !self.has_worktree() => {
                    self.message = "Selected runtime has no known worktree. Attach it or choose a worktree first.".into();
                }
                KeyCode::Char('s') => return Some(command("start")),
                KeyCode::Char('r') => return Some(command("resume")),
                KeyCode::Char('a') => return Some(command("add")),
                KeyCode::Char('c') => return Some(command("clean")),
                KeyCode::Char('p') => return Some(command("prune")),
                KeyCode::Char('m') if !self.has_worktree() => {
                    return Some(at_path(Action::NewTmux, self.workspace.root.clone()));
                }
                KeyCode::Char('m') => return Some(Action::NewTmux),
                KeyCode::Char('e') if self.view == View::Worktrees => {
                    return self.request_activity(self.action_context());
                }
                KeyCode::Char('d') if self.view == View::Worktrees => {
                    return self.request_remove();
                }
                KeyCode::Char('x') if self.view == View::Sessions => {
                    if let Some(Action::Attach(id)) = self.activate() {
                        self.mode = Mode::ConfirmClose(id);
                    } else {
                        self.message = "Closing a terminal detaches its tmux client; the tmux session stays available.".into();
                    }
                }
                KeyCode::Esc => {
                    self.query.clear();
                    self.refilter();
                }
                _ => {}
            },
        }
        None
    }
}

fn activity_choices(row: &TreeRow) -> Vec<ActivityChoice> {
    let mut choices = row
        .worktree
        .activity
        .terminals
        .iter()
        .map(|terminal| ActivityChoice {
            title: terminal.title.clone(),
            detail: format!(
                "HQ {} · {}",
                terminal.kind,
                if terminal.exited {
                    "completed output"
                } else {
                    "running"
                }
            ),
            action: Action::Attach(terminal.id.clone()),
        })
        .collect::<Vec<_>>();
    for pane in &row.worktree.activity.tmux {
        choices.push(ActivityChoice {
            title: format!("tmux · {} {}/{}", pane.session, pane.window, pane.pane),
            detail: format!(
                "{}{}",
                pane.command,
                if pane.active { " · focused" } else { "" }
            ),
            action: Action::AttachTmux {
                name: pane.session.clone(),
                path: row.worktree.path.display().to_string(),
                pane: Some(pane.pane.clone()),
            },
        });
    }
    choices
}

fn agent_attach(agent: &Agent) -> Option<Action> {
    if agent
        .capabilities
        .iter()
        .any(|capability| capability == "attach")
        && let Some(target) = &agent.target
    {
        if let Some(id) = &target.terminal_id {
            return Some(Action::Attach(id.clone()));
        }
        if let Some(name) = &target.tmux_session {
            return Some(Action::AttachTmux {
                name: name.clone(),
                path: agent
                    .worktree_path
                    .as_ref()
                    .unwrap_or(&agent.cwd)
                    .display()
                    .to_string(),
                pane: target.tmux_pane.clone(),
            });
        }
    }
    if !agent.live
        && matches!(agent.provider.as_str(), "claude" | "codex" | "opencode")
        && agent
            .capabilities
            .iter()
            .any(|capability| capability == "resume")
        && let Some(id) = &agent.session_id
    {
        return Some(at_path(
            Action::Command(vec![
                "resume".into(),
                "--provider".into(),
                agent.provider.clone(),
                "--session".into(),
                id.clone(),
            ]),
            agent
                .worktree_path
                .clone()
                .unwrap_or_else(|| agent.cwd.clone()),
        ));
    }
    None
}

fn age(time: Option<u64>) -> String {
    let Some(time) = time else {
        return "unknown".into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = now.saturating_sub(time);
    if seconds < 60 {
        "now".into()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn compact_title(title: &str, width: usize) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if Span::raw(&title).width() <= width {
        return title;
    }
    let mut truncated = String::new();
    for ch in title.chars() {
        if Span::raw(format!("{truncated}{ch}…")).width() > width {
            break;
        }
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

fn quota_value(quota: &Quota) -> String {
    let value = quota
        .used_percent
        .filter(|value| value.is_finite())
        .map_or_else(|| "unknown".into(), |used| format!("{used:.0}% used"));
    format!("{value}{}", if quota.stale { " · stale" } else { "" })
}

fn command(name: &str) -> Action {
    Action::Command(vec![name.into()])
}

fn at_path(action: Action, path: PathBuf) -> Action {
    Action::AtPath {
        path,
        action: Box::new(action),
    }
}

fn parse_command(line: &str) -> Result<Vec<String>, String> {
    let mut args = shlex::split(line)
        .ok_or_else(|| "Close the quote before running this command.".to_string())?;
    if args.first().is_some_and(|arg| arg == "bonsai") {
        args.remove(0);
    }
    validate_bonsai_args(&args).map_err(|error| error.message)?;
    Ok(args)
}

fn clamp_selection(state: &mut ListState, len: usize) {
    state.select((len > 0).then(|| state.selected().unwrap_or(0).min(len.saturating_sub(1))));
}

fn fuzzy_score(query: &str, text: &str) -> Option<usize> {
    let text = text.to_lowercase();
    let mut remaining = text.char_indices();
    let mut score = 0;
    let mut previous = None;
    for needle in query
        .to_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
    {
        let (position, _) = remaining.find(|(_, ch)| *ch == needle)?;
        score += position;
        if previous.is_some_and(|previous| position == previous + 1) {
            score = score.saturating_sub(2);
        }
        previous = Some(position);
    }
    Some(score)
}

fn ranked(query: &str, values: impl Iterator<Item = String>) -> Vec<usize> {
    let mut matches = values
        .enumerate()
        .filter_map(|(index, value)| fuzzy_score(query, &value).map(|score| (score, index)))
        .collect::<Vec<_>>();
    matches.sort_by_key(|&(score, index)| (score, index));
    matches.into_iter().map(|(_, index)| index).collect()
}

type RefreshResult = Result<Workspace, String>;

struct RefreshWorker {
    latest: Arc<Mutex<Option<RefreshResult>>>,
    wake: mpsc::SyncSender<()>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RefreshWorker {
    fn new(state: AppState) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let target = Arc::clone(&latest);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let (wake, receive) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let result = snapshot_value(&state)
                    .and_then(|value| serde_json::from_value(value).map_err(Into::into))
                    .map_err(|error| format!("{error:#}"));
                *target.lock().unwrap_or_else(|error| error.into_inner()) = Some(result);
                if matches!(
                    receive.recv_timeout(Duration::from_secs(1)),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                ) {
                    break;
                }
            }
        });
        Self {
            latest,
            wake,
            stopped,
            worker: Some(worker),
        }
    }

    fn request(&self) {
        let _ = self.wake.try_send(());
    }
    fn take(&self) -> Option<RefreshResult> {
        self.latest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

impl Drop for RefreshWorker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.request();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Screen {
    terminal: DefaultTerminal,
    active: bool,
}

impl Screen {
    fn new() -> Result<Self> {
        let terminal = match ratatui::try_init() {
            Ok(terminal) => terminal,
            Err(error) => {
                ratatui::restore();
                return Err(error.into());
            }
        };
        let screen = Self {
            terminal,
            active: true,
        };
        execute!(stdout(), event::EnableBracketedPaste)?;
        Ok(screen)
    }

    fn suspend(&mut self) {
        if self.active {
            let _ = execute!(stdout(), event::DisableBracketedPaste);
            let _ = self.terminal.show_cursor();
            ratatui::restore();
            self.active = false;
        }
    }

    fn resume(&mut self) -> Result<()> {
        self.active = true;
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen, event::EnableBracketedPaste)?;
        self.terminal.clear()?;
        Ok(())
    }

    fn attach(&mut self, session: Arc<terminal::TerminalSession>, stop: &AtomicBool) -> Result<()> {
        self.suspend();
        let mut result = terminal::attach_local(Arc::clone(&session));
        if result.is_ok() && session.exited() && !stop.load(Ordering::Acquire) {
            result = wait_after_exit(stop);
        }
        self.resume()?;
        result
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.suspend();
    }
}

fn wait_after_exit(stop: &AtomicBool) -> Result<()> {
    struct RawMode;
    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    enable_raw_mode()?;
    let _raw = RawMode;
    write!(
        stdout(),
        "\r\n\x1b[90mProcess exited. Enter or Ctrl-] returns to Bonsai HQ.\x1b[0m\r\n"
    )?;
    stdout().flush()?;
    while !stop.load(Ordering::Acquire) {
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                stop.store(true, Ordering::Release);
                break;
            }
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                || key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(']')
            {
                break;
            }
        }
    }
    Ok(())
}

enum JobResult {
    Attach(String),
    Message(String),
}

pub(super) fn run(state: AppState, stop: Arc<AtomicBool>) -> Result<()> {
    let refresh = RefreshWorker::new(state.clone());
    let mut screen = Screen::new().context("cannot initialize the HQ terminal interface")?;
    let mut model = Model::new(state.config.root_dir());
    let (jobs, finished) = mpsc::channel::<Result<JobResult, String>>();
    let address = state
        .security
        .launch_url()
        .split('#')
        .next()
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    while !stop.load(Ordering::Acquire) {
        if let Some(update) = refresh.take() {
            match update {
                Ok(workspace) => model.apply(workspace),
                Err(error) => model.message = error,
            }
        }
        if let Some(notice) = state
            .notice
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            model.message = notice;
        }
        if let Ok(result) = finished.try_recv() {
            model.busy = None;
            match result {
                Ok(JobResult::Attach(id)) => {
                    if let Some(session) = state.terminals.get(&id) {
                        model.message = match screen.attach(session, &stop) {
                            Ok(()) => {
                                "Back at HQ. The browser can attach to the same terminal.".into()
                            }
                            Err(error) => format!("{error:#}"),
                        };
                    }
                }
                Ok(JobResult::Message(message)) => model.message = message,
                Err(error) => model.message = error,
            }
            refresh.request();
        }
        screen
            .terminal
            .draw(|frame| draw(frame, &mut model, &address))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let action = match event::read()? {
            Event::Key(key) => model.key(key),
            Event::Paste(text) => {
                match &mut model.mode {
                    Mode::Command { line, .. } | Mode::Reply { line, .. } if !model.small => {
                        line.push_str(&text.replace(['\r', '\n'], " "))
                    }
                    Mode::Search if !model.small => {
                        model.query.push_str(&text.replace(['\r', '\n'], " "));
                        model.refilter();
                    }
                    _ => {}
                }
                None
            }
            _ => None,
        };
        let Some(action) = action else {
            continue;
        };
        match action {
            Action::Quit => break,
            Action::RequestQuit => {
                if state.terminals.list().iter().any(|session| !session.exited) {
                    model.mode = Mode::ConfirmQuit;
                } else {
                    break;
                }
            }
            Action::Refresh => {
                refresh.request();
                model.message = "Refreshing workspace…".into();
            }
            Action::Attach(id) => {
                if let Some(session) = state.terminals.get(&id) {
                    if let Some(summary) = state
                        .terminals
                        .list()
                        .into_iter()
                        .find(|session| session.id == id)
                    {
                        let state = state.clone();
                        std::thread::spawn(move || {
                            if let Err(error) =
                                state.agents.visit(std::path::Path::new(&summary.path))
                            {
                                *state
                                    .notice
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner()) =
                                    Some(format!("Could not save recent worktree: {error}"));
                            }
                        });
                    }
                    model.message = match screen.attach(session, &stop) {
                        Ok(()) => {
                            "Detached. The terminal stays available in both interfaces.".into()
                        }
                        Err(error) => format!("{error:#}"),
                    };
                    refresh.request();
                } else {
                    model.message = "This terminal has closed. Refreshing…".into();
                    refresh.request();
                }
            }
            action => {
                if model.busy.is_some() {
                    model.message =
                        "The previous action is still opening. You can keep browsing.".into();
                    continue;
                }
                let path = model.path();
                let state = state.clone();
                let jobs = jobs.clone();
                model.busy = Some(
                    match action {
                        Action::Browser => "Opening browser…",
                        Action::Close(_) => "Closing terminal…",
                        _ => "Opening terminal…",
                    }
                    .into(),
                );
                std::thread::spawn(move || {
                    let _ = jobs.send(perform(&state, action, path));
                });
            }
        }
    }
    screen.suspend();
    Ok(())
}

fn perform(state: &AppState, action: Action, path: PathBuf) -> Result<JobResult, String> {
    let request = match action {
        Action::AtPath { path, action } => return perform(state, *action, path),
        Action::Browser => {
            open_browser(&state.security.launch_url()).map_err(|error| error.to_string())?;
            return Ok(JobResult::Message(
                "Browser opened. It shares this HQ's terminals.".into(),
            ));
        }
        Action::Agent {
            id,
            action,
            text,
            request_id,
        } => {
            state
                .agents
                .action(&id, &action, text.as_deref(), request_id.as_deref())
                .map_err(|error| error.to_string())?;
            return Ok(JobResult::Message(format!("Agent action sent: {action}")));
        }
        Action::Acknowledge(id) => {
            state
                .agents
                .acknowledge(&id)
                .map_err(|error| error.to_string())?;
            return Ok(JobResult::Message("Result marked reviewed.".into()));
        }
        Action::Integration { provider, action } => {
            state
                .agents
                .integration_action(&provider, &action)
                .map_err(|error| error.to_string())?;
            return Ok(JobResult::Message(format!("{provider}: {action} complete")));
        }
        Action::Close(id) => {
            state.terminals.close(&id);
            return Ok(JobResult::Message("Terminal closed.".into()));
        }
        Action::Shell => CreateTerminal {
            path,
            args: None,
            tmux: None,
            tmux_pane: None,
            new_tmux: false,
        },
        Action::Command(args) => CreateTerminal {
            path,
            args: Some(args),
            tmux: None,
            tmux_pane: None,
            new_tmux: false,
        },
        Action::NewTmux => CreateTerminal {
            path,
            args: None,
            tmux: None,
            tmux_pane: None,
            new_tmux: true,
        },
        Action::AttachTmux { name, path, pane } => CreateTerminal {
            path: path.into(),
            args: None,
            tmux: Some(name),
            tmux_pane: pane,
            new_tmux: false,
        },
        _ => return Err("This action is unavailable.".into()),
    };
    spawn_terminal(state, request)
        .map(|session| JobResult::Attach(session.id))
        .map_err(|error| error.message)
}

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(49, 70, 72)))
        .title(title)
        .title_style(Style::default().fg(GREEN).add_modifier(Modifier::BOLD))
}

fn draw(frame: &mut Frame, model: &mut Model, _address: &str) {
    let area = frame.area();
    frame.render_widget(
        Block::new().style(Style::default().bg(SURFACE).fg(INK)),
        area,
    );
    model.small = area.width < 48 || area.height < 14;
    if model.small {
        let text = if matches!(model.mode, Mode::ConfirmQuit) {
            "Quit HQ? Owned terminals will stop.\nTmux sessions remain.\nEnter quit · Esc cancel"
        } else {
            "Bonsai HQ\nEnlarge to 48 × 14 to navigate.\nb browser · q quit"
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(GREEN))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let active = model
        .workspace
        .agents
        .iter()
        .filter(|agent| agent.live && agent.state == "running")
        .count();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " bonsai HQ ",
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} needs you", model.workspace.attention.len()),
                Style::default().fg(if model.workspace.attention.is_empty() {
                    MUTED
                } else {
                    AMBER
                }),
            ),
            Span::raw(format!(
                " · {active} working · {} worktrees",
                model.rows.len()
            )),
        ])),
        layout[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" 1 Home ", tab_style(model.view == View::Worktrees)),
            Span::styled(" 2 Terminals ", tab_style(model.view == View::Sessions)),
            Span::styled(
                " 3 Integrations ",
                tab_style(model.view == View::Integrations),
            ),
            Span::styled(
                if model.mode == Mode::Search || !model.query.is_empty() {
                    format!(" / {}", model.query)
                } else {
                    " / search".into()
                },
                Style::default().fg(GREEN),
            ),
        ])),
        layout[1],
    );
    let quota = if model.workspace.quotas.is_empty() {
        " Quotas unavailable · 3 integrations for status".into()
    } else {
        format!(
            " {}",
            model
                .workspace
                .quotas
                .iter()
                .map(|quota| format!("{} {} {}", quota.provider, quota.label, quota_value(quota)))
                .collect::<Vec<_>>()
                .join("  ·  ")
        )
    };
    frame.render_widget(
        Paragraph::new(quota).style(Style::default().fg(MUTED)),
        layout[2],
    );
    match model.view {
        View::Worktrees => draw_worktrees(frame, model, layout[3]),
        View::Sessions => draw_sessions(frame, model, layout[3]),
        View::Integrations => draw_integrations(frame, model, layout[3]),
    }
    let status = if model.mode == Mode::Search {
        format!(" / {}▏  Enter keep · Esc clear", model.query)
    } else {
        format!(" {}", model.busy.as_deref().unwrap_or(&model.message))
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(MUTED)),
        layout[4],
    );
    let help = if area.width >= 88 {
        " ↑↓/jk move  ←→ expand  Enter open  [] attention  Space actions  i details  t shell  / search  q quit"
    } else if area.width >= 64 {
        " ↵ open  ←→ expand  [] attention  Space actions  i details  / find"
    } else {
        " ↵ open  [] next  Space actions  / find  q quit"
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(GREEN)),
        layout[5],
    );
    match &model.mode {
        Mode::Actions { selected, context } => draw_actions(frame, *selected, &context.path),
        Mode::Activity {
            selected,
            path,
            choices,
        } => draw_activity(frame, *selected, path, choices),
        Mode::Details {
            title,
            lines,
            scroll,
        } => {
            let area = popup(area, 100, 26);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(lines.iter().cloned().map(Line::raw).collect::<Vec<_>>())
                    .block(panel(format!(" {title} · ↑↓ scroll · Esc close ")))
                    .style(Style::default().bg(SURFACE))
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                area,
            );
        }
        Mode::Command { line, path } | Mode::Reply { line, path, .. } => {
            let replying = matches!(model.mode, Mode::Reply { .. });
            let popup = popup(area, 90, 9);
            frame.render_widget(Clear, popup);
            let title = if replying {
                " Reply to agent · Enter send · Esc cancel "
            } else {
                " Bonsai command · Enter run · Esc cancel "
            };
            let inner = panel(title).inner(popup);
            frame.render_widget(panel(title).style(Style::default().bg(SURFACE)), popup);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(path.display().to_string(), Style::default().fg(MUTED)),
                    Line::styled(
                        format!("{} {line}", if replying { ">" } else { "bonsai" }),
                        Style::default().fg(GREEN),
                    ),
                ])
                .wrap(Wrap { trim: false }),
                inner,
            );
            frame.set_cursor_position((
                inner.x
                    + (if replying { 2 } else { 7 } + line.chars().count() as u16)
                        .min(inner.width.saturating_sub(1)),
                inner.y + 1,
            ));
        }
        Mode::ConfirmAgent { id, action, .. } => draw_confirm(
            frame,
            " Confirm agent action ",
            &format!("{action} · {id}\n\nEnter confirm · Esc cancel"),
        ),
        Mode::ConfirmQuit => draw_confirm(
            frame,
            " Quit Bonsai HQ? ",
            "Owned shells and commands will stop. Tmux sessions remain.\n\nEnter quit · Esc cancel",
        ),
        Mode::ConfirmClose(_) => draw_confirm(
            frame,
            " Close this terminal? ",
            "Its shell or command will stop. A tmux client detaches.\n\nEnter close · Esc cancel",
        ),
        Mode::ConfirmRemove { path, .. } => draw_confirm(
            frame,
            " Remove this worktree? ",
            &format!(
                "{}\nBonsai checks uncommitted changes; the branch is kept.\n\nEnter remove · Esc cancel",
                path.display()
            ),
        ),
        _ => {}
    }
}

fn tab_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(GREEN)
            .bg(SELECTED)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    }
}

fn draw_worktrees(frame: &mut Frame, model: &mut Model, area: Rect) {
    let mut selection = model.worktree_state;
    let items = model
        .home_rows
        .iter()
        .map(|entry| {
            let line = match entry {
                HomeRow::Section(section) => {
                    let count = model
                        .filtered
                        .iter()
                        .filter(|&&index| model.priority(index) == *section)
                        .count();
                    let toggle = if *section == "Older" {
                        if model.older_open { "▾ " } else { "▸ " }
                    } else {
                        ""
                    };
                    Line::styled(
                        format!(
                            " {toggle}{section}{}",
                            if count > 0 {
                                format!("  {count}")
                            } else {
                                String::new()
                            }
                        ),
                        Style::default()
                            .fg(if *section == "Needs you" {
                                AMBER
                            } else {
                                MUTED
                            })
                            .add_modifier(Modifier::BOLD),
                    )
                }
                HomeRow::Tree(index) => {
                    let row = &model.rows[*index];
                    let tree = &row.worktree;
                    let agents = model.tree_agents(*index);
                    let provider_summary = agents
                        .iter()
                        .map(|&index| model.workspace.agents[index].provider.as_str())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join("/");
                    let runtime = if agents.is_empty() {
                        let terminals = tree
                            .activity
                            .terminals
                            .iter()
                            .filter(|terminal| !terminal.exited)
                            .count();
                        let panes = tree.activity.tmux.len();
                        if terminals + panes == 0 {
                            String::new()
                        } else {
                            format!(" · {terminals} HQ · {panes} tmux")
                        }
                    } else {
                        format!(" · {} {}", agents.len(), provider_summary)
                    };
                    let mut line = Line::from(vec![
                        Span::styled(
                            format!(
                                " {} ",
                                if model.expanded.contains(&tree.path) || !model.query.is_empty() {
                                    "▾"
                                } else {
                                    "▸"
                                }
                            ),
                            Style::default().fg(MUTED),
                        ),
                        Span::styled(format!("{} / ", row.project), Style::default().fg(MUTED)),
                        Span::raw(tree.branch.as_deref().unwrap_or("detached")),
                        Span::styled(
                            if tree.prunable {
                                " × unavailable"
                            } else if tree.dirty == Some(true) {
                                " *"
                            } else {
                                ""
                            },
                            Style::default().fg(AMBER),
                        ),
                        Span::styled(runtime, Style::default().fg(GREEN)),
                        Span::styled(
                            format!(" · {}", age(tree.last_activity)),
                            Style::default().fg(MUTED),
                        ),
                    ]);
                    let remaining = usize::from(area.width).saturating_sub(line.width() + 1);
                    if remaining >= 8
                        && let Some(agent) = agents
                            .iter()
                            .map(|&index| &model.workspace.agents[index])
                            .filter(|agent| !agent.title.trim().is_empty())
                            .max_by_key(|agent| {
                                (
                                    model
                                        .workspace
                                        .attention
                                        .iter()
                                        .any(|item| item.agent_id == agent.id),
                                    agent.live,
                                    agent.updated_at,
                                )
                            })
                    {
                        let title = compact_title(&agent.title, remaining - 3);
                        line.spans.push(Span::styled(
                            format!(" · {title}"),
                            Style::default().fg(MUTED),
                        ));
                    }
                    line
                }
                HomeRow::Agent(index, depth) => {
                    let agent = &model.workspace.agents[*index];
                    let attention = model
                        .workspace
                        .attention
                        .iter()
                        .any(|item| item.agent_id == agent.id);
                    let state = if agent.stale { "stale" } else { &agent.state };
                    let color = if attention || agent.state == "waiting" {
                        AMBER
                    } else if agent.state == "failed" {
                        RED
                    } else if agent.state == "running" {
                        GREEN
                    } else {
                        MUTED
                    };
                    let target = agent
                        .target
                        .as_ref()
                        .and_then(|target| target.tmux_pane.as_deref())
                        .unwrap_or("");
                    let children = model
                        .workspace
                        .agents
                        .iter()
                        .any(|child| child.parent_id.as_deref() == Some(&agent.id));
                    Line::from(vec![
                        Span::styled(
                            format!(
                                "{}{} ",
                                "  ".repeat((*depth).min(5)),
                                if children {
                                    if model.collapsed_agents.contains(&agent.id) {
                                        "▸"
                                    } else {
                                        "▾"
                                    }
                                } else {
                                    "·"
                                }
                            ),
                            Style::default().fg(MUTED),
                        ),
                        Span::styled(
                            format!("{} {state}", agent.provider),
                            Style::default().fg(color),
                        ),
                        Span::raw(format!(" · {}", agent.title)),
                        Span::styled(
                            format!(
                                " {}{}",
                                agent.model.as_deref().unwrap_or(""),
                                if target.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {target}")
                                }
                            ),
                            Style::default().fg(MUTED),
                        ),
                    ])
                }
            };
            ListItem::new(line)
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        let text = if !model.loaded {
            " Discovering worktrees…"
        } else if !model.query.is_empty() {
            " No matches. Esc clears search."
        } else {
            " No worktrees yet. t opens a shell; a adds a worktree."
        };
        frame.render_widget(Paragraph::new(text).style(Style::default().fg(MUTED)), area);
    } else {
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().bg(SELECTED))
                .highlight_symbol("›"),
            area,
            &mut selection,
        );
    }
    model.worktree_state = selection;
}

fn draw_sessions(frame: &mut Frame, model: &mut Model, area: Rect) {
    let items = model
        .session_filtered
        .iter()
        .map(|&index| {
            if let Some(session) = model.workspace.terminals.get(index) {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {} ", if session.exited { "○" } else { "●" }),
                        Style::default().fg(if session.exited { MUTED } else { GREEN }),
                    ),
                    Span::raw(&session.title),
                    Span::styled(
                        format!(
                            " · {}{} · {}",
                            session.kind,
                            if session.exited {
                                format!(
                                    " exit {}",
                                    session
                                        .exit_code
                                        .map_or_else(|| "unknown".into(), |code| code.to_string())
                                )
                            } else {
                                String::new()
                            },
                            session.path
                        ),
                        Style::default().fg(MUTED),
                    ),
                ]))
            } else {
                let session =
                    &model.workspace.tmux.sessions[index - model.workspace.terminals.len()];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" ◇ tmux {}", session.name),
                        Style::default().fg(AMBER),
                    ),
                    Span::styled(
                        format!(
                            " · {} windows · {} · {}",
                            session.windows,
                            if session.attached {
                                "attached"
                            } else {
                                "detached"
                            },
                            session.path
                        ),
                        Style::default().fg(MUTED),
                    ),
                ]))
            }
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        let text = if model.workspace.tmux.available {
            " No terminals. t opens a shell; m opens a persistent tmux shell."
        } else {
            " No terminals. t opens a shell; install tmux for persistent sessions."
        };
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), area);
    } else {
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().bg(SELECTED))
                .highlight_symbol("›"),
            area,
            &mut model.session_state,
        );
    }
}

fn draw_integrations(frame: &mut Frame, model: &mut Model, area: Rect) {
    let items = model
        .workspace
        .integrations
        .iter()
        .map(|integration| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} · {}", integration.provider, integration.status),
                    Style::default().fg(if integration.status == "connected" {
                        GREEN
                    } else {
                        AMBER
                    }),
                ),
                Span::styled(
                    format!(" · {}", integration.message),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(" No integration status available. R refreshes; i shows quota details.")
                .wrap(Wrap { trim: false }),
            area,
        );
    } else {
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().bg(SELECTED))
                .highlight_symbol("›"),
            area,
            &mut model.integration_state,
        );
    }
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_actions(frame: &mut Frame, selected: usize, path: &std::path::Path) {
    let area = popup(frame.area(), 96, ACTIONS.len() as u16 + 2);
    frame.render_widget(Clear, area);
    let items = ACTIONS.iter().map(|(name, description)| {
        ListItem::new(Line::from(vec![
            Span::raw(*name),
            Span::styled(format!(" · {description}"), Style::default().fg(MUTED)),
        ]))
    });
    frame.render_stateful_widget(
        List::new(items)
            .block(
                panel(format!(" Actions · {} ", path.display()))
                    .style(Style::default().bg(SURFACE)),
            )
            .highlight_style(Style::default().bg(SELECTED).fg(GREEN))
            .highlight_symbol("› "),
        area,
        &mut ListState::default().with_selected(Some(selected)),
    );
}

fn draw_activity(
    frame: &mut Frame,
    selected: usize,
    path: &std::path::Path,
    choices: &[ActivityChoice],
) {
    let height = choices.len().saturating_add(2).min(24) as u16;
    let area = popup(frame.area(), 82, height);
    frame.render_widget(Clear, area);
    let items = choices.iter().map(|choice| {
        ListItem::new(Line::from(vec![
            Span::raw(choice.title.as_str()),
            Span::styled(format!(" · {}", choice.detail), Style::default().fg(MUTED)),
        ]))
    });
    frame.render_stateful_widget(
        List::new(items)
            .block(
                panel(format!(" Activity · {} ", path.display()))
                    .style(Style::default().bg(SURFACE)),
            )
            .highlight_style(Style::default().bg(SELECTED).fg(GREEN))
            .highlight_symbol("› "),
        area,
        &mut ListState::default().with_selected(Some(selected)),
    );
}

fn draw_confirm(frame: &mut Frame, title: &'static str, text: &str) {
    let area = popup(frame.area(), 70, 8);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .block(panel(title))
            .style(Style::default().bg(SURFACE).fg(AMBER))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::Path;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn data() -> Value {
        json!({
            "root": "/workspaces",
            "projects": [{"name":"bonsai","worktrees":[
                {"path":"/workspaces/main","branch":"main","main":true,"dirty":false,"priority":"recent","lastActivity":100},
                {"path":"/workspaces/fix","branch":"ab/fix","dirty":true,"priority":"recent","lastActivity":90},
                {"path":"/workspaces/old","branch":"ab/old","dirty":false,"priority":"older","lastActivity":1}
            ]}],
            "terminals":[{"id":"shared","title":"Shared shell","path":"/workspaces/fix","kind":"shell","exited":false,"exitCode":null}],
            "tmux":{"available":true,"sessions":[{"name":"work","path":"/workspaces/old","windows":11,"attached":true}]},
            "agents":[], "attention":[], "quotas":[],
            "integrations":[{"provider":"codex","status":"connected","message":"Ready","capabilities":["reply"]}]
        })
    }

    fn workspace() -> Workspace {
        serde_json::from_value(data()).unwrap()
    }

    fn with_agents() -> Workspace {
        let mut value = data();
        value["projects"][0]["worktrees"][0]["priority"] = json!("needs-you");
        value["agents"] = json!([
            {"id":"codex:one","provider":"codex","sessionId":"one","parentId":null,"worktreePath":"/workspaces/main","cwd":"/workspaces/main/src","title":"Fix parser","model":"gpt-test","state":"waiting","waitingReason":"Approve edit","updatedAt":100,"observedAt":100,"live":true,"stale":false,"target":{"tmuxSession":"work","tmuxPane":"%3"},"capabilities":["attach","reply","approve","interrupt"]},
            {"id":"codex:child","provider":"codex","sessionId":"child","parentId":"codex:one","worktreePath":"/workspaces/main","cwd":"/workspaces/main","title":"Inspect grammar","model":null,"state":"running","waitingReason":null,"updatedAt":99,"observedAt":100,"live":true,"target":{"tmuxSession":"work","tmuxPane":"%4"},"capabilities":["attach"]}
        ]);
        value["attention"] = json!([{ "id":"notice-one","agentId":"codex:one","kind":"approval","summary":"Approve parser edit","createdAt":100,"requestId":"provider-request-7" }]);
        serde_json::from_value(value).unwrap()
    }

    fn rendered_at(model: &mut Model, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, model, "http://127.0.0.1:4837"))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn home_collapses_older_but_search_finds_every_worktree() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        assert_eq!(model.path(), PathBuf::from("/workspaces/main"));
        let screen = rendered_at(&mut model, 80, 24);
        assert!(screen.contains("Older  1"));
        assert!(!screen.contains("ab/old"));
        assert_eq!(model.key(press(KeyCode::Enter)), None);
        assert!(model.expanded.contains(&PathBuf::from("/workspaces/main")));
        model.key(press(KeyCode::Char('/')));
        for ch in "abold".chars() {
            model.key(press(KeyCode::Char(ch)));
        }
        assert_eq!(model.path(), PathBuf::from("/workspaces/old"));
        assert!(rendered_at(&mut model, 48, 14).contains("ab/old"));
        model.key(press(KeyCode::Esc));
        model.key(press(KeyCode::End));
        assert_eq!(model.selected_home(), Some(&HomeRow::Section("Older")));
        model.key(press(KeyCode::Right));
        assert!(model.older_open);
        model.key(press(KeyCode::Down));
        assert_eq!(model.path(), PathBuf::from("/workspaces/old"));
    }

    #[test]
    fn eleven_worktrees_and_twenty_two_terminals_fit_without_scrolling() {
        let mut value = data();
        value["projects"][0]["worktrees"] = json!((0..11).map(|index| json!({"path":format!("/workspaces/tree-{index:02}"),"branch":format!("ab/tree-{index:02}"),"dirty":false,"priority":"working"})).collect::<Vec<_>>());
        value["terminals"] = json!((0..22).map(|index| json!({"id":format!("terminal-{index:02}"),"title":format!("Agent terminal {index:02}"),"path":"/workspaces/main","kind":"command","exited":false})).collect::<Vec<_>>());
        let mut model = Model::new("/workspaces".into());
        model.apply(serde_json::from_value(value).unwrap());
        let screen = rendered_at(&mut model, 80, 24);
        for index in 0..11 {
            assert!(screen.contains(&format!("ab/tree-{index:02}")), "{screen}");
        }
        model.view = View::Sessions;
        let screen = rendered_at(&mut model, 120, 36);
        for index in 0..22 {
            assert!(
                screen.contains(&format!("Agent terminal {index:02}")),
                "{screen}"
            );
        }
        assert_eq!(model.session_state.offset(), 0);
    }

    #[test]
    fn collapsed_worktrees_show_the_most_relevant_task_on_one_row() {
        let mut workspace = with_agents();
        workspace.agents[1].updated_at = Some(200);
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace);
        let screen = rendered_at(&mut model, 120, 36);
        assert!(screen.contains("Fix parser"));
        assert!(!screen.contains("Inspect grammar"));
        assert!(model.expanded.is_empty());
        model.workspace.attention.clear();
        assert!(rendered_at(&mut model, 120, 36).contains("Inspect grammar"));
        for agent in &mut model.workspace.agents {
            agent.live = false;
        }
        model.workspace.agents[0].updated_at = Some(300);
        assert!(rendered_at(&mut model, 120, 36).contains("Fix parser"));
        model.workspace.agents[0].title = "A very long task title ".repeat(20);
        let screen = rendered_at(&mut model, 120, 36);
        assert!(screen.contains("A very long task title"));
        assert!(screen.contains('…'));
        assert!(screen.contains("bonsai / main"));
    }

    #[test]
    fn attention_navigation_reveals_agent_and_attaches_exact_pane() {
        let mut model = Model::new("/workspaces".into());
        model.apply(with_agents());
        model.key(press(KeyCode::Char(']')));
        assert_eq!(model.agent().unwrap().id, "codex:one");
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::AttachTmux {
                name: "work".into(),
                path: "/workspaces/main".into(),
                pane: Some("%3".into())
            })
        );
        let screen = rendered_at(&mut model, 120, 36);
        assert!(screen.contains("codex waiting · Fix parser"));
        assert!(screen.contains("Inspect grammar"));
        model.key(press(KeyCode::Left));
        assert!(!rendered_at(&mut model, 120, 36).contains("Inspect grammar"));
        model.key(press(KeyCode::Right));
        model.key(press(KeyCode::Down));
        assert_eq!(model.agent().unwrap().id, "codex:child");
        model.key(press(KeyCode::Left));
        assert_eq!(model.agent().unwrap().id, "codex:one");
    }

    #[test]
    fn pending_requests_cannot_be_dismissed_from_agent_actions() {
        for kind in ["question", "approval", "error", "completed"] {
            let mut workspace = with_agents();
            workspace.attention[0].kind = kind.into();
            let mut model = Model::new("/workspaces".into());
            model.apply(workspace);
            model.key(press(KeyCode::Char(']')));
            model.key(press(KeyCode::Char(' ')));
            let Mode::Activity { choices, .. } = &model.mode else {
                panic!("expected agent actions");
            };
            assert_eq!(
                choices
                    .iter()
                    .any(|choice| matches!(choice.action, Action::Acknowledge(_))),
                kind == "completed"
            );
        }
    }

    #[test]
    fn agent_action_dialog_captures_provider_request_across_refresh() {
        let mut model = Model::new("/workspaces".into());
        model.apply(with_agents());
        model.key(press(KeyCode::Char(']')));
        model.key(press(KeyCode::Char(' ')));
        assert!(
            matches!(&model.mode, Mode::Activity { choices, .. } if choices.iter().any(|choice| matches!(&choice.action, Action::Agent { action, request_id, .. } if action == "approve" && request_id.as_deref() == Some("provider-request-7"))))
        );
        let index = match &model.mode {
            Mode::Activity { choices, .. } => choices
                .iter()
                .position(|choice| choice.title == "reply")
                .unwrap(),
            _ => unreachable!(),
        };
        for _ in 0..index {
            model.key(press(KeyCode::Down));
        }
        model.apply(workspace());
        model.key(press(KeyCode::Enter));
        assert!(
            matches!(&model.mode, Mode::Reply { agent_id, path, .. } if agent_id == "codex:one" && path == Path::new("/workspaces/main"))
        );
        for ch in "continue".chars() {
            model.key(press(KeyCode::Char(ch)));
        }
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Agent {
                id: "codex:one".into(),
                action: "reply".into(),
                text: Some("continue".into()),
                request_id: Some("provider-request-7".into())
            })
        );
    }

    #[test]
    fn session_view_actions_use_selected_terminal_directory() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        model.view = View::Sessions;
        assert_eq!(model.path(), PathBuf::from("/workspaces/fix"));
        model.key(press(KeyCode::Char(':')));
        assert!(
            matches!(&model.mode, Mode::Command { path, .. } if path == Path::new("/workspaces/fix"))
        );
        model.key(press(KeyCode::Esc));
        model.key(press(KeyCode::Down));
        assert_eq!(model.path(), PathBuf::from("/workspaces/old"));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::AttachTmux {
                name: "work".into(),
                path: "/workspaces/old".into(),
                pane: None
            })
        );
        assert_eq!(model.key(press(KeyCode::Char('t'))), Some(Action::Shell));
    }

    #[test]
    fn session_subdirectories_use_their_worktree_and_unrelated_sessions_cannot_use_another_tree() {
        let mut workspace = workspace();
        workspace.terminals[0].path = "/workspaces/fix/src".into();
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace);
        model.view = View::Sessions;
        assert_eq!(model.path(), PathBuf::from("/workspaces/fix"));
        model.key(press(KeyCode::Char('i')));
        assert!(rendered_at(&mut model, 80, 24).contains("/workspaces/fix/src"));
        model.key(press(KeyCode::Esc));
        model.workspace.terminals[0].path = "/unrelated".into();
        assert_eq!(model.path(), PathBuf::from("/unrelated"));
        assert_eq!(model.key(press(KeyCode::Char('s'))), None);
        assert!(model.message.contains("no known worktree"));
        assert_eq!(
            model.key(press(KeyCode::Char('t'))),
            Some(at_path(Action::Shell, "/workspaces".into()))
        );
    }

    #[test]
    fn historical_agents_resume_the_exact_provider_session_without_a_shell_fallback() {
        let mut workspace = with_agents();
        workspace.agents[0].live = false;
        workspace.agents[0].target = None;
        workspace.agents[0].capabilities = vec!["resume".into()];
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace);
        model.key(press(KeyCode::Char(']')));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(at_path(
                Action::Command(vec![
                    "resume".into(),
                    "--provider".into(),
                    "codex".into(),
                    "--session".into(),
                    "one".into()
                ]),
                "/workspaces/main".into()
            ))
        );
        model.workspace.agents[0].capabilities.clear();
        assert_eq!(model.key(press(KeyCode::Enter)), None);
    }

    #[test]
    fn refresh_retains_agent_and_inactive_terminal_selection() {
        let mut model = Model::new("/workspaces".into());
        model.apply(with_agents());
        model.key(press(KeyCode::Char(']')));
        let selected = model.selection_id();
        let mut updated = with_agents();
        updated.projects[0].worktrees.reverse();
        updated.agents.reverse();
        updated.terminals.insert(
            0,
            Session {
                id: "new".into(),
                title: "New shell".into(),
                path: "/workspaces".into(),
                kind: "shell".into(),
                exited: false,
                exit_code: None,
            },
        );
        model.apply(updated);
        assert_eq!(model.selection_id(), selected);
        model.view = View::Sessions;
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Attach("shared".into()))
        );
    }

    #[test]
    fn command_and_removal_dialogs_never_retarget_after_refresh() {
        for opener in [':', 'd', ' '] {
            let mut model = Model::new("/workspaces".into());
            model.apply(workspace());
            model.key(press(KeyCode::Down));
            let path = model.path();
            model.key(press(KeyCode::Char(opener)));
            if opener == ':' {
                for ch in "clean".chars() {
                    model.key(press(KeyCode::Char(ch)));
                }
            }
            model.apply(Workspace {
                root: "/workspaces".into(),
                ..Workspace::default()
            });
            let action = match opener {
                ':' => command("clean"),
                'd' => Action::Command(vec![
                    "remove".into(),
                    "--".into(),
                    path.display().to_string(),
                ]),
                _ => Action::Shell,
            };
            assert_eq!(
                model.key(press(KeyCode::Enter)),
                Some(at_path(action, path))
            );
        }
    }

    #[test]
    fn activity_picker_targets_each_pane_individually() {
        let mut value = data();
        value["projects"][0]["worktrees"][0]["activity"] = json!({"terminals":[],"tmux":[
            {"session":"work","window":"@2","pane":"%3","command":"codex","active":true},
            {"session":"work","window":"@2","pane":"%4","command":"claude","active":false}
        ]});
        let mut model = Model::new("/workspaces".into());
        model.apply(serde_json::from_value(value).unwrap());
        model.key(press(KeyCode::Char('e')));
        model.key(press(KeyCode::Down));
        model.apply(workspace());
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::AttachTmux {
                name: "work".into(),
                path: "/workspaces/main".into(),
                pane: Some("%4".into())
            })
        );
    }

    #[test]
    fn unavailable_quota_and_stale_agent_remain_explicit() {
        let mut workspace = with_agents();
        workspace.agents[0].stale = true;
        workspace.quotas = serde_json::from_value(json!([{"id":"codex-weekly","provider":"codex","label":"weekly","usedPercent":null,"resetsAt":null,"observedAt":100,"stale":true,"unavailableReason":"No recent provider sample"}])).unwrap();
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace);
        model.key(press(KeyCode::Char(']')));
        let screen = rendered_at(&mut model, 120, 36);
        assert!(screen.contains("unknown · stale"));
        assert!(screen.contains("codex stale"));
        model.view = View::Integrations;
        model.key(press(KeyCode::Char('i')));
        assert!(rendered_at(&mut model, 120, 36).contains("No recent provider sample"));
    }

    #[test]
    fn minimum_size_remains_usable_and_smaller_sizes_cannot_trigger_hidden_actions() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        assert!(rendered_at(&mut model, 48, 14).contains("main"));
        model.key(press(KeyCode::Char(':')));
        for ch in "clean".chars() {
            model.key(press(KeyCode::Char(ch)));
        }
        rendered_at(&mut model, 30, 8);
        assert_eq!(model.key(press(KeyCode::Enter)), None);
        assert!(matches!(model.mode, Mode::Command { .. }));
        for key in ['t', 's', 'r', 'd', 'm', ' '] {
            assert_eq!(model.key(press(KeyCode::Char(key))), None);
        }
        model.mode = Mode::ConfirmQuit;
        assert!(rendered_at(&mut model, 30, 8).contains("Quit HQ?"));
        assert_eq!(model.key(press(KeyCode::Esc)), None);
        assert_eq!(model.mode, Mode::Browse);
        rendered_at(&mut model, 48, 14);
        assert_eq!(model.key(press(KeyCode::Char('t'))), Some(Action::Shell));
    }

    #[test]
    fn command_parser_preserves_quotes_and_rejects_recursive_hq() {
        assert_eq!(
            parse_command("bonsai add 'ab/fix api' --base HEAD").unwrap(),
            ["add", "ab/fix api", "--base", "HEAD"]
        );
        assert!(parse_command("hq").is_err());
        assert!(parse_command("add 'unfinished").is_err());
        assert!(parse_command("list; rm -rf /tmp/example").is_err());
    }

    #[test]
    fn quit_close_and_removal_have_cancelable_states() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        assert_eq!(
            model.key(press(KeyCode::Char('q'))),
            Some(Action::RequestQuit)
        );
        model.mode = Mode::ConfirmQuit;
        model.key(press(KeyCode::Esc));
        assert_eq!(model.mode, Mode::Browse);
        model.view = View::Sessions;
        model.key(press(KeyCode::Char('x')));
        assert_eq!(model.mode, Mode::ConfirmClose("shared".into()));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Close("shared".into()))
        );
        model.view = View::Worktrees;
        model.key(press(KeyCode::Char('d')));
        assert_eq!(model.mode, Mode::Browse);
    }
}
