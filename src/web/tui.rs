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
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
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
}

#[derive(Clone, Default, Deserialize)]
struct WorktreeActivity {
    #[serde(default)]
    terminals: Vec<ActivityTerminal>,
    #[serde(default)]
    tmux: Vec<ActivityPane>,
}

impl WorktreeActivity {
    fn live_terminals(&self) -> usize {
        self.terminals
            .iter()
            .filter(|terminal| !terminal.exited)
            .count()
    }

    fn badge(&self) -> String {
        let terminals = self.live_terminals();
        let panes = self.tmux.len();
        if terminals == 0 && panes == 0 {
            String::new()
        } else {
            format!("  {terminals} HQ · {panes} tmux")
        }
    }
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

struct TreeRow {
    project: String,
    worktree: Worktree,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum View {
    #[default]
    Worktrees,
    Sessions,
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
    AttachTmux { name: String, path: String },
    Close(String),
    Browser,
    Refresh,
    RequestQuit,
    Quit,
    AtPath { path: PathBuf, action: Box<Action> },
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
    session_filtered: Vec<usize>,
    query: String,
    view: View,
    mode: Mode,
    worktree_state: ListState,
    session_state: ListState,
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
            session_filtered: Vec::new(),
            query: String::new(),
            view: View::Worktrees,
            mode: Mode::Browse,
            worktree_state: ListState::default(),
            session_state: ListState::default(),
            message: "Discovering your workspace…".into(),
            busy: None,
            loaded: false,
        }
    }

    fn apply(&mut self, workspace: Workspace) {
        let selected = self.path();
        let selected_session = self
            .session_filtered
            .get(self.session_state.selected().unwrap_or(0))
            .and_then(|&index| self.session_action(index));
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
        self.refilter();
        if let Some(index) = self
            .filtered
            .iter()
            .position(|&row| self.rows[row].worktree.path == selected)
        {
            self.worktree_state.select(Some(index));
        }
        if let Some(selected) = selected_session
            && let Some(index) = self
                .session_filtered
                .iter()
                .position(|&index| self.session_action(index).as_ref() == Some(&selected))
        {
            self.session_state.select(Some(index));
        }
        if !self.loaded {
            self.message = "Ready. Both interfaces share the same terminals.".into();
        }
        self.loaded = true;
    }

    fn refilter(&mut self) {
        self.filtered = ranked(
            &self.query,
            self.rows.iter().map(|row| {
                format!(
                    "{} {} {}",
                    row.project,
                    row.worktree.branch.as_deref().unwrap_or("detached"),
                    row.worktree.path.display()
                )
            }),
        );
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
        clamp_selection(&mut self.worktree_state, self.filtered.len());
        clamp_selection(&mut self.session_state, self.session_filtered.len());
    }

    fn worktree(&self) -> Option<&TreeRow> {
        self.filtered
            .get(self.worktree_state.selected().unwrap_or(0))
            .map(|&row| &self.rows[row])
    }

    fn path(&self) -> PathBuf {
        self.worktree()
            .map(|row| row.worktree.path.clone())
            .unwrap_or_else(|| self.workspace.root.clone())
    }

    fn move_selection(&mut self, direction: isize) {
        let (selection, len) = match self.view {
            View::Worktrees => (&mut self.worktree_state, self.filtered.len()),
            View::Sessions => (&mut self.session_state, self.session_filtered.len()),
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

    fn activate(&self) -> Option<Action> {
        if self.view == View::Worktrees {
            return Some(Action::Shell);
        }
        let index = *self
            .session_filtered
            .get(self.session_state.selected().unwrap_or(0))?;
        self.session_action(index)
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
            })
        }
    }

    fn request_remove(&mut self) -> Option<Action> {
        self.request_remove_at(self.action_context())
    }

    fn action_context(&self) -> ActionContext {
        ActionContext {
            path: self.path(),
            removable: self
                .worktree()
                .is_some_and(|row| !row.worktree.main && !row.worktree.external),
            activity: self.worktree().map(activity_choices).unwrap_or_default(),
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
        let mode = std::mem::take(&mut self.mode);
        match mode {
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
                        return Some(choices.remove(selected).action);
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
                        View::Sessions => View::Worktrees,
                    }
                }
                KeyCode::Char('1') => self.view = View::Worktrees,
                KeyCode::Char('2') => self.view = View::Sessions,
                KeyCode::Char('/') => self.mode = Mode::Search,
                KeyCode::Char(':') => {
                    self.mode = Mode::Command {
                        line: String::new(),
                        path: self.path(),
                    }
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
                KeyCode::Char('t') => return Some(Action::Shell),
                KeyCode::Char('s') => return Some(command("start")),
                KeyCode::Char('r') => return Some(command("resume")),
                KeyCode::Char('a') => return Some(command("add")),
                KeyCode::Char('c') => return Some(command("clean")),
                KeyCode::Char('p') => return Some(command("prune")),
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
    let mut sessions = BTreeSet::new();
    for pane in &row.worktree.activity.tmux {
        if sessions.insert(&pane.session) {
            let panes = row
                .worktree
                .activity
                .tmux
                .iter()
                .filter(|other| other.session == pane.session)
                .count();
            choices.push(ActivityChoice {
                title: format!("tmux · {}", pane.session),
                detail: format!(
                    "{panes} {} in this worktree · persistent session",
                    if panes == 1 { "pane" } else { "panes" }
                ),
                action: Action::AttachTmux {
                    name: pane.session.clone(),
                    path: row.worktree.path.display().to_string(),
                },
            });
        }
    }
    choices
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
                    receive.recv_timeout(Duration::from_secs(2)),
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
                    Mode::Command { line, .. } => line.push_str(&text.replace(['\r', '\n'], " ")),
                    Mode::Search => {
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
        Action::Close(id) => {
            state.terminals.close(&id);
            return Ok(JobResult::Message("Terminal closed.".into()));
        }
        Action::Shell => CreateTerminal {
            path,
            args: None,
            tmux: None,
            new_tmux: false,
        },
        Action::Command(args) => CreateTerminal {
            path,
            args: Some(args),
            tmux: None,
            new_tmux: false,
        },
        Action::NewTmux => CreateTerminal {
            path,
            args: None,
            tmux: None,
            new_tmux: true,
        },
        Action::AttachTmux { name, path } => CreateTerminal {
            path: path.into(),
            args: None,
            tmux: Some(name),
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

fn draw(frame: &mut Frame, model: &mut Model, address: &str) {
    let area = frame.area();
    frame.render_widget(
        Block::new().style(Style::default().bg(SURFACE).fg(INK)),
        area,
    );
    if area.width < 48 || area.height < 14 {
        frame.render_widget(
            Paragraph::new(
                "Bonsai HQ\n\nEnlarge this terminal to at least 48 × 14.\n\n[b] browser  [q] quit",
            )
            .style(Style::default().fg(GREEN))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .horizontal_margin(1)
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " bonsai ",
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled("HQ", Style::default().fg(INK).add_modifier(Modifier::BOLD)),
            Span::styled("   your workspace, connected", Style::default().fg(MUTED)),
        ]))
        .block(
            Block::new()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(MUTED)),
        ),
        layout[0],
    );
    let tabs = Line::from(vec![
        Span::styled(" 1  Worktrees ", tab_style(model.view == View::Worktrees)),
        Span::raw("  "),
        Span::styled(
            format!(" 2  Terminals ({}) ", model.workspace.terminals.len()),
            tab_style(model.view == View::Sessions),
        ),
        Span::styled(
            format!(
                "    {} projects · {} worktrees",
                model.workspace.projects.len(),
                model.rows.len()
            ),
            Style::default().fg(MUTED),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![
            tabs,
            Line::styled(
                model.busy.as_deref().unwrap_or(&model.message),
                Style::default().fg(MUTED),
            ),
        ]),
        layout[1],
    );
    let columns = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(layout[2]);
    match model.view {
        View::Worktrees => draw_worktrees(frame, model, &columns),
        View::Sessions => draw_sessions(frame, model, &columns),
    }
    let query = if model.query.is_empty() && model.mode != Mode::Search {
        "Search projects, branches, paths, and terminals…"
    } else {
        &model.query
    };
    let searching = model.mode == Mode::Search;
    frame.render_widget(
        Paragraph::new(format!("/ {query}"))
            .style(Style::default().fg(if searching { GREEN } else { MUTED }))
            .block(panel(if searching {
                " Fuzzy search · Enter keep · Esc clear "
            } else {
                " / search · Tab switch view "
            })),
        layout[3],
    );
    if searching {
        frame.set_cursor_position((
            layout[3].x
                + 3
                + (model.query.chars().count() as u16).min(layout[3].width.saturating_sub(5)),
            layout[3].y + 1,
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " ↵ attach  e activity  Space actions  : command  b browser  q quit",
                Style::default().fg(MUTED),
            ),
            Span::styled(format!("   {address}"), Style::default().fg(GREEN)),
        ])),
        layout[4],
    );
    match &model.mode {
        Mode::Actions { selected, context } => draw_actions(frame, *selected, &context.path),
        Mode::Activity {
            selected,
            path,
            choices,
        } => {
            draw_activity(frame, *selected, path, choices);
        }
        Mode::Command { line, path } => {
            let popup = popup(area, 76, 9);
            frame.render_widget(Clear, popup);
            let inner = panel(" Run Bonsai command ").inner(popup);
            frame.render_widget(
                panel(" Run Bonsai command ").style(Style::default().bg(SURFACE)),
                popup,
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(path.display().to_string(), Style::default().fg(MUTED)),
                    Line::raw(""),
                    Line::styled(format!("bonsai {line}"), Style::default().fg(GREEN)),
                    Line::raw(""),
                    Line::styled(
                        "Enter run · Esc cancel · quotes preserve arguments",
                        Style::default().fg(MUTED),
                    ),
                ])
                .wrap(Wrap { trim: false }),
                inner,
            );
            frame.set_cursor_position((
                inner.x + (7 + line.chars().count() as u16).min(inner.width.saturating_sub(1)),
                inner.y + 2,
            ));
        }
        Mode::ConfirmQuit => draw_confirm(
            frame,
            " Quit Bonsai HQ? ",
            "Running shells and commands will stop.\nPersistent tmux sessions remain available.\n\n[q / Enter] quit both interfaces   [Esc] cancel",
        ),
        Mode::ConfirmClose(_) => draw_confirm(
            frame,
            " Close this terminal? ",
            "Its shell or command will stop. A tmux client detaches.\n\n[y / Enter] close   [Esc] cancel",
        ),
        Mode::ConfirmRemove { path, .. } => draw_confirm(
            frame,
            " Remove this worktree? ",
            &format!(
                "{}\n\nBonsai will check for uncommitted changes. The branch is kept.\n\n[y / Enter] remove   [Esc] cancel",
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

fn draw_worktrees(frame: &mut Frame, model: &mut Model, columns: &[Rect]) {
    let mut previous = "";
    let items = model
        .filtered
        .iter()
        .map(|&index| {
            let row = &model.rows[index];
            let mut lines = Vec::new();
            if previous != row.project {
                lines.push(Line::styled(
                    format!("  {}", row.project),
                    Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
                ));
            }
            previous = &row.project;
            let worktree = &row.worktree;
            let mark = if worktree.prunable {
                "×"
            } else if worktree.dirty == Some(true) {
                "●"
            } else {
                "○"
            };
            let kind = if worktree.main {
                "  root"
            } else if worktree.external {
                "  external"
            } else {
                ""
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  └ {mark} "),
                    Style::default().fg(if worktree.dirty == Some(true) {
                        AMBER
                    } else {
                        GREEN
                    }),
                ),
                Span::raw(
                    worktree
                        .branch
                        .clone()
                        .unwrap_or_else(|| "(detached)".into()),
                ),
                Span::styled(kind, Style::default().fg(MUTED)),
                Span::styled(worktree.activity.badge(), Style::default().fg(GREEN)),
            ]));
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(" Worktree constellation "))
            .highlight_style(Style::default().bg(SELECTED).fg(INK))
            .highlight_symbol("›"),
        columns[0],
        &mut model.worktree_state,
    );
    if model.filtered.is_empty() {
        let message = if !model.loaded {
            "Discovering projects…"
        } else if !model.query.is_empty() {
            "No matches. Press Esc to clear the search."
        } else {
            "Your workspace is ready.\n\nPress Enter to open a shell.\nClone a project, then run bonsai add.\n\nPress b to explore in the browser."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: false }),
            panel("").inner(columns[0]),
        );
    }
    let mut lines = Vec::new();
    if let Some(row) = model.worktree() {
        let tree = &row.worktree;
        lines.extend([
            Line::styled(
                row.project.clone(),
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Line::raw(
                tree.branch
                    .clone()
                    .unwrap_or_else(|| "Detached HEAD".into()),
            ),
            Line::raw(""),
            Line::styled(tree.path.display().to_string(), Style::default().fg(MUTED)),
            Line::raw(""),
            Line::styled(
                if tree.dirty == Some(true) {
                    "● Changes in this worktree"
                } else if tree.dirty == Some(false) {
                    "○ Working tree clean"
                } else {
                    "Status unavailable"
                },
                Style::default().fg(if tree.dirty == Some(true) {
                    AMBER
                } else {
                    GREEN
                }),
            ),
            Line::raw(format!(
                "+{} added  ~{} modified",
                tree.added, tree.modified
            )),
            Line::raw(format!(
                "−{} deleted  ?{} untracked",
                tree.deleted, tree.untracked
            )),
            Line::raw(format!("↑{} ahead   ↓{} behind", tree.ahead, tree.behind)),
        ]);
        if tree.locked {
            lines.push(Line::styled("Locked worktree", Style::default().fg(AMBER)));
        }
        if tree.prunable {
            lines.push(Line::styled(
                "Unavailable · registration is prunable",
                Style::default().fg(RED),
            ));
        }
        if tree.external {
            lines.push(Line::styled(
                "External · Git owns this registration",
                Style::default().fg(MUTED),
            ));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "ACTIVITY · e attach",
            Style::default().fg(MUTED),
        ));
        if tree.activity.terminals.is_empty() && tree.activity.tmux.is_empty() {
            lines.push(Line::styled(
                "No terminal activity",
                Style::default().fg(MUTED),
            ));
        } else {
            lines.push(Line::styled(
                format!(
                    "{} HQ terminals · {} tmux panes",
                    tree.activity.live_terminals(),
                    tree.activity.tmux.len()
                ),
                Style::default().fg(GREEN),
            ));
            for terminal in tree.activity.terminals.iter().take(3) {
                lines.push(Line::styled(
                    format!(
                        "{} {}{}",
                        if terminal.exited { "○" } else { "●" },
                        terminal.title,
                        if terminal.exited { " · completed" } else { "" }
                    ),
                    Style::default().fg(if terminal.exited { MUTED } else { INK }),
                ));
            }
            if tree.activity.terminals.len() > 3 {
                lines.push(Line::styled(
                    format!("+{} more HQ terminals", tree.activity.terminals.len() - 3),
                    Style::default().fg(MUTED),
                ));
            }
            for pane in tree.activity.tmux.iter().take(3) {
                lines.push(Line::styled(
                    format!(
                        "{} {} {}/{} · {}",
                        if pane.active { "●" } else { "○" },
                        pane.session,
                        pane.window,
                        pane.pane,
                        pane.command
                    ),
                    Style::default().fg(INK),
                ));
            }
            if tree.activity.tmux.len() > 3 {
                lines.push(Line::styled(
                    format!("+{} more tmux panes", tree.activity.tmux.len() - 3),
                    Style::default().fg(MUTED),
                ));
            }
        }
    } else {
        lines.push(Line::styled(
            model.workspace.root.display().to_string(),
            Style::default().fg(GREEN),
        ));
    }
    lines.extend([
        Line::raw(""),
        Line::styled("WORK HERE", Style::default().fg(MUTED)),
        Line::raw("Enter / t   Shell"),
        Line::raw("s   Start coding   r   Resume"),
        Line::raw("a   Add worktree    m   tmux shell"),
        Line::raw("Space   All actions"),
        Line::raw(""),
        Line::styled(
            model.busy.as_deref().unwrap_or(&model.message),
            Style::default().fg(MUTED),
        ),
    ]);
    if let Some(warning) = model.workspace.warnings.first() {
        lines.push(Line::styled(warning.clone(), Style::default().fg(AMBER)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(" At a glance "))
            .wrap(Wrap { trim: false }),
        columns[1],
    );
}

fn draw_sessions(frame: &mut Frame, model: &mut Model, columns: &[Rect]) {
    let items = model
        .session_filtered
        .iter()
        .map(|&index| {
            if let Some(session) = model.workspace.terminals.get(index) {
                ListItem::new(vec![
                    Line::styled(
                        format!(
                            " {} {}",
                            if session.exited { "○" } else { "●" },
                            session.title
                        ),
                        Style::default().fg(if session.exited { MUTED } else { GREEN }),
                    ),
                    Line::styled(format!("   {}", session.path), Style::default().fg(MUTED)),
                ])
            } else {
                let session =
                    &model.workspace.tmux.sessions[index - model.workspace.terminals.len()];
                ListItem::new(vec![
                    Line::styled(
                        format!(" ◇ tmux · {}", session.name),
                        Style::default().fg(AMBER),
                    ),
                    Line::styled(
                        format!(
                            "   {} windows · {}",
                            session.windows,
                            if session.attached {
                                "attached"
                            } else {
                                "detached"
                            }
                        ),
                        Style::default().fg(MUTED),
                    ),
                ])
            }
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(" Shared terminals & tmux "))
            .highlight_style(Style::default().bg(SELECTED))
            .highlight_symbol("›"),
        columns[0],
        &mut model.session_state,
    );
    let mut lines = vec![
        Line::styled(
            "One session. Both interfaces.",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw(
            "Open a terminal here or in the browser. Attach to it from either interface, with the same running process and output.",
        ),
        Line::raw(""),
        Line::styled("Ctrl-] returns to HQ", Style::default().fg(AMBER)),
        Line::raw("The terminal keeps running when you detach or close a browser tab."),
        Line::raw(""),
        Line::raw("Enter   Attach selected terminal"),
        Line::raw("t       New shell"),
        Line::raw("m       Persistent tmux shell"),
        Line::raw("x       Close selected terminal"),
        Line::raw("Tab     Back to worktrees"),
        Line::raw(""),
    ];
    if let Some(&index) = model
        .session_filtered
        .get(model.session_state.selected().unwrap_or(0))
        && let Some(session) = model.workspace.terminals.get(index)
    {
        lines.push(Line::styled(
            session.title.clone(),
            Style::default().fg(GREEN),
        ));
        lines.push(Line::styled(
            session.path.clone(),
            Style::default().fg(MUTED),
        ));
        if session.exited {
            lines.push(Line::raw(format!(
                "Exited · status {}",
                session
                    .exit_code
                    .map_or_else(|| "unknown".into(), |code| code.to_string())
            )));
        }
    }
    if !model.workspace.tmux.available {
        lines.push(Line::styled(
            "Install tmux for persistent sessions.",
            Style::default().fg(MUTED),
        ));
    }
    lines.push(Line::styled(
        model.busy.as_deref().unwrap_or(&model.message),
        Style::default().fg(MUTED),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(" Terminal desk "))
            .wrap(Wrap { trim: false }),
        columns[1],
    );
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
    let area = popup(frame.area(), 78, 24);
    frame.render_widget(Clear, area);
    let items = ACTIONS.iter().map(|(name, description)| {
        ListItem::new(vec![
            Line::raw(*name),
            Line::styled(*description, Style::default().fg(MUTED)),
        ])
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
    let height = choices.len().saturating_mul(2).saturating_add(2).min(24) as u16;
    let area = popup(frame.area(), 82, height);
    frame.render_widget(Clear, area);
    let items = choices.iter().map(|choice| {
        ListItem::new(vec![
            Line::raw(choice.title.as_str()),
            Line::styled(choice.detail.as_str(), Style::default().fg(MUTED)),
        ])
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

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn workspace() -> Workspace {
        serde_json::from_value(serde_json::json!({
            "root": "/workspaces", "projects": [
                {"name": "bonsai", "worktrees": [{"path":"/workspaces/bonsai/main","branch":"main","main":true,"dirty":false},{"path":"/workspaces/bonsai/ab/fix-api","branch":"ab/fix-api","dirty":true}]},
                {"name":"docs","worktrees":[{"path":"/workspaces/docs/main","branch":"main","dirty":false}]}
            ], "terminals":[{"id":"shared","title":"Shell · bonsai","path":"/workspaces/bonsai/main","kind":"shell","exited":false,"exitCode":null}],
            "tmux":{"available":true,"sessions":[{"name":"work","path":"/workspaces/docs/main","windows":2,"attached":false}]}
        })).unwrap()
    }

    fn workspace_with_activity() -> Workspace {
        serde_json::from_value(serde_json::json!({
            "root": "/workspaces", "projects": [{"name": "bonsai", "worktrees": [
                {"path":"/workspaces/bonsai/main","branch":"main","main":true,"dirty":false,
                 "activity": {
                    "terminals": [
                        {"id":"running","title":"Coding session","kind":"command","exited":false},
                        {"id":"finished","title":"Cleanup output","kind":"command","exited":true}
                    ],
                    "tmux": [
                        {"session":"editor","window":"@2","pane":"%3","command":"nvim","active":true},
                        {"session":"editor","window":"@2","pane":"%4","command":"zsh","active":false}
                    ]
                 }},
                {"path":"/workspaces/bonsai/ab/inactive","branch":"ab/inactive","dirty":false,
                 "activity":{"terminals":[],"tmux":[]}}
            ]}]
        })).unwrap()
    }

    fn rendered(model: &mut Model) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 42)).unwrap();
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
    fn worktree_activity_is_visible_without_hiding_inactive_worktrees() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace_with_activity());
        let screen = rendered(&mut model);
        assert!(screen.contains("1 HQ · 2 tmux"), "{screen}");
        assert!(screen.contains("Coding session"));
        assert!(screen.contains("Cleanup output"));
        assert!(screen.contains("editor @2/%3 · nvim"));
        assert!(screen.contains("ab/inactive"));
        assert_eq!(model.filtered.len(), 2);
        model.key(press(KeyCode::Down));
        assert!(rendered(&mut model).contains("No terminal activity"));
        assert_eq!(model.key(press(KeyCode::Char('e'))), None);
        assert_eq!(model.mode, Mode::Browse);
        assert!(model.message.contains("No terminal activity"));
    }

    #[test]
    fn worktree_activity_picker_retains_its_sessions_across_inventory_refresh() {
        for (selection, expected) in [
            (0, Action::Attach("running".into())),
            (1, Action::Attach("finished".into())),
            (
                2,
                Action::AttachTmux {
                    name: "editor".into(),
                    path: "/workspaces/bonsai/main".into(),
                },
            ),
        ] {
            let mut model = Model::new("/workspaces".into());
            model.apply(workspace_with_activity());
            model.key(press(KeyCode::Char('e')));
            assert!(matches!(&model.mode, Mode::Activity { choices, .. } if choices.len() == 3));
            for _ in 0..selection {
                model.key(press(KeyCode::Down));
            }
            let mut updated = workspace();
            updated.projects.remove(0);
            model.apply(updated);
            assert_eq!(model.key(press(KeyCode::Enter)), Some(expected));
        }
    }

    #[test]
    fn action_menu_captures_worktree_activity_before_a_refresh() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace_with_activity());
        model.key(press(KeyCode::Char(' ')));
        let mut updated = workspace();
        updated.projects.remove(0);
        model.apply(updated);
        for _ in 0..10 {
            model.key(press(KeyCode::Down));
        }
        assert_eq!(model.key(press(KeyCode::Enter)), None);
        let screen = rendered(&mut model);
        assert!(screen.contains("Activity · /workspaces/bonsai/main"));
        assert!(screen.contains("Coding session"));
        assert!(screen.contains("completed output"));
        assert!(screen.contains("2 panes in this worktree"));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Attach("running".into()))
        );
    }

    #[test]
    fn fuzzy_search_matches_project_branch_and_path_and_handles_empty_results() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        model.key(press(KeyCode::Char('/')));
        for ch in "bfxapi".chars() {
            model.key(press(KeyCode::Char(ch)));
        }
        assert_eq!(model.filtered.len(), 1);
        assert_eq!(model.path(), PathBuf::from("/workspaces/bonsai/ab/fix-api"));
        model.key(press(KeyCode::Char('z')));
        assert!(model.filtered.is_empty());
        model.key(press(KeyCode::Esc));
        assert_eq!(model.filtered.len(), 3);
        assert_eq!(model.mode, Mode::Browse);
    }

    #[test]
    fn tabs_attach_the_same_shared_terminal_and_existing_tmux_session() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        model.key(press(KeyCode::Tab));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Attach("shared".into()))
        );
        model.key(press(KeyCode::Down));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::AttachTmux {
                name: "work".into(),
                path: "/workspaces/docs/main".into()
            })
        );
        model.key(press(KeyCode::Tab));
        assert_eq!(model.key(press(KeyCode::Char('s'))), Some(command("start")));
        assert_eq!(
            model.key(press(KeyCode::Char('r'))),
            Some(command("resume"))
        );
    }

    #[test]
    fn refresh_keeps_the_selected_session_when_a_browser_opens_another_terminal() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        model.view = View::Sessions;
        let mut updated = workspace();
        updated.terminals.insert(
            0,
            Session {
                id: "new-browser-terminal".into(),
                title: "New shell".into(),
                path: "/workspaces".into(),
                kind: "shell".into(),
                exited: false,
                exit_code: None,
            },
        );
        model.apply(updated);
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Attach("shared".into()))
        );
    }

    #[test]
    fn refresh_does_not_retarget_open_dialogs_to_another_project() {
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
            let mut next = workspace();
            next.projects.remove(0);
            model.apply(next);
            assert_ne!(model.path(), path);
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
                Some(Action::AtPath {
                    path,
                    action: Box::new(action)
                }),
                "dialog {opener}"
            );
        }
    }

    #[test]
    fn refresh_preserves_selected_path_and_bounds_selection_after_removal() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        model.key(press(KeyCode::Down));
        let selected = model.path();
        let mut next = workspace();
        next.projects.reverse();
        model.apply(next);
        assert_eq!(model.path(), selected);
        model.apply(Workspace {
            root: "/workspaces".into(),
            ..Workspace::default()
        });
        assert_eq!(model.path(), PathBuf::from("/workspaces"));
        assert_eq!(model.key(press(KeyCode::Enter)), Some(Action::Shell));
    }

    #[test]
    fn command_runner_preserves_quotes_and_rejects_recursive_hq_and_shell_syntax() {
        assert_eq!(
            parse_command("bonsai add 'ab/fix api' --base HEAD").unwrap(),
            ["add", "ab/fix api", "--base", "HEAD"]
        );
        assert!(parse_command("hq").is_err());
        assert!(parse_command("add 'unfinished").is_err());
        assert!(parse_command("list; rm -rf /tmp/example").is_err());
        let mut model = Model::new("/workspaces".into());
        model.mode = Mode::Command {
            line: "resume".into(),
            path: "/workspaces".into(),
        };
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(at_path(command("resume"), "/workspaces".into()))
        );
    }

    #[test]
    fn quit_and_close_have_explicit_cancelable_states() {
        let mut model = Model::new("/workspaces".into());
        model.apply(workspace());
        assert_eq!(
            model.key(press(KeyCode::Char('q'))),
            Some(Action::RequestQuit)
        );
        model.mode = Mode::ConfirmQuit;
        assert_eq!(model.key(press(KeyCode::Esc)), None);
        assert_eq!(model.mode, Mode::Browse);
        model.mode = Mode::ConfirmQuit;
        assert_eq!(model.key(press(KeyCode::Char('q'))), Some(Action::Quit));
        model.view = View::Sessions;
        model.key(press(KeyCode::Char('x')));
        assert_eq!(model.mode, Mode::ConfirmClose("shared".into()));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(Action::Close("shared".into()))
        );
    }

    #[test]
    fn removal_confirms_one_managed_worktree_including_detached_heads() {
        let mut model = Model::new("/workspaces".into());
        let mut data = workspace();
        data.projects[0].worktrees[1].branch = None;
        model.apply(data);
        assert_eq!(model.key(press(KeyCode::Char('d'))), None);
        assert_eq!(model.mode, Mode::Browse);
        model.key(press(KeyCode::Down));
        assert_eq!(model.key(press(KeyCode::Char('d'))), None);
        assert!(matches!(model.mode, Mode::ConfirmRemove { .. }));
        assert_eq!(
            model.key(press(KeyCode::Enter)),
            Some(at_path(
                Action::Command(vec![
                    "remove".into(),
                    "--".into(),
                    "/workspaces/bonsai/ab/fix-api".into()
                ]),
                "/workspaces/bonsai/ab/fix-api".into()
            ))
        );
        model.rows[1].worktree.external = true;
        assert_eq!(model.key(press(KeyCode::Char('d'))), None);
        assert_eq!(model.mode, Mode::Browse);
    }

    #[test]
    fn dashboard_renders_on_normal_and_small_terminals() {
        for (width, height) in [(120, 36), (60, 18), (30, 8)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let mut model = Model::new("/workspaces".into());
            model.apply(workspace());
            terminal
                .draw(|frame| draw(frame, &mut model, "http://127.0.0.1:4837"))
                .unwrap();
            model.view = View::Sessions;
            terminal
                .draw(|frame| draw(frame, &mut model, "http://127.0.0.1:4837"))
                .unwrap();
        }
    }
}
