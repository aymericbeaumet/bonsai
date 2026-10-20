mod footprint;
mod inventory;
mod jobs;
mod owner;
mod rows;
mod table;
mod view;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufWriter, IsTerminal, Stderr, Write};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use inventory::{Project, Snapshot, WorktreeEntry};
use rows::{Info, Row};

use crate::config::Config;
use crate::edit::LineInput;
use crate::picker;

type Term = ratatui::Terminal<CrosstermBackend<BufWriter<Stderr>>>;

/// Browse discovered worktrees. `--json` prints the inventory and returns.
/// `all` scans the whole root; otherwise only the current project is shown.
/// Returns the checkout to change into when the user leaves with `cd`, or a
/// way out when a command removed the directory hq started in.
pub fn run(config: &Config, json: bool, all: bool) -> Result<Option<PathBuf>> {
    std::fs::create_dir_all(config.root_dir()).context("cannot create the bonsai root")?;
    let initial = std::env::current_dir().ok();
    if json {
        let snapshot = inventory::snapshot(config, initial.as_deref(), all)?;
        print_inventory(&snapshot)?;
        return Ok(None);
    }
    // The interface draws on stderr, so the shell wrapper can capture stdout
    // for the directory to change into.
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("hq needs a terminal; pass --json to print the inventory");
    }
    let mut source = Source {
        config,
        cwd: initial.clone(),
        initial,
        all,
    };
    let mut terminal = open_terminal()?;
    let result = drive(&mut terminal, &mut source);
    close_terminal();
    let target = result?;
    Ok(target.or_else(|| source.recovery()))
}

/// Where the inventory comes from, for reloads after a command.
struct Source<'a> {
    config: &'a Config,
    /// Where hq looks for the current project. After the first discovery
    /// this is the project's main checkout, which outlives its worktrees.
    initial: Option<PathBuf>,
    /// The shell's directory when hq started.
    cwd: Option<PathBuf>,
    all: bool,
}

impl Source<'_> {
    fn discover(&self) -> Result<Snapshot> {
        inventory::discover(self.config, self.initial.as_deref(), self.all)
    }

    fn reload(&self, model: &mut Model) -> Result<(), String> {
        match self.discover() {
            Ok(snapshot) => {
                model.set_snapshot(snapshot);
                Ok(())
            }
            Err(error) => Err(format!("refresh failed: {error}")),
        }
    }

    /// Anchor on the main checkout of the project containing the cwd.
    fn anchor(&mut self, snapshot: &Snapshot) {
        let Some(cwd) = self.cwd.as_deref() else {
            return;
        };
        let cwd = crate::paths::canonicalize_or_self(cwd);
        if let Some(project) = snapshot.projects.iter().find(|project| {
            project
                .worktrees
                .iter()
                .any(|worktree| cwd.starts_with(&worktree.path))
        }) {
            self.initial = Some(project.path.clone());
        }
    }

    /// When a command removed the directory the shell is in, leave for the
    /// project's main checkout, or the bonsai root.
    fn recovery(&self) -> Option<PathBuf> {
        let cwd = self.cwd.as_deref()?;
        if cwd.exists() {
            return None;
        }
        self.initial
            .clone()
            .filter(|path| path.exists())
            .or_else(|| Some(self.config.root_dir()))
    }

    /// Global options hq itself received, for the commands it starts.
    fn flags(&self) -> Vec<OsString> {
        let mut flags = vec![
            OsString::from("--root"),
            self.config.root_dir().into_os_string(),
        ];
        if let Some(remote) = &self.config.remote {
            flags.extend([OsString::from("--remote"), OsString::from(remote)]);
        }
        flags
    }
}

fn open_terminal() -> Result<Term> {
    let previous = std::panic::take_hook();
    let interface = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        // A panicking worker must not tear the interface down under the UI.
        if std::thread::current().id() == interface {
            close_terminal();
        }
        previous(info);
    }));
    crossterm::terminal::enable_raw_mode().context("cannot start the terminal interface")?;
    let mut output = BufWriter::new(std::io::stderr());
    crossterm::execute!(
        output,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
    )
    .context("cannot start the terminal interface")?;
    let mut terminal = ratatui::Terminal::new(CrosstermBackend::new(output))
        .context("cannot start the terminal interface")?;
    let _ = terminal.hide_cursor();
    Ok(terminal)
}

fn close_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stderr(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

fn suspend_terminal(terminal: &mut Term) -> Result<()> {
    let _ = terminal.show_cursor();
    crossterm::terminal::disable_raw_mode().context("cannot leave the terminal interface")?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )
    .context("cannot leave the terminal interface")?;
    Ok(())
}

/// `Terminal::clear` asks the terminal for the cursor position on stdout,
/// which the shell wrapper captures, so redraw by resizing instead.
fn resume_terminal(terminal: &mut Term) -> Result<()> {
    crossterm::terminal::enable_raw_mode().context("cannot restore the terminal interface")?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
    )
    .context("cannot restore the terminal interface")?;
    let size = terminal
        .size()
        .context("cannot restore the terminal interface")?;
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
        .context("cannot restore the terminal interface")?;
    let _ = terminal.hide_cursor();
    Ok(())
}

fn pause_for_return() {
    let _ = writeln!(std::io::stderr(), "\nbonsai: press enter to return");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

/// Keys typed before an automatically opened question are not answers.
const ANSWER_DELAY: Duration = Duration::from_millis(300);

fn drive(terminal: &mut Term, source: &mut Source<'_>) -> Result<Option<PathBuf>> {
    terminal.draw(|frame| view::draw_message(frame, "Loading worktrees…"))?;
    let snapshot = source.discover()?;
    source.anchor(&snapshot);
    let mut model = Model::from_snapshot(snapshot);
    model.scrolloff = source.config.hq.scrolloff;
    model.flags = source.flags();
    if let Some(path) = owner::cache_path() {
        model.owners = owner::load(&path);
        model.owner_cache = Some(path);
    }
    if let Some(path) = footprint::cache_path() {
        model.load_sizes(path);
    }
    let mut probes = begin_probes(&mut model, Remeasure::All);
    let mut running: Option<(jobs::Background, Option<String>)> = None;
    // A first `q` while a command runs only warns; only keys clear that.
    let mut stop_armed = false;
    let outcome = loop {
        terminal.draw(|frame| view::draw(frame, &mut model))?;
        let mut next: Option<Action> = None;
        let (effect, keyed) = loop {
            let mut changed = drain(&probes, &mut model);
            if let Some((job, _)) = running.as_mut() {
                match pump(job, &mut model)? {
                    Pumped::Idle => {}
                    Pumped::Output => changed = true,
                    Pumped::Exited(status) => {
                        let (job, project) = running.take().expect("a running command");
                        let remeasure = project.map_or(Remeasure::None, Remeasure::Project);
                        let finished = finish(&job, status, &mut model);
                        if finished.reload {
                            reload(source, &mut model, &mut probes, remeasure);
                        }
                        next = finished.next;
                        changed = true;
                    }
                }
            }
            model.save_sizes(false);
            if changed {
                break (Effect::None, false);
            }
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => break (model.on_key(key), true),
                Event::Resize(_, _) => break (Effect::None, false),
                _ => {}
            }
        };
        let effect = match next {
            Some(action) => Effect::Run(action),
            None => effect,
        };
        let armed = if keyed {
            std::mem::take(&mut stop_armed)
        } else {
            stop_armed
        };
        match effect {
            Effect::None => {}
            Effect::Quit | Effect::Cd(_) if running.is_some() => {
                let Some((job, _)) = running.as_mut() else {
                    continue;
                };
                if armed {
                    job.interrupt();
                    let _ = job.wait();
                    break match effect {
                        Effect::Cd(path) => Some(path),
                        _ => None,
                    };
                }
                stop_armed = true;
                model.notice = Some(format!(
                    "{} is still running; press again to stop it and leave",
                    job.spec.label
                ));
            }
            Effect::Quit => break None,
            Effect::Cd(path) => break Some(path),
            Effect::Refresh => {
                model.notice = Some("Refreshing…".to_string());
                terminal.draw(|frame| view::draw(frame, &mut model))?;
                reload(source, &mut model, &mut probes, Remeasure::All);
                if model.notice.as_deref() == Some("Refreshing…") {
                    model.notice = None;
                }
            }
            Effect::Copy(text) => {
                let place = jobs::copy(&text);
                model.notice = Some(format!(
                    "copied {} to the {place}",
                    path_label(Path::new(&text))
                ));
            }
            Effect::Run(action) => {
                if let Some((job, _)) = &running {
                    model.notice = Some(format!("wait for {} to finish", job.spec.label));
                    continue;
                }
                match action {
                    Action::Background { spec, project } => match jobs::Background::start(spec) {
                        Ok(job) => {
                            model.pane = Some(jobs::Pane::new(&job.spec.label));
                            running = Some((job, project));
                        }
                        Err(error) => model.notice = Some(format!("{error:#}")),
                    },
                    Action::Foreground(task) => {
                        suspend_terminal(terminal)?;
                        let held = jobs::HoldInterrupts::new();
                        let outcome = jobs::foreground(&task.program, &task.args, &task.dir);
                        let failed = !matches!(&outcome, Ok(status) if status.success());
                        if let Err(error) = &outcome {
                            let _ = writeln!(std::io::stderr(), "bonsai: {error:#}");
                        }
                        if task.pause || failed {
                            pause_for_return();
                        }
                        drop(held);
                        resume_terminal(terminal)?;
                        let message = match outcome {
                            Ok(status) if status.success() => format!("{} finished", task.label),
                            Ok(status) => format!("{} failed: {}", task.label, exit_reason(status)),
                            Err(error) => format!("{error:#}"),
                        };
                        let remeasure = task.project.map_or(Remeasure::None, Remeasure::Project);
                        reload(source, &mut model, &mut probes, remeasure);
                        if model.notice.is_none() {
                            model.notice = Some(message);
                        }
                    }
                }
            }
        }
    };
    model.save_sizes(true);
    Ok(outcome)
}

fn reload(source: &Source<'_>, model: &mut Model, probes: &mut Probes, remeasure: Remeasure) {
    match source.reload(model) {
        Ok(()) => {
            *probes = begin_probes(model, remeasure);
            if let Some(path) = model.select_after.take() {
                model.reveal(&path);
            }
        }
        Err(error) => {
            model.note_error(&error);
            model.notice = Some(error);
        }
    }
}

enum Pumped {
    Idle,
    Output,
    Exited(ExitStatus),
}

fn pump(job: &mut jobs::Background, model: &mut Model) -> Result<Pumped> {
    let mut output = false;
    // Bounded, so a chatty command cannot starve the keyboard.
    for _ in 0..512 {
        match job.poll()? {
            jobs::Poll::Chunk(chunk) => {
                if let Some(pane) = model.pane.as_mut() {
                    pane.push(chunk);
                }
                output = true;
            }
            jobs::Poll::Pending => break,
            jobs::Poll::Exited(status) => return Ok(Pumped::Exited(status)),
        }
    }
    Ok(if output { Pumped::Output } else { Pumped::Idle })
}

struct Finished {
    reload: bool,
    /// A command to start right away, such as a verified clean.
    next: Option<Action>,
}

/// Record a finished background command.
fn finish(job: &jobs::Background, status: ExitStatus, model: &mut Model) -> Finished {
    let label = job.spec.label.clone();
    let ok = status.success();
    if let Some(pane) = model.pane.as_mut() {
        pane.state = if ok {
            jobs::State::Succeeded
        } else {
            jobs::State::Failed(exit_reason(status))
        };
    }
    let planning = matches!(
        job.spec.after,
        jobs::After::CleanPlan { .. } | jobs::After::CleanVerify { .. }
    );
    if !ok {
        model.notice = Some(format!(
            "{label} failed: {}; its output is below",
            exit_reason(status)
        ));
        return Finished {
            reload: !planning,
            next: None,
        };
    }
    let mut next = None;
    match &job.spec.after {
        jobs::After::Refresh => model.notice = Some(format!("{label} finished")),
        jobs::After::SelectPrinted => {
            model.select_after = job
                .stdout
                .iter()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(|line| PathBuf::from(line.trim()));
            model.notice = Some(format!("{label} finished"));
        }
        jobs::After::CleanPlan { project } => {
            model.pane = None;
            model.offer_clean_plan(project, &job.stdout.join("\n"));
        }
        jobs::After::CleanVerify { project, expected } => {
            model.pane = None;
            next = model.verified_clean(project, expected, &job.stdout.join("\n"));
        }
    }
    Finished {
        reload: !planning,
        next,
    }
}

fn exit_reason(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit {code}"),
        None => "stopped by a signal".to_string(),
    }
}

struct Probes {
    measure: footprint::MeasureJob,
    status: inventory::StatusJob,
    owners: owner::OwnerJob,
}

/// Which sizes to measure again after a reload. Others keep this session's
/// measurement.
enum Remeasure {
    All,
    Project(String),
    None,
}

fn begin_probes(model: &mut Model, remeasure: Remeasure) -> Probes {
    model.generation = model.generation.saturating_add(1);
    match remeasure {
        Remeasure::All => model.fresh.clear(),
        Remeasure::Project(id) => {
            if let Some(project) = model.snapshot.projects.iter().find(|item| item.id == id) {
                for worktree in &project.worktrees {
                    model.fresh.remove(&worktree.path);
                }
            }
        }
        Remeasure::None => {}
    }
    let paths = inventory::status_paths(&model.snapshot);
    model.pending = paths.iter().cloned().collect();
    for project in &mut model.snapshot.projects {
        if project.id.starts_with("unavailable:") {
            continue;
        }
        for worktree in &mut project.worktrees {
            worktree.dirty = None;
            worktree.added = 0;
            worktree.modified = 0;
            worktree.deleted = 0;
            worktree.untracked = 0;
            worktree.ahead = 0;
            worktree.behind = 0;
            worktree.upstream = None;
            worktree.upstream_gone = false;
        }
    }
    let first = model.selected_project().map(|project| project.id.clone());
    let measure: Vec<PathBuf> = rows::measure_order(model, first.as_deref())
        .into_iter()
        .filter(|path| !model.fresh.contains(path))
        .collect();
    model.measuring = measure.iter().cloned().collect();
    model.rebuild();
    let pending_owners = unknown_github_owners(&model.snapshot, &model.owners);
    Probes {
        measure: footprint::start(measure, model.generation),
        status: inventory::start_status(paths, model.generation),
        owners: owner::start(pending_owners),
    }
}

fn drain(probes: &Probes, model: &mut Model) -> bool {
    let mut changed = false;
    while let Some(update) = probes.status.try_recv() {
        changed |= model.apply_status(update);
    }
    while let Some(update) = probes.measure.try_recv() {
        changed |= model.apply_footprint(update);
    }
    while let Some(update) = probes.owners.try_recv() {
        changed |= model.apply_owner(update);
    }
    changed
}

fn unknown_github_owners(
    snapshot: &Snapshot,
    known: &BTreeMap<String, owner::OwnerKind>,
) -> Vec<String> {
    let mut names = BTreeSet::new();
    for project in &snapshot.projects {
        let Some(remote) = project.remote.as_deref() else {
            continue;
        };
        let Some(id) = owner::owner_from_remote(remote) else {
            continue;
        };
        if id.host != "github.com" {
            continue;
        }
        if known.contains_key(&owner::cache_key(&id)) {
            continue;
        }
        names.insert(id.name.to_lowercase());
    }
    names.into_iter().collect()
}

fn print_inventory(snapshot: &Snapshot) -> Result<()> {
    let paths = snapshot
        .projects
        .iter()
        .flat_map(|project| {
            project
                .worktrees
                .iter()
                .filter(|worktree| !worktree.missing)
                .map(|worktree| worktree.path.clone())
        })
        .collect::<Vec<_>>();
    let footprints = footprint::measure_all(&paths);
    let mut value = serde_json::to_value(snapshot).context("cannot format the inventory")?;
    if let Some(projects) = value
        .get_mut("projects")
        .and_then(|value| value.as_array_mut())
    {
        for project in projects {
            let Some(worktrees) = project
                .get_mut("worktrees")
                .and_then(|value| value.as_array_mut())
            else {
                continue;
            };
            for worktree in worktrees {
                let Some(path) = worktree.get("path").and_then(|value| value.as_str()) else {
                    continue;
                };
                let Some(item) = footprints.get(Path::new(path)) else {
                    continue;
                };
                let Some(object) = worktree.as_object_mut() else {
                    continue;
                };
                object.insert(
                    "footprint".to_string(),
                    serde_json::to_value(item).context("cannot format a footprint")?,
                );
            }
        }
    }
    crate::output::line(format_args!(
        "{}",
        serde_json::to_string_pretty(&value).context("cannot format the inventory")?
    ))?;
    Ok(())
}

/// First visible row so the cursor keeps `scrolloff` rows of context.
///
/// The window stays put while the cursor moves inside it. It scrolls when the
/// cursor would sit closer than that to the top or bottom. At either end of
/// the list the margin shrinks, and a short window caps it so the cursor can
/// still move.
fn scroll_top(scroll: usize, cursor: usize, height: usize, len: usize, scrolloff: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let height = height.min(len);
    let max_top = len - height;
    let cursor = cursor.min(len - 1);
    let margin = scrolloff.min(height.saturating_sub(1) / 2);
    let mut top = scroll.min(max_top);
    let bottom = top + height - 1;
    if cursor < top + margin {
        top = cursor.saturating_sub(margin);
    } else if cursor + margin > bottom {
        top = (cursor + margin).saturating_sub(height - 1);
    }
    top.min(max_top)
}

/// A key with Shift folded into the character, as terminals report either.
fn shifted(key: &KeyEvent) -> KeyCode {
    match key.code {
        KeyCode::Char(ch) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            KeyCode::Char(ch.to_ascii_uppercase())
        }
        code => code,
    }
}

fn ctrl_letter(key: &KeyEvent, ch: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char(got) if got.eq_ignore_ascii_case(&ch))
}

fn plain(key: &KeyEvent) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

struct Model {
    snapshot: Snapshot,
    filter: LineInput,
    filtering: bool,
    /// First key of a two-key command: `g` (gg), `z` (zz zt zb), `d` (dd),
    /// or `y` (yy).
    pending_key: Option<char>,
    /// A count typed before a motion, as in vim: `5j`.
    count: Option<usize>,
    /// Home list nests host, owner, repository, then each checkout.
    tree: bool,
    /// `s` cycles this. Alphabetical is the order the list opens in.
    sort: Sort,
    view: View,
    palette: Option<Palette>,
    confirm: Option<Confirm>,
    help: bool,
    /// Output of the latest background command.
    pane: Option<jobs::Pane>,
    /// Checkout to select once the next reload finishes.
    select_after: Option<PathBuf>,
    /// A clean plan that arrived while you were typing, opened by `C`.
    ready_plan: Option<(PathBuf, String)>,
    /// Global options passed on to the commands hq starts.
    flags: Vec<OsString>,
    rows: Vec<Row>,
    cursor: usize,
    notice: Option<String>,
    /// Best known size of each checkout: this session's measurement, a
    /// partial walk, or the last session's.
    footprints: HashMap<PathBuf, footprint::Footprint>,
    /// Checkouts measured completely by this session.
    fresh: HashSet<PathBuf>,
    /// Checkouts the current size job is measuring.
    measuring: HashSet<PathBuf>,
    size_cache: Option<PathBuf>,
    cached: HashMap<PathBuf, footprint::Cached>,
    sizes_changed: bool,
    sizes_saved: Option<Instant>,
    pending: HashSet<PathBuf>,
    owners: BTreeMap<String, owner::OwnerKind>,
    owner_cache: Option<PathBuf>,
    generation: u64,
    logged: HashSet<String>,
    /// Visible list rows from the last draw. Page motions use this.
    viewport: u16,
    /// Rows kept above and below the cursor before the list scrolls.
    scrolloff: usize,
    /// First visible row. The cursor moves inside this window.
    scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sort {
    Name,
    Recent,
    Size,
}

impl Sort {
    fn cycle(self) -> Self {
        match self {
            Self::Name => Self::Recent,
            Self::Recent => Self::Size,
            Self::Size => Self::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Recent => "recent",
            Self::Size => "size",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum View {
    List,
    Detail { project: String },
}

struct Palette {
    query: LineInput,
    cursor: usize,
    scroll: usize,
}

/// A question shown before a command that changes or deletes things.
struct Confirm {
    title: String,
    lines: Vec<Line<'static>>,
    yes: &'static str,
    tone: Color,
    action: Action,
    /// Opened by a command finishing rather than by a key: keys before this
    /// moment were typed for something else.
    ready_at: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    Add,
    Cd,
    Remove,
    Delete,
    Clean,
    Prune,
    Fetch,
    Start,
    Resume,
    Edit,
    Copy,
    Refresh,
    Sort,
    Tree,
    Help,
    Quit,
}

struct CommandSpec {
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    hint: &'static str,
    key: &'static str,
    cmd: Cmd,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "add",
        aliases: &["new"],
        args: "<branch>",
        hint: "create a branch and its worktree in this project",
        key: "a",
        cmd: Cmd::Add,
    },
    CommandSpec {
        name: "cd",
        aliases: &["open"],
        args: "",
        hint: "leave hq in the selected checkout",
        key: "c",
        cmd: Cmd::Cd,
    },
    CommandSpec {
        name: "remove",
        aliases: &["rm"],
        args: "",
        hint: "remove the selected worktree, keep its branch",
        key: "dd",
        cmd: Cmd::Remove,
    },
    CommandSpec {
        name: "delete",
        aliases: &[],
        args: "",
        hint: "remove the selected worktree and delete its branch",
        key: "D",
        cmd: Cmd::Delete,
    },
    CommandSpec {
        name: "clean",
        aliases: &[],
        args: "",
        hint: "remove this project's merged worktrees",
        key: "C",
        cmd: Cmd::Clean,
    },
    CommandSpec {
        name: "prune",
        aliases: &[],
        args: "[all]",
        hint: "drop deleted and stale worktrees; all: every project",
        key: "P",
        cmd: Cmd::Prune,
    },
    CommandSpec {
        name: "fetch",
        aliases: &[],
        args: "",
        hint: "fetch this project's remote",
        key: "f",
        cmd: Cmd::Fetch,
    },
    CommandSpec {
        name: "start",
        aliases: &["agent"],
        args: "[claude|codex|opencode]",
        hint: "start a coding agent in the selected checkout",
        key: "A",
        cmd: Cmd::Start,
    },
    CommandSpec {
        name: "resume",
        aliases: &[],
        args: "[query]",
        hint: "resume a coding-agent session from here",
        key: "R",
        cmd: Cmd::Resume,
    },
    CommandSpec {
        name: "edit",
        aliases: &["editor"],
        args: "",
        hint: "open the selected checkout in $VISUAL or $EDITOR",
        key: "e",
        cmd: Cmd::Edit,
    },
    CommandSpec {
        name: "copy",
        aliases: &["yank"],
        args: "",
        hint: "copy the selected checkout's path",
        key: "yy",
        cmd: Cmd::Copy,
    },
    CommandSpec {
        name: "refresh",
        aliases: &["reload"],
        args: "",
        hint: "reload worktrees, status, and sizes",
        key: "r",
        cmd: Cmd::Refresh,
    },
    CommandSpec {
        name: "sort",
        aliases: &[],
        args: "[name|recent|size]",
        hint: "change the order",
        key: "s",
        cmd: Cmd::Sort,
    },
    CommandSpec {
        name: "tree",
        aliases: &[],
        args: "",
        hint: "show or hide the tree",
        key: "t",
        cmd: Cmd::Tree,
    },
    CommandSpec {
        name: "help",
        aliases: &[],
        args: "",
        hint: "list every key",
        key: "?",
        cmd: Cmd::Help,
    },
    CommandSpec {
        name: "quit",
        aliases: &["q", "exit"],
        args: "",
        hint: "leave hq",
        key: "q",
        cmd: Cmd::Quit,
    },
];

/// A command started from headquarters.
#[derive(Debug)]
enum Action {
    /// Streams into the output pane. `project` is re-measured afterwards.
    Background {
        spec: jobs::Spec,
        project: Option<String>,
    },
    /// Takes over the terminal while the interface is suspended.
    Foreground(Task),
}

#[derive(Debug)]
struct Task {
    label: String,
    program: PathBuf,
    args: Vec<OsString>,
    dir: PathBuf,
    /// Wait for enter afterwards so the output can be read.
    pause: bool,
    project: Option<String>,
}

#[derive(Debug)]
enum Effect {
    None,
    Quit,
    Refresh,
    Cd(PathBuf),
    Copy(String),
    Run(Action),
}

#[derive(Clone, Copy)]
enum Place {
    Top,
    Middle,
    Bottom,
}

impl Model {
    fn from_snapshot(snapshot: Snapshot) -> Self {
        let mut model = Self {
            snapshot,
            filter: LineInput::default(),
            filtering: false,
            pending_key: None,
            count: None,
            tree: false,
            sort: Sort::Name,
            view: View::List,
            palette: None,
            confirm: None,
            help: false,
            pane: None,
            select_after: None,
            ready_plan: None,
            flags: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            notice: None,
            footprints: HashMap::new(),
            fresh: HashSet::new(),
            measuring: HashSet::new(),
            size_cache: None,
            cached: HashMap::new(),
            sizes_changed: false,
            sizes_saved: None,
            pending: HashSet::new(),
            owners: BTreeMap::new(),
            owner_cache: None,
            generation: 0,
            logged: HashSet::new(),
            viewport: 0,
            scrolloff: 10,
            scroll: 0,
        };
        model.rebuild();
        model.record_warnings();
        model
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        let missing = matches!(&self.view, View::Detail { project } if !snapshot
            .projects
            .iter()
            .any(|item| &item.id == project));
        if missing {
            self.view = View::List;
        }
        self.snapshot = snapshot;
        self.rebuild();
        self.record_warnings();
    }

    fn on_key(&mut self, key: KeyEvent) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        self.notice = None;
        if self.help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) || ctrl_letter(&key, 'o')
            {
                self.help = false;
            } else if ctrl_letter(&key, 'c') {
                return Effect::Quit;
            }
            return Effect::None;
        }
        if self.confirm.is_some() {
            return self.on_confirm_key(key);
        }
        if self.palette.is_some() {
            return self.on_palette_key(key);
        }
        if self.filtering {
            return self.on_filter_key(key);
        }
        self.on_normal_key(key)
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> Effect {
        if ctrl_letter(&key, 'c') {
            return Effect::Quit;
        }
        if self
            .confirm
            .as_ref()
            .and_then(|confirm| confirm.ready_at)
            .is_some_and(|at| Instant::now() < at)
        {
            return Effect::None;
        }
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                let confirm = self.confirm.take().expect("a confirmation");
                Effect::Run(confirm.action)
            }
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => {
                self.confirm = None;
                self.notice = Some("cancelled".to_string());
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Effect::Quit,
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Char('p') if ctrl => self.move_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_selection(1),
            KeyCode::PageUp => self.move_rows(-self.page_delta(false)),
            KeyCode::PageDown => self.move_rows(self.page_delta(false)),
            KeyCode::Enter | KeyCode::Tab => self.finish_filter(),
            KeyCode::Char('i') if ctrl => self.finish_filter(),
            KeyCode::Esc => self.go_up(),
            KeyCode::Char('o') if ctrl => self.go_up(),
            _ if self.filter.edit(&key) => self.rebuild(),
            _ => {}
        }
        Effect::None
    }

    /// Enter in search: open the project of the match on the home list, or
    /// keep the filter and return to the keys inside a project.
    fn finish_filter(&mut self) {
        if matches!(self.view, View::List) {
            self.go_in();
        } else {
            self.filtering = false;
        }
    }

    fn take_count(&mut self) -> Option<usize> {
        self.count.take()
    }

    fn on_normal_key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let code = shifted(&key);
        if let Some(first) = self.pending_key.take()
            && plain(&key)
        {
            let count = self.take_count();
            match (first, code) {
                ('g', KeyCode::Char('g')) => {
                    self.jump_to(count, false);
                    return Effect::None;
                }
                ('z', KeyCode::Char('z')) => return self.align(Place::Middle),
                ('z', KeyCode::Char('t')) => return self.align(Place::Top),
                ('z', KeyCode::Char('b')) => return self.align(Place::Bottom),
                ('d', KeyCode::Char('d')) => return self.command(Cmd::Remove, ""),
                ('y', KeyCode::Char('y')) => return self.command(Cmd::Copy, ""),
                _ => {}
            }
        }
        if let KeyCode::Char(digit @ '0'..='9') = code
            && plain(&key)
            && (digit != '0' || self.count.is_some())
        {
            let value = u32::from(digit) - u32::from('0');
            let next = self
                .count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(value as usize)
                .min(99_999);
            self.count = Some(next);
            return Effect::None;
        }
        let count = self.take_count();
        let times = isize::try_from(count.unwrap_or(1)).unwrap_or(1);
        if ctrl {
            match code {
                KeyCode::Char('c' | 'C') => return Effect::Quit,
                KeyCode::Char('p' | 'P') => self.move_selection(-times),
                KeyCode::Char('n' | 'N') => self.move_selection(times),
                KeyCode::Char('d' | 'D') => self.move_rows(self.page_delta(true) * times),
                KeyCode::Char('u' | 'U') => self.move_rows(-self.page_delta(true) * times),
                KeyCode::Char('f' | 'F') => self.move_rows(self.page_delta(false) * times),
                KeyCode::Char('b' | 'B') => self.move_rows(-self.page_delta(false) * times),
                KeyCode::Char('e' | 'E') => self.scroll_view(times),
                KeyCode::Char('y' | 'Y') => self.scroll_view(-times),
                KeyCode::Char('i' | 'I') => self.go_in(),
                KeyCode::Char('o' | 'O') => self.go_up(),
                _ => {}
            }
            return Effect::None;
        }
        if !plain(&key) {
            return Effect::None;
        }
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-times),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(times),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => self.go_up(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => self.go_in(),
            KeyCode::Enter | KeyCode::Char('o') => return self.open(),
            KeyCode::PageUp => self.move_rows(-self.page_delta(false) * times),
            KeyCode::PageDown => self.move_rows(self.page_delta(false) * times),
            KeyCode::Home => self.jump_to(None, false),
            KeyCode::End => self.jump_to(None, true),
            KeyCode::Char('G') => self.jump_to(count, true),
            KeyCode::Char(first @ ('g' | 'z' | 'd' | 'y')) => {
                self.pending_key = Some(first);
                self.count = count;
            }
            KeyCode::Char('H') => self.screen_jump(Place::Top),
            KeyCode::Char('M') => self.screen_jump(Place::Middle),
            KeyCode::Char('L') => self.screen_jump(Place::Bottom),
            KeyCode::Char('{') => self.jump_group(-times),
            KeyCode::Char('}') => self.jump_group(times),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char(':') => self.open_palette(""),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Char('r') => return Effect::Refresh,
            KeyCode::Char('t') => return self.command(Cmd::Tree, ""),
            KeyCode::Char('s') => return self.command(Cmd::Sort, ""),
            KeyCode::Char('a') => return self.command(Cmd::Add, ""),
            KeyCode::Char('c') => return self.command(Cmd::Cd, ""),
            KeyCode::Char('D') => return self.command(Cmd::Delete, ""),
            KeyCode::Char('C') => return self.command(Cmd::Clean, ""),
            KeyCode::Char('P') => return self.command(Cmd::Prune, ""),
            KeyCode::Char('f') => return self.command(Cmd::Fetch, ""),
            KeyCode::Char('e') => return self.command(Cmd::Edit, ""),
            KeyCode::Char('A') => return self.command(Cmd::Start, ""),
            KeyCode::Char('R') => return self.command(Cmd::Resume, ""),
            _ => {}
        }
        Effect::None
    }

    fn open_palette(&mut self, text: &str) {
        let mut query = LineInput::default();
        query.set(text);
        self.palette = Some(Palette {
            query,
            cursor: 0,
            scroll: 0,
        });
    }

    fn on_palette_key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Effect::Quit,
            KeyCode::Esc => self.go_up(),
            KeyCode::Char('o') if ctrl => self.go_up(),
            KeyCode::Enter => {
                let Some(palette) = &self.palette else {
                    return Effect::None;
                };
                let text = palette.query.text().trim().to_string();
                let args = text
                    .split_once(char::is_whitespace)
                    .map_or("", |(_, rest)| rest.trim())
                    .to_string();
                let Some(cmd) = self.selected_command() else {
                    return Effect::None;
                };
                self.palette = None;
                return self.command(cmd, &args);
            }
            KeyCode::Tab => {
                if let Some(spec) = palette_matches(self)
                    .get(self.palette.as_ref().map_or(0, |palette| palette.cursor))
                    .map(|spec| spec.name)
                    && let Some(palette) = self.palette.as_mut()
                {
                    let text = palette.query.text().trim_start().to_string();
                    let args = text
                        .split_once(char::is_whitespace)
                        .map_or("", |(_, rest)| rest);
                    palette.query.set(format!("{spec} {args}"));
                    palette.cursor = 0;
                }
            }
            KeyCode::Up | KeyCode::Char('p') if ctrl || matches!(key.code, KeyCode::Up) => {
                self.move_palette(-1);
            }
            KeyCode::Down | KeyCode::Char('n') if ctrl || matches!(key.code, KeyCode::Down) => {
                self.move_palette(1);
            }
            KeyCode::PageUp => self.move_palette(-self.page_delta(false)),
            KeyCode::PageDown => self.move_palette(self.page_delta(false)),
            _ if self
                .palette
                .as_mut()
                .is_some_and(|palette| palette.query.edit(&key)) =>
            {
                self.clamp_palette();
            }
            _ => {}
        }
        Effect::None
    }

    /// Run a command on the selection. Keys and the command line share this.
    fn command(&mut self, cmd: Cmd, args: &str) -> Effect {
        match cmd {
            Cmd::Add => self.add(args),
            Cmd::Cd => match self.checkout_path() {
                Some((path, missing)) if !missing => Effect::Cd(path),
                Some(_) => self.say("its directory was deleted; press P to prune it"),
                None => self.say("select a checkout first"),
            },
            Cmd::Remove => self.confirm_remove(false),
            Cmd::Delete => self.confirm_remove(true),
            Cmd::Clean => {
                let ready = self.selected_project().map(|project| project.path.clone());
                // A plan for another project is stale; it is dropped.
                if let Some((project, report)) = self.ready_plan.take()
                    && ready.as_ref() == Some(&project)
                {
                    self.open_clean_plan(&project, &report, None);
                    return Effect::None;
                }
                self.background(&["clean", "--dry-run", "--json"], |project| {
                    jobs::After::CleanPlan {
                        project: project.to_path_buf(),
                    }
                })
            }
            Cmd::Prune => {
                let all = args.trim() == "all";
                let args: &[&str] = if all { &["prune", "-a"] } else { &["prune"] };
                self.foreground_bonsai(args, true, !all)
            }
            Cmd::Fetch => {
                let Some(project) = self.selected_project() else {
                    return self.say("select a project first");
                };
                let spec =
                    jobs::Spec::git(&project.path, &["fetch", "--prune"], jobs::After::Refresh);
                let project = Some(project.id.clone());
                Effect::Run(Action::Background { spec, project })
            }
            Cmd::Start => {
                let agent = args.split_whitespace().next();
                let mut args = vec!["start"];
                args.extend(agent);
                self.foreground_in_checkout(&args)
            }
            Cmd::Resume => {
                let query = if args.trim().is_empty() {
                    self.selected_worktree()
                        .and_then(|(_, worktree)| worktree.branch.clone())
                        .unwrap_or_default()
                } else {
                    args.trim().to_string()
                };
                let mut args = vec!["resume"];
                if !query.is_empty() {
                    args.push(query.as_str());
                }
                self.foreground_in_checkout(&args)
            }
            Cmd::Edit => {
                let Some((path, missing)) = self.checkout_path() else {
                    return self.say("select a checkout first");
                };
                if missing {
                    return self.say("its directory was deleted");
                }
                let Some((program, mut args)) = jobs::editor() else {
                    return self.say("set $VISUAL or $EDITOR to choose an editor");
                };
                args.push(path.clone().into_os_string());
                Effect::Run(Action::Foreground(Task {
                    label: program.display().to_string(),
                    program,
                    args,
                    dir: path,
                    pause: false,
                    project: None,
                }))
            }
            Cmd::Copy => match self.checkout_path() {
                Some((path, _)) => Effect::Copy(path.display().to_string()),
                None => self.say("select a checkout first"),
            },
            Cmd::Refresh => Effect::Refresh,
            Cmd::Sort => {
                self.sort = match args.trim() {
                    "name" | "a-z" => Sort::Name,
                    "recent" => Sort::Recent,
                    "size" => Sort::Size,
                    _ => self.sort.cycle(),
                };
                self.rebuild();
                Effect::None
            }
            Cmd::Tree => {
                self.tree = !self.tree;
                self.view = View::List;
                self.rebuild();
                Effect::None
            }
            Cmd::Help => {
                self.help = true;
                Effect::None
            }
            Cmd::Quit => Effect::Quit,
        }
    }

    fn say(&mut self, notice: &str) -> Effect {
        self.notice = Some(notice.to_string());
        Effect::None
    }

    fn add(&mut self, branch: &str) -> Effect {
        let branch = branch.trim();
        if branch.is_empty() {
            self.open_palette("add ");
            return self.say("type the new branch name, then press enter");
        }
        self.background(&["add", branch], |_| jobs::After::SelectPrinted)
    }

    /// A bonsai subcommand for the selected project, streamed into the pane.
    fn background(&mut self, args: &[&str], after: impl Fn(&Path) -> jobs::After) -> Effect {
        let Some(project) = self.selected_project() else {
            return self.say("select a project first");
        };
        let (path, id) = (project.path.clone(), project.id.clone());
        match jobs::Spec::bonsai(&path, &self.flags, args, after(&path)) {
            Ok(spec) => Effect::Run(Action::Background {
                spec,
                project: Some(id),
            }),
            Err(error) => self.say(&format!("{error:#}")),
        }
    }

    fn foreground_bonsai(&mut self, args: &[&str], pause: bool, scoped: bool) -> Effect {
        let Some(project) = self.selected_project() else {
            return self.say("select a project first");
        };
        let (dir, id) = (project.path.clone(), project.id.clone());
        self.foreground_at(&dir, args, pause, scoped.then_some(id))
    }

    fn foreground_in_checkout(&mut self, args: &[&str]) -> Effect {
        let Some((dir, missing)) = self.checkout_path() else {
            return self.say("select a checkout first");
        };
        if missing {
            return self.say("its directory was deleted; press P to prune it");
        }
        let project = self.selected_project().map(|project| project.id.clone());
        self.foreground_at(&dir, args, false, project)
    }

    fn foreground_at(
        &mut self,
        dir: &Path,
        args: &[&str],
        pause: bool,
        project: Option<String>,
    ) -> Effect {
        let program = match std::env::current_exe() {
            Ok(program) => program,
            Err(error) => {
                return self.say(&format!("cannot locate the bonsai executable: {error}"));
            }
        };
        let mut full: Vec<OsString> = vec!["-C".into(), dir.as_os_str().to_owned()];
        full.extend(self.flags.iter().cloned());
        full.extend(args.iter().map(OsString::from));
        Effect::Run(Action::Foreground(Task {
            label: format!("bonsai {}", args.join(" ")),
            program,
            args: full,
            dir: dir.to_path_buf(),
            pause,
            project,
        }))
    }

    fn confirm_remove(&mut self, delete_branch: bool) -> Effect {
        let Some((project, worktree)) = self.selected_worktree() else {
            return self.say("open a project and select a checkout first");
        };
        if worktree.main {
            return self.say("the main checkout cannot be removed");
        }
        if worktree.external {
            return self.say("external worktrees are read-only to bonsai");
        }
        if delete_branch && worktree.branch.is_none() {
            return self.say("a detached checkout has no branch to delete; use dd");
        }
        let branch = rows::branch_label(worktree);
        let warn = Style::new().fg(Color::Yellow);
        let mut lines = vec![Line::raw(path_label(&worktree.path)), Line::raw("")];
        if worktree.missing {
            lines.push(Line::raw(
                "Its directory is already gone; this drops the registration.",
            ));
        } else if worktree.dirty == Some(true) {
            lines.push(Line::styled(
                "It has uncommitted changes, so bonsai will refuse to remove it.",
                Style::new().fg(Color::Red),
            ));
        }
        // `remove -d` deletes the branch only when its tip is on its upstream,
        // or on the main checkout's HEAD without one; otherwise it refuses.
        if delete_branch {
            if self.pending.contains(&worktree.path) {
                lines.push(Line::raw(
                    "Its status is still loading; bonsai checks the branch before deleting it.",
                ));
            } else if worktree.upstream_gone {
                lines.push(Line::styled(
                    "Its upstream is gone, so the branch is deleted only if it is merged \
                     into the main checkout. For squash-merged branches, use C to clean.",
                    warn,
                ));
            } else if let Some(upstream) = &worktree.upstream {
                if worktree.ahead > 0 {
                    lines.push(Line::styled(
                        format!(
                            "{} not on {upstream}, so bonsai will refuse to delete the branch.",
                            commits(worktree.ahead)
                        ),
                        warn,
                    ));
                } else {
                    lines.push(Line::raw(format!(
                        "Its commits are on {upstream}, so the branch can be deleted."
                    )));
                }
            } else {
                lines.push(Line::styled(
                    "It has no upstream, so the branch is deleted only if it is merged \
                     into the main checkout.",
                    warn,
                ));
            }
        } else {
            lines.push(Line::raw(format!("The branch {branch} is kept.")));
        }
        let mut args = vec!["remove".to_string()];
        if delete_branch {
            args.push("-d".to_string());
        }
        args.push(worktree.path.to_string_lossy().into_owned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (dir, id) = (project.path.clone(), project.id.clone());
        let mut spec = match jobs::Spec::bonsai(&dir, &self.flags, &args, jobs::After::Refresh) {
            Ok(spec) => spec,
            Err(error) => return self.say(&format!("{error:#}")),
        };
        // The path is the precise argument; the branch reads better.
        spec.label = format!("bonsai {} {branch}", args[..args.len() - 1].join(" "));
        self.confirm = Some(Confirm {
            title: if delete_branch {
                format!("Remove {branch} and delete its branch?")
            } else {
                format!("Remove the {branch} worktree?")
            },
            lines,
            yes: if delete_branch { "delete" } else { "remove" },
            tone: if delete_branch {
                Color::Red
            } else {
                Color::Yellow
            },
            action: Action::Background {
                spec,
                project: Some(id),
            },
            ready_at: None,
        });
        Effect::None
    }

    /// A clean plan finished. Show it unless you are typing something else,
    /// so a stray key cannot answer a question you have not read.
    fn offer_clean_plan(&mut self, project: &Path, report: &str) {
        let busy = self.palette.is_some()
            || self.filtering
            || self.confirm.is_some()
            || self.help
            || self.pending_key.is_some()
            || self.count.is_some();
        if busy {
            self.ready_plan = Some((project.to_path_buf(), report.to_string()));
            self.notice = Some("the clean plan is ready; press C to review it".to_string());
            return;
        }
        self.open_clean_plan(project, report, Some(Instant::now() + ANSWER_DELAY));
    }

    /// Show what `bonsai clean` would remove and ask before running it.
    fn open_clean_plan(&mut self, project: &Path, report: &str, ready_at: Option<Instant>) {
        let plan = match CleanPlan::parse(report) {
            Ok(plan) => plan,
            Err(error) => {
                self.notice = Some(format!("cannot read the clean plan: {error}"));
                return;
            }
        };
        let name = self
            .snapshot
            .projects
            .iter()
            .find(|item| item.path == project)
            .map_or_else(|| path_label(project), |item| item.name.clone());
        let mut skipped = Vec::new();
        if plan.dirty > 0 {
            skipped.push(format!("{} with uncommitted changes", plan.dirty));
        }
        if plan.missing > 0 {
            skipped.push(format!(
                "{} whose directory was deleted (press P)",
                plan.missing
            ));
        }
        if plan.planned.is_empty() {
            let mut notice = format!("nothing to clean in {name}");
            if !skipped.is_empty() {
                notice.push_str(&format!("; kept {}", skipped.join(" and ")));
            }
            self.notice = Some(notice);
            return;
        }
        let width = plan
            .planned
            .iter()
            .map(|entry| entry.branch.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines = vec![Line::raw(
            "These worktrees are merged; they and their branches will be deleted:",
        )];
        for entry in &plan.planned {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    pad(entry.branch.clone(), width),
                    Style::new().add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(entry.reason.clone(), Style::new().fg(Color::Green)),
            ]));
        }
        if !skipped.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::raw(format!("Kept: {}.", skipped.join(", "))));
        }
        let id = self
            .snapshot
            .projects
            .iter()
            .find(|item| item.path == project)
            .map(|item| item.id.clone());
        // `clean --yes` plans again when it runs, so check first that it
        // would remove exactly these worktrees.
        let spec = match jobs::Spec::bonsai(
            project,
            &self.flags,
            &["clean", "--dry-run", "--json", "--no-fetch"],
            jobs::After::CleanVerify {
                project: project.to_path_buf(),
                expected: plan.paths(),
            },
        ) {
            Ok(mut spec) => {
                spec.label = "bonsai clean (checking the plan)".to_string();
                spec
            }
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                return;
            }
        };
        self.confirm = Some(Confirm {
            title: format!("Clean {name}?"),
            lines,
            yes: "clean",
            tone: Color::Yellow,
            action: Action::Background { spec, project: id },
            ready_at,
        });
    }

    /// The fresh plan matches what you confirmed: run the clean. Otherwise
    /// show the new plan instead.
    fn verified_clean(
        &mut self,
        project: &Path,
        expected: &[PathBuf],
        report: &str,
    ) -> Option<Action> {
        let current = match CleanPlan::parse(report) {
            Ok(plan) => plan.paths(),
            Err(error) => {
                self.notice = Some(format!("cannot read the clean plan: {error}"));
                return None;
            }
        };
        if current != expected {
            self.open_clean_plan(project, report, Some(Instant::now() + ANSWER_DELAY));
            if self.confirm.is_some() {
                self.notice = Some("the plan changed since you reviewed it".to_string());
            }
            return None;
        }
        let id = self
            .snapshot
            .projects
            .iter()
            .find(|item| item.path == project)
            .map(|item| item.id.clone());
        match jobs::Spec::bonsai(
            project,
            &self.flags,
            &["clean", "--yes", "--no-fetch"],
            jobs::After::Refresh,
        ) {
            Ok(spec) => Some(Action::Background { spec, project: id }),
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                None
            }
        }
    }

    fn apply_footprint(&mut self, update: footprint::Measured) -> bool {
        if update.generation != self.generation {
            return false;
        }
        if update.complete {
            self.fresh.insert(update.path.clone());
            self.cached.insert(
                update.path.clone(),
                footprint::Cached {
                    footprint: update.footprint.clone(),
                    measured_at: now_secs(),
                },
            );
            self.sizes_changed = true;
            self.footprints.insert(update.path, update.footprint);
        } else {
            // Keep a larger earlier size until the walk passes it, so a
            // remembered total does not shrink and grow back.
            let replace = self
                .footprints
                .get(&update.path)
                .is_none_or(|known| update.footprint.bytes >= known.bytes);
            if replace {
                // An interrupted walk still leaves the next session an
                // estimate; the complete measurement replaces it.
                self.cached.insert(
                    update.path.clone(),
                    footprint::Cached {
                        footprint: update.footprint.clone(),
                        measured_at: now_secs(),
                    },
                );
                self.sizes_changed = true;
                self.footprints.insert(update.path, update.footprint);
            }
        }
        self.rebuild();
        true
    }

    fn load_sizes(&mut self, path: PathBuf) {
        self.cached = footprint::load_cache(&path);
        for (checkout, cached) in &self.cached {
            self.footprints
                .insert(checkout.clone(), cached.footprint.clone());
        }
        self.size_cache = Some(path);
        self.rebuild();
    }

    /// Write measured sizes, at most every two seconds unless `now`.
    fn save_sizes(&mut self, now: bool) {
        let Some(path) = &self.size_cache else {
            return;
        };
        if !self.sizes_changed
            || (!now
                && self
                    .sizes_saved
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(2)))
        {
            return;
        }
        self.cached.retain(|checkout, _| checkout.exists());
        let _ = footprint::save_cache(path, &self.cached);
        self.sizes_changed = false;
        self.sizes_saved = Some(Instant::now());
    }

    fn is_measuring(&self, path: &Path) -> bool {
        self.measuring.contains(path) && !self.fresh.contains(path)
    }

    /// Projects, checkouts, known bytes, and checkouts with a known size.
    fn overall(&self) -> (usize, usize, u64, usize) {
        let mut checkouts = 0;
        let mut bytes = 0u64;
        let mut sized = 0;
        for worktree in self
            .snapshot
            .projects
            .iter()
            .flat_map(|project| &project.worktrees)
        {
            checkouts += 1;
            if worktree.missing {
                continue;
            }
            if let Some(item) = self.footprints.get(&worktree.path) {
                bytes = bytes.saturating_add(item.bytes);
                sized += 1;
            }
        }
        (self.snapshot.projects.len(), checkouts, bytes, sized)
    }

    fn apply_status(&mut self, update: inventory::StatusUpdate) -> bool {
        if update.generation != self.generation {
            return false;
        }
        self.pending.remove(&update.path);
        let missing = update
            .warning
            .as_deref()
            .is_some_and(|warning| warning.ends_with("missing checkout"));
        if let Some(warning) = update.warning.clone()
            && !self.snapshot.warnings.contains(&warning)
        {
            self.snapshot.warnings.push(warning);
        }
        self.record_warnings();
        let mut found = false;
        for project in &mut self.snapshot.projects {
            for worktree in &mut project.worktrees {
                if worktree.path == update.path {
                    worktree.dirty = update.dirty;
                    worktree.added = update.added;
                    worktree.modified = update.modified;
                    worktree.deleted = update.deleted;
                    worktree.untracked = update.untracked;
                    worktree.ahead = update.ahead;
                    worktree.behind = update.behind;
                    worktree.upstream.clone_from(&update.upstream);
                    worktree.upstream_gone = update.upstream_gone;
                    if missing {
                        worktree.missing = true;
                    }
                    found = true;
                }
            }
        }
        if found {
            self.rebuild();
        }
        found
    }

    fn apply_owner(&mut self, update: owner::OwnerResolved) -> bool {
        self.owners.insert(update.key, update.kind);
        if let Some(path) = &self.owner_cache {
            let _ = owner::save(path, &self.owners);
        }
        self.rebuild();
        true
    }

    fn status_line(&self) -> String {
        if let Some(notice) = &self.notice {
            return notice.clone();
        }
        let mut head = view::describe(self).unwrap_or_else(|| path_label(&self.snapshot.root));
        let total = self.measuring.len();
        let done = self
            .measuring
            .iter()
            .filter(|path| self.fresh.contains(*path))
            .count();
        if done < total {
            head.push_str(&format!("  ·  measuring sizes, {done} of {total} done"));
        }
        with_warnings(&head, &self.warnings())
    }

    /// Warnings worth a line. Deleted checkouts already show in the list.
    fn warnings(&self) -> Vec<String> {
        self.snapshot
            .warnings
            .iter()
            .filter(|warning| !warning.ends_with(": missing checkout"))
            .cloned()
            .collect()
    }

    fn record_warnings(&mut self) {
        let Some(path) = log_file() else {
            return;
        };
        let pending: Vec<String> = self
            .snapshot
            .warnings
            .iter()
            .filter(|warning| !self.logged.contains(*warning))
            .cloned()
            .collect();
        for warning in pending {
            self.logged.insert(warning.clone());
            append_log_to(&path, &warning);
        }
    }

    fn note_error(&mut self, warning: &str) {
        let warning = one_line(warning);
        if warning.is_empty() || !self.logged.insert(warning.clone()) {
            return;
        }
        if let Some(path) = log_file() {
            append_log_to(&path, &warning);
        }
    }

    fn rebuild(&mut self) {
        let selected = self.selected_path();
        // A project row follows its project: its first checkout changes as
        // the list is sorted or a worktree is added.
        let project = self
            .rows
            .get(self.cursor)
            .filter(|row| matches!(row.info, Info::Group(_)) && !row.project_id.is_empty())
            .map(|row| (row.project_id.clone(), row.depth));
        self.rows = rows::visible_rows(self, now_secs());
        let same_project = |row: &Row| {
            project.as_ref().is_some_and(|(id, depth)| {
                matches!(row.info, Info::Group(_)) && &row.project_id == id && row.depth == *depth
            })
        };
        self.cursor = self
            .rows
            .iter()
            .position(|row| row.path.is_some() && same_project(row))
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| row.path.is_some() && row.path == selected)
            })
            .or_else(|| self.rows.iter().position(|row| row.path.is_some()))
            .unwrap_or(0);
    }

    /// Select a checkout, opening its project when the list does not show it.
    fn reveal(&mut self, path: &Path) {
        let shown = |model: &Self| {
            model.rows.iter().position(|row| {
                row.path.as_deref() == Some(path) && matches!(row.info, Info::Checkout(_))
            })
        };
        if shown(self).is_none()
            && let Some(project) = self.project_id_for(path)
        {
            self.filtering = false;
            self.filter.clear();
            self.view = View::Detail { project };
            self.rebuild();
        }
        if let Some(index) = shown(self) {
            self.cursor = index;
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.rows.get(self.cursor).and_then(|row| row.path.clone())
    }

    fn project_id_for(&self, path: &Path) -> Option<String> {
        self.snapshot
            .projects
            .iter()
            .find(|project| {
                project
                    .worktrees
                    .iter()
                    .any(|worktree| worktree.path == path)
            })
            .map(|project| project.id.clone())
    }

    fn selected_project(&self) -> Option<&Project> {
        let id = match &self.view {
            View::Detail { project } => project.as_str(),
            View::List => self.rows.get(self.cursor)?.project_id.as_str(),
        };
        self.snapshot.projects.iter().find(|item| item.id == id)
    }

    /// The selected checkout row and its project.
    fn selected_worktree(&self) -> Option<(&Project, &WorktreeEntry)> {
        let row = self.rows.get(self.cursor)?;
        if !matches!(row.info, Info::Checkout(_)) {
            return None;
        }
        let path = row.path.as_ref()?;
        self.snapshot.projects.iter().find_map(|project| {
            project
                .worktrees
                .iter()
                .find(|worktree| &worktree.path == path)
                .map(|worktree| (project, worktree))
        })
    }

    /// Where checkout commands act, and whether its directory is gone: the
    /// selected checkout, or a project's main checkout on the home list.
    fn checkout_path(&self) -> Option<(PathBuf, bool)> {
        if let Some((_, worktree)) = self.selected_worktree() {
            return Some((worktree.path.clone(), worktree.missing));
        }
        let project = self.selected_project()?;
        let missing = project
            .worktrees
            .iter()
            .find(|worktree| worktree.path == project.path)
            .is_some_and(|worktree| worktree.missing);
        Some((project.path.clone(), missing))
    }

    /// Open the selected project. Inside a project, cd into the checkout.
    fn open(&mut self) -> Effect {
        if matches!(self.view, View::Detail { .. }) {
            return self.command(Cmd::Cd, "");
        }
        self.go_in();
        Effect::None
    }

    /// Open the selected project. Tab, `l`, and Ctrl-I do this.
    fn go_in(&mut self) {
        self.filtering = false;
        if matches!(self.view, View::List)
            && let Some(path) = self.selected_path()
            && let Some(project) = self.project_id_for(&path)
        {
            self.view = View::Detail { project };
            self.rebuild();
        }
    }

    /// Leave one level: the command line, the output, a search, a project.
    fn go_up(&mut self) {
        self.pending_key = None;
        self.count = None;
        if self.palette.is_some() {
            self.palette = None;
            return;
        }
        if self.pane.as_ref().is_some_and(|pane| !pane.running()) {
            self.pane = None;
            return;
        }
        if self.filtering || !self.filter.is_empty() {
            self.filtering = false;
            self.filter.clear();
            self.rebuild();
        } else if matches!(self.view, View::Detail { .. }) {
            self.view = View::List;
            self.rebuild();
        }
    }

    fn selectable(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.path.is_some().then_some(index))
            .collect()
    }

    /// Full page, or half a page for Ctrl-D and Ctrl-U. Before the first draw,
    /// a page is ten rows.
    fn page_delta(&self, half: bool) -> isize {
        let visible = usize::from(self.viewport);
        let span = if visible == 0 { 10 } else { visible.max(1) };
        let span = if half { span / 2 } else { span };
        isize::try_from(span.max(1)).unwrap_or(1)
    }

    fn move_selection(&mut self, delta: isize) {
        let selectable = self.selectable();
        if selectable.is_empty() {
            self.cursor = 0;
            return;
        }
        let position = selectable
            .iter()
            .position(|index| *index == self.cursor)
            .unwrap_or(0);
        let next = position
            .saturating_add_signed(delta)
            .min(selectable.len() - 1);
        self.cursor = selectable[next];
    }

    /// Move by screen rows, then land on a checkout. Half-page and page keys
    /// use this so a tree's group headers still count toward the page.
    fn move_rows(&mut self, delta: isize) {
        let selectable = self.selectable();
        let Some(fallback) = selectable.first().copied() else {
            self.cursor = 0;
            return;
        };
        let origin = if selectable.contains(&self.cursor) {
            self.cursor
        } else {
            fallback
        };
        if delta == 0 || self.rows.is_empty() {
            self.cursor = origin;
            return;
        }
        let target = origin.saturating_add_signed(delta).min(self.rows.len() - 1);
        self.cursor = self.nearest(target, delta > 0).unwrap_or(origin);
    }

    /// The selectable row at or after `target` (or before it), falling back
    /// to the other direction.
    fn nearest(&self, target: usize, forward: bool) -> Option<usize> {
        let selectable = self.selectable();
        let after = selectable.iter().copied().find(|index| *index >= target);
        let before = selectable
            .iter()
            .copied()
            .rev()
            .find(|index| *index <= target);
        if forward {
            after.or(before)
        } else {
            before.or(after)
        }
    }

    /// The nth selectable row (1-based), or the first or last one.
    fn jump_to(&mut self, nth: Option<usize>, last: bool) {
        let selectable = self.selectable();
        if selectable.is_empty() {
            return;
        }
        let index = match nth {
            Some(nth) => nth.saturating_sub(1).min(selectable.len() - 1),
            None if last => selectable.len() - 1,
            None => 0,
        };
        self.cursor = selectable[index];
    }

    /// Height of the visible window and the scrolloff margin inside it.
    fn window(&self) -> (usize, usize) {
        let height = usize::from(self.viewport).min(self.rows.len());
        (height, self.scrolloff.min(height.saturating_sub(1) / 2))
    }

    /// Rows the cursor may occupy without scrolling, as vim's scrolloff
    /// allows: the margin applies except at either end of the list.
    fn cursor_band(&self, top: usize) -> Option<(usize, usize)> {
        let (height, margin) = self.window();
        if height == 0 {
            return None;
        }
        let max_top = self.rows.len() - height;
        let low = if top == 0 { 0 } else { top + margin };
        let high = if top >= max_top {
            self.rows.len() - 1
        } else {
            top + height - 1 - margin
        };
        Some((low, high.max(low)))
    }

    /// Ctrl-E and Ctrl-Y: move the window, and the cursor only when it would
    /// leave it.
    fn scroll_view(&mut self, delta: isize) {
        let (height, _) = self.window();
        if height == 0 || self.rows.len() <= height {
            return;
        }
        let max_top = self.rows.len() - height;
        self.scroll = self.scroll.saturating_add_signed(delta).min(max_top);
        let Some((low, high)) = self.cursor_band(self.scroll) else {
            return;
        };
        if self.cursor < low {
            self.cursor = self.nearest(low, true).unwrap_or(self.cursor);
        } else if self.cursor > high {
            self.cursor = self.nearest(high, false).unwrap_or(self.cursor);
        }
    }

    /// H, M, and L.
    fn screen_jump(&mut self, place: Place) {
        let Some((low, high)) = self.cursor_band(self.scroll) else {
            return;
        };
        let (height, _) = self.window();
        let target = match place {
            Place::Top => low,
            Place::Middle => (self.scroll + height.saturating_sub(1) / 2).clamp(low, high),
            Place::Bottom => high,
        };
        let forward = !matches!(place, Place::Bottom);
        if let Some(index) = self.nearest(target, forward) {
            self.cursor = index;
        }
    }

    /// zt, zz, and zb.
    fn align(&mut self, place: Place) -> Effect {
        let (height, margin) = self.window();
        if height == 0 {
            return Effect::None;
        }
        let max_top = self.rows.len() - height;
        let top = match place {
            Place::Top => self.cursor.saturating_sub(margin),
            Place::Middle => self.cursor.saturating_sub(height / 2),
            Place::Bottom => (self.cursor + margin + 1).saturating_sub(height),
        };
        self.scroll = top.min(max_top);
        Effect::None
    }

    /// `{` and `}`: the first checkout of the previous or next project.
    fn jump_group(&mut self, delta: isize) {
        let selectable = self.selectable();
        let starts: Vec<usize> = selectable
            .iter()
            .enumerate()
            .filter(|(position, index)| {
                *position == 0
                    || self.rows[**index].project_id
                        != self.rows[selectable[position - 1]].project_id
            })
            .map(|(_, index)| *index)
            .collect();
        if starts.is_empty() {
            return;
        }
        let current = starts
            .iter()
            .rposition(|start| *start <= self.cursor)
            .unwrap_or(0);
        let target = if delta > 0 {
            current.saturating_add(delta.unsigned_abs())
        } else if starts[current] < self.cursor {
            current.saturating_sub(delta.unsigned_abs() - 1)
        } else {
            current.saturating_sub(delta.unsigned_abs())
        };
        match starts.get(target) {
            Some(index) => self.cursor = *index,
            None => self.jump_to(None, true),
        }
    }

    fn move_palette(&mut self, delta: isize) {
        let len = palette_matches(self).len();
        let Some(palette) = &mut self.palette else {
            return;
        };
        if len == 0 {
            palette.cursor = 0;
            return;
        }
        palette.cursor = palette.cursor.saturating_add_signed(delta).min(len - 1);
    }

    fn clamp_palette(&mut self) {
        let len = palette_matches(self).len();
        let Some(palette) = &mut self.palette else {
            return;
        };
        palette.cursor = if len == 0 {
            0
        } else {
            palette.cursor.min(len - 1)
        };
    }

    fn selected_command(&self) -> Option<Cmd> {
        palette_matches(self)
            .get(self.palette.as_ref()?.cursor)
            .map(|spec| spec.cmd)
    }
}

/// The parts of a `bonsai clean --dry-run --json` report hq shows.
struct CleanPlan {
    planned: Vec<PlannedRemoval>,
    dirty: usize,
    missing: usize,
}

#[derive(serde::Deserialize)]
struct PlannedRemoval {
    branch: String,
    reason: String,
    path: PathBuf,
}

impl CleanPlan {
    fn parse(report: &str) -> Result<Self, serde_json::Error> {
        #[derive(serde::Deserialize)]
        struct Report {
            #[serde(default)]
            planned: Vec<PlannedRemoval>,
            #[serde(default)]
            skipped_dirty: Vec<serde_json::Value>,
            #[serde(default)]
            skipped_missing: Vec<serde_json::Value>,
        }
        let report: Report = serde_json::from_str(report)?;
        Ok(Self {
            planned: report.planned,
            dirty: report.skipped_dirty.len(),
            missing: report.skipped_missing.len(),
        })
    }

    fn paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .planned
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        paths.sort();
        paths
    }
}

fn commits(count: usize) -> String {
    if count == 1 {
        "1 commit is".to_string()
    } else {
        format!("{count} commits are")
    }
}

/// Commands matching the first word of the command line: exact names first,
/// then prefixes, then fuzzy matches of the name, then words of the
/// description.
fn palette_matches(model: &Model) -> Vec<&'static CommandSpec> {
    let Some(palette) = &model.palette else {
        return Vec::new();
    };
    let word = palette
        .query
        .text()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if word.is_empty() {
        return COMMANDS.iter().collect();
    }
    let rank = |spec: &CommandSpec| -> Option<u8> {
        let names = || std::iter::once(spec.name).chain(spec.aliases.iter().copied());
        if names().any(|name| name == word) {
            Some(0)
        } else if names().any(|name| name.starts_with(&word)) {
            Some(1)
        } else if names().any(|name| picker::fuzzy_match(name, &word)) {
            Some(2)
        } else if word.chars().count() >= 3 && spec.hint.to_lowercase().contains(&word) {
            Some(3)
        } else {
            None
        }
    };
    let mut ranked: Vec<(u8, &CommandSpec)> = COMMANDS
        .iter()
        .filter_map(|spec| rank(spec).map(|rank| (rank, spec)))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, spec)| spec).collect()
}

fn path_label(path: &Path) -> String {
    if let Some(home) = std::env::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

fn with_warnings(head: &str, warnings: &[String]) -> String {
    match warnings {
        [] => head.to_string(),
        [warning] => format!("{head}  {}", truncate(warning, 96)),
        many => format!("{head}  {} warnings · {}", many.len(), log_label()),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `_BONSAI_HQ_LOG` overrides the file. `0` or empty disables it. Unit tests
/// stay quiet unless that variable is set, so they do not write the user's
/// state directory. The JSON command never reaches this path.
fn log_file() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("_BONSAI_HQ_LOG") {
        if value.is_empty() || value == "0" {
            return None;
        }
        return Some(PathBuf::from(value));
    }
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        Some(default_log_path())
    }
}

fn default_log_path() -> PathBuf {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state).join("bonsai/hq.log");
    }
    if let Some(home) = std::env::home_dir() {
        return home.join(".local/state/bonsai/hq.log");
    }
    PathBuf::from("/tmp/bonsai/hq.log")
}

fn log_label() -> String {
    match std::env::var("_BONSAI_HQ_LOG") {
        Ok(value) if !value.is_empty() && value != "0" => path_label(Path::new(&value)),
        _ => path_label(&default_log_path()),
    }
}

fn append_log_to(path: &Path, warning: &str) {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{} {}", now_secs(), one_line(warning));
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or(0)
}

fn truncate(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let keep = width.saturating_sub(1);
    let mut shortened: String = text.chars().take(keep).collect();
    if width > 1 {
        shortened.push('…');
    }
    shortened
}

fn pad(mut text: String, width: usize) -> String {
    let extra = width.saturating_sub(text.chars().count());
    text.extend(std::iter::repeat_n(' ', extra));
    text
}

#[cfg(test)]
mod tests {
    use super::rows::{Depth, TREE_RAIL, child_prefix};
    use super::table::{self, Col, Kind};
    use super::*;
    use ratatui::style::Modifier;

    fn paint(model: &mut Model, width: u16, height: u16) -> (String, ratatui::buffer::Buffer) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| view::draw(frame, model)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (screen, buffer)
    }

    fn painted(model: &mut Model) -> (String, ratatui::buffer::Buffer) {
        paint(model, 100, 24)
    }

    fn row_index(screen: &str, needle: &str) -> u16 {
        u16::try_from(
            screen
                .lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle}")),
        )
        .expect("row")
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn typed(model: &mut Model, text: &str) {
        for ch in text.chars() {
            model.on_key(press(KeyCode::Char(ch)));
        }
    }

    fn span_style(line: &Line<'_>, needle: &str) -> Style {
        line.spans
            .iter()
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle}"))
            .style
    }

    /// Every cell of a row, as the words a reader sees.
    fn words(row: &Row) -> String {
        [
            Col::Checkouts,
            Col::Changes,
            Col::Upstream,
            Col::Size,
            Col::Deps,
            Col::Stack,
            Col::Agents,
            Col::Notes,
        ]
        .into_iter()
        .map(|col| {
            table::cell(row, col)
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
    }

    fn row_for<'a>(model: &'a Model, suffix: &str) -> &'a Row {
        model
            .rows
            .iter()
            .find(|row| {
                row.path.as_ref().is_some_and(|path| path.ends_with(suffix))
                    && matches!(row.info, Info::Checkout(_))
            })
            .unwrap_or_else(|| panic!("no checkout row for {suffix}"))
    }

    fn worktree(branch: &str, path: &str, main: bool, dirty: Option<bool>) -> WorktreeEntry {
        WorktreeEntry {
            path: path.into(),
            branch: Some(branch.to_string()),
            head: "abc".to_string(),
            main,
            external: false,
            locked: false,
            prunable: false,
            dirty,
            added: 0,
            modified: 0,
            deleted: 0,
            untracked: usize::from(dirty == Some(true)),
            ahead: 0,
            behind: 0,
            upstream: None,
            upstream_gone: false,
            missing: false,
            last_activity: None,
        }
    }

    fn sample() -> Snapshot {
        Snapshot {
            root: "/trees".into(),
            warnings: vec!["one checkout is unreadable".to_string()],
            projects: vec![
                Project {
                    id: "bonsai".to_string(),
                    name: "bonsai".to_string(),
                    path: "/trees/bonsai".into(),
                    remote: Some("git@github.com:aymericbeaumet/bonsai.git".to_string()),
                    worktrees: vec![
                        worktree("ab/dev", "/trees/bonsai/ab/dev", false, Some(true)),
                        worktree("main", "/trees/bonsai/main", true, Some(false)),
                    ],
                },
                Project {
                    id: "other".to_string(),
                    name: "other".to_string(),
                    path: "/trees/other".into(),
                    remote: Some("git@github.com:interfaceinc/other.git".to_string()),
                    worktrees: vec![worktree(
                        "feature",
                        "/trees/other/feature",
                        true,
                        Some(false),
                    )],
                },
            ],
        }
    }

    fn many(count: usize) -> Model {
        let projects = (0..count)
            .map(|index| {
                let name = format!("p{index:02}");
                Project {
                    id: name.clone(),
                    name: name.clone(),
                    path: format!("/trees/{name}").into(),
                    remote: Some(format!("git@github.com:user/{name}.git")),
                    worktrees: vec![worktree(
                        "main",
                        &format!("/trees/{name}/main"),
                        true,
                        Some(false),
                    )],
                }
            })
            .collect();
        Model::from_snapshot(Snapshot {
            root: "/trees".into(),
            warnings: Vec::new(),
            projects,
        })
    }

    fn selected(model: &Model) -> String {
        model.rows[model.cursor].project.clone()
    }

    fn sized(bytes: u64) -> footprint::Footprint {
        footprint::Footprint {
            ecosystems: vec!["rust".to_string()],
            bytes,
            dependencies: vec![footprint::Dependency {
                name: "target".to_string(),
                bytes: Some(bytes / 2),
                sharing: footprint::Sharing::Local,
            }],
            harnesses: vec!["claude".to_string(), "grok".to_string()],
        }
    }

    fn measured(model: &Model, path: &str, bytes: u64, complete: bool) -> footprint::Measured {
        footprint::Measured {
            generation: model.generation,
            path: path.into(),
            footprint: sized(bytes),
            complete,
        }
    }

    fn background(effect: Effect) -> jobs::Spec {
        match effect {
            Effect::Run(Action::Background { spec, .. }) => spec,
            other => panic!("expected a background command, got {other:?}"),
        }
    }

    fn arguments(spec: &jobs::Spec) -> Vec<String> {
        spec.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn enter_and_o_open_a_project_then_cd_into_a_checkout() {
        let mut model = Model::from_snapshot(sample());
        assert!(model.selected_path().unwrap().ends_with("dev"));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('o'))),
            Effect::None
        ));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        assert!(model.rows.iter().any(|row| row.title.contains("ab/dev")));
        assert!(model.rows.iter().any(|row| row.title == "main"));
        model.on_key(press(KeyCode::Esc));
        assert!(matches!(model.view, View::List));
        assert!(matches!(model.on_key(press(KeyCode::Enter)), Effect::None));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(press(KeyCode::Char('j')));
        assert!(matches!(
            model.on_key(press(KeyCode::Enter)),
            Effect::Cd(path) if path.ends_with("bonsai/main")
        ));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('o'))),
            Effect::Cd(path) if path.ends_with("bonsai/main")
        ));
        model.on_key(press(KeyCode::Esc));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('c'))),
            Effect::Cd(path) if path == Path::new("/trees/bonsai")
        ));
        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('o')));
        assert_eq!(model.filter.text(), "o");
        assert!(matches!(model.view, View::List));
    }

    #[test]
    fn h_and_l_go_up_and_in_like_a_file_manager() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('l')));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(press(KeyCode::Char('h')));
        assert!(matches!(model.view, View::List));
        model.on_key(press(KeyCode::Right));
        assert!(matches!(model.view, View::Detail { .. }));
        model.on_key(press(KeyCode::Left));
        assert!(matches!(model.view, View::List));
    }

    #[test]
    fn ctrl_i_goes_into_the_project_and_ctrl_o_and_escape_go_up() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(ctrl('i'));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(ctrl('o'));
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Tab));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(press(KeyCode::Esc));
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('d')));
        model.on_key(ctrl('o'));
        assert!(model.filter.is_empty());
        assert!(!model.filtering);
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Enter));
        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('m')));
        assert!(matches!(model.view, View::Detail { .. }));
        model.on_key(press(KeyCode::Enter));
        assert!(!model.filtering);
        assert_eq!(model.filter.text(), "m");
        model.on_key(press(KeyCode::Esc));
        assert!(model.filter.is_empty());
        assert!(matches!(model.view, View::Detail { .. }));
        model.on_key(ctrl('o'));
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Char(':')));
        assert!(model.palette.is_some());
        model.on_key(ctrl('o'));
        assert!(model.palette.is_none());
        model.on_key(press(KeyCode::Char(':')));
        model.on_key(press(KeyCode::Esc));
        assert!(model.palette.is_none());
    }

    #[test]
    fn slash_filters_without_treating_q_as_quit() {
        let mut model = Model::from_snapshot(sample());
        assert!(matches!(
            model.on_key(press(KeyCode::Char('/'))),
            Effect::None
        ));
        typed(&mut model, "dev");
        assert_eq!(model.filter.text(), "dev");
        assert!(model.rows.iter().any(|row| row.title.contains("ab/dev")));
        assert!(
            !model
                .rows
                .iter()
                .any(|row| row.path.as_ref().is_some_and(|path| path.ends_with("main")))
        );
        assert!(matches!(
            model.on_key(press(KeyCode::Char('q'))),
            Effect::None
        ));
        assert!(model.filter.text().ends_with('q'));
        assert!(matches!(model.on_key(press(KeyCode::Esc)), Effect::None));
        assert!(model.filter.is_empty());
        assert!(!model.filtering);
        assert!(matches!(
            model.on_key(press(KeyCode::Char('q'))),
            Effect::Quit
        ));
    }

    #[test]
    fn vim_motions_take_counts_and_pages() {
        let mut model = many(20);
        model.viewport = 10;
        assert_eq!(selected(&model), "p00");

        model.on_key(press(KeyCode::Down));
        model.on_key(press(KeyCode::Char('j')));
        assert_eq!(selected(&model), "p02");
        model.on_key(press(KeyCode::Up));
        model.on_key(press(KeyCode::Char('k')));
        assert_eq!(selected(&model), "p00");
        typed(&mut model, "5j");
        assert_eq!(selected(&model), "p05");
        typed(&mut model, "12k");
        assert_eq!(selected(&model), "p00");
        model.on_key(ctrl('n'));
        model.on_key(ctrl('p'));
        assert_eq!(selected(&model), "p00");
        model.on_key(ctrl('d'));
        assert_eq!(selected(&model), "p05");
        model.on_key(ctrl('u'));
        assert_eq!(selected(&model), "p00");
        model.on_key(ctrl('f'));
        assert_eq!(selected(&model), "p10");
        model.on_key(ctrl('b'));
        assert_eq!(selected(&model), "p00");
        model.on_key(press(KeyCode::Char('G')));
        assert_eq!(selected(&model), "p19");
        model.on_key(press(KeyCode::Char('g')));
        assert_eq!(selected(&model), "p19");
        model.on_key(press(KeyCode::Char('g')));
        assert_eq!(selected(&model), "p00");
        typed(&mut model, "3G");
        assert_eq!(selected(&model), "p02");
        typed(&mut model, "7gg");
        assert_eq!(selected(&model), "p06");
        // A second key that does not complete `gg` cancels it.
        typed(&mut model, "gj");
        assert_eq!(selected(&model), "p07");
        model.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT));
        assert_eq!(selected(&model), "p19");
        // A lone zero is not a count.
        typed(&mut model, "0k");
        assert_eq!(selected(&model), "p18");

        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "p1");
        model.on_key(ctrl('e'));
        model.on_key(ctrl('d'));
        assert_eq!(model.filter.text(), "p1");
        assert!(model.filtering);
        model.on_key(ctrl('a'));
        model.on_key(ctrl('d'));
        assert_eq!(model.filter.text(), "1");
        model.on_key(press(KeyCode::Left));
        assert_eq!(model.filter.text(), "1");
        typed(&mut model, "gG5");
        assert_eq!(model.filter.text(), "gG51");
        assert!(model.filtering);
    }

    #[test]
    fn screen_motions_and_scrolling_follow_vim() {
        let mut model = many(40);
        model.scrolloff = 2;
        let (_, _) = paint(&mut model, 80, 14);
        let height = usize::from(model.viewport);
        assert!(height >= 6, "viewport {height}");
        assert_eq!(model.scroll, 0);

        // Ctrl-E scrolls the list; the cursor stays until the margin pushes it.
        model.on_key(ctrl('e'));
        assert_eq!(model.scroll, 1);
        assert_eq!(selected(&model), "p03");
        model.on_key(ctrl('y'));
        assert_eq!(model.scroll, 0);
        assert_eq!(selected(&model), "p03");

        model.on_key(press(KeyCode::Char('L')));
        assert_eq!(model.cursor, height - 1 - 2);
        model.on_key(press(KeyCode::Char('H')));
        assert_eq!(selected(&model), "p00");
        model.on_key(press(KeyCode::Char('M')));
        assert_eq!(model.cursor, (height - 1) / 2);

        typed(&mut model, "20G");
        let (_, _) = paint(&mut model, 80, 14);
        typed(&mut model, "zt");
        assert_eq!(model.scroll, 19 - 2);
        typed(&mut model, "zb");
        assert_eq!(model.scroll, 19 + 2 + 1 - height);
        typed(&mut model, "zz");
        assert_eq!(model.scroll, 19 - height / 2);
        let (screen, _) = paint(&mut model, 80, 14);
        assert!(screen.contains("p19"));
        assert_eq!(selected(&model), "p19");
    }

    #[test]
    fn braces_jump_between_projects() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "trees");
        model.on_key(press(KeyCode::Esc));
        model.filter.set("trees");
        model.rebuild();
        assert_eq!(model.rows.len(), 3);
        assert!(model.selected_path().unwrap().ends_with("ab/dev"));
        model.on_key(press(KeyCode::Char('}')));
        assert!(model.selected_path().unwrap().ends_with("other/feature"));
        model.on_key(press(KeyCode::Char('{')));
        assert!(model.selected_path().unwrap().ends_with("ab/dev"));
        model.on_key(press(KeyCode::Char('j')));
        model.on_key(press(KeyCode::Char('{')));
        assert!(model.selected_path().unwrap().ends_with("ab/dev"));
    }

    #[test]
    fn gg_and_half_page_move_through_the_tree() {
        let mut model = many(20);
        model.on_key(press(KeyCode::Char('t')));
        model.viewport = 10;
        let path = |model: &Model| model.selected_path().unwrap();
        assert!(path(&model).ends_with("p00/main"));

        model.on_key(ctrl('d'));
        assert!(path(&model).ends_with("p03/main"));
        model.on_key(ctrl('u'));
        assert!(path(&model).ends_with("p00/main"));
        model.on_key(ctrl('D'));
        assert!(path(&model).ends_with("p03/main"));
        model.on_key(press(KeyCode::Char('G')));
        assert!(path(&model).ends_with("p19/main"));
        model.on_key(press(KeyCode::Char('g')));
        model.on_key(press(KeyCode::Char('g')));
        assert!(path(&model).ends_with("p00/main"));
    }

    #[test]
    fn scroll_keeps_a_margin_until_the_ends() {
        // A move inside the window leaves the top where it is.
        assert_eq!(scroll_top(10, 25, 30, 100, 10), 10);
        // The next row toward the bottom scrolls by one.
        assert_eq!(scroll_top(0, 19, 30, 100, 10), 0);
        assert_eq!(scroll_top(0, 20, 30, 100, 10), 1);
        assert_eq!(scroll_top(10, 29, 30, 100, 10), 10);
        assert_eq!(scroll_top(10, 30, 30, 100, 10), 11);
        // Moving up does the same.
        assert_eq!(scroll_top(10, 20, 30, 100, 10), 10);
        assert_eq!(scroll_top(10, 19, 30, 100, 10), 9);
        // The cursor may sit on the first and last rows.
        assert_eq!(scroll_top(0, 0, 30, 100, 10), 0);
        assert_eq!(scroll_top(0, 39, 30, 40, 10), 10);
        // A short window caps the margin so the cursor can still move.
        assert_eq!(scroll_top(0, 2, 5, 20, 10), 0);
        assert_eq!(scroll_top(0, 3, 5, 20, 10), 1);
        // Zero scrolls only when the cursor would leave the window.
        assert_eq!(scroll_top(0, 29, 30, 100, 0), 0);
        assert_eq!(scroll_top(0, 30, 30, 100, 0), 1);
        assert_eq!(scroll_top(5, 5, 30, 100, 0), 5);
        assert_eq!(scroll_top(5, 4, 30, 100, 0), 4);
        assert_eq!(scroll_top(3, 1, 0, 10, 10), 0);
        assert_eq!(scroll_top(3, 0, 10, 0, 10), 0);
    }

    #[test]
    fn the_cursor_moves_until_it_reaches_the_margin() {
        let mut model = many(40);
        let (screen, _) = paint(&mut model, 80, 40);
        let height = usize::from(model.viewport);
        let margin = model.scrolloff.min(height.saturating_sub(1) / 2);
        assert_eq!(model.scrolloff, 10);
        assert_eq!(margin, 10);
        assert_eq!(model.scroll, 0);
        assert!(screen.contains("p00"));

        let last_safe = height - 1 - margin;
        for _ in 0..last_safe {
            model.on_key(press(KeyCode::Char('j')));
        }
        let (screen, _) = paint(&mut model, 80, 40);
        assert_eq!(selected(&model), format!("p{last_safe:02}"));
        assert_eq!(model.scroll, 0);
        assert!(screen.contains("p00"));

        model.on_key(press(KeyCode::Char('j')));
        let (screen, _) = paint(&mut model, 80, 40);
        assert!(model.scroll > 0);
        assert!(!screen.contains("p00"));

        model.on_key(press(KeyCode::Char('G')));
        let (screen, _) = paint(&mut model, 80, 40);
        assert!(screen.contains("p39"));
        assert!(!screen.contains("p00"));

        model.on_key(press(KeyCode::Char('g')));
        model.on_key(press(KeyCode::Char('g')));
        let (screen, _) = paint(&mut model, 80, 40);
        assert_eq!(model.scroll, 0);
        assert!(screen.contains("p00"));
        assert_eq!(selected(&model), "p00");
    }

    #[test]
    fn tree_labels_use_the_table_colors() {
        let mut model = Model::from_snapshot(sample());
        let width = 120;
        let home_layout = table::layout(Kind::Projects, &model.rows, width);
        let home = table::line(
            model
                .rows
                .iter()
                .find(|row| row.project == "bonsai")
                .unwrap(),
            &home_layout,
            width,
        );
        model.on_key(press(KeyCode::Char('t')));
        let line = |model: &Model, pred: fn(&Row) -> bool| {
            let layout = table::layout(Kind::Tree, &model.rows, width);
            table::line(
                model.rows.iter().find(|row| pred(row)).unwrap(),
                &layout,
                width,
            )
        };
        let host = line(&model, |row| row.depth == Depth::Host);
        let owner = line(&model, |row| {
            row.depth == Depth::Owner && row.owner == "aymericbeaumet"
        });
        let repo = line(&model, |row| {
            row.depth == Depth::Repo && row.project == "bonsai"
        });
        let dev = line(&model, |row| row.title == "ab/dev");
        let main = line(&model, |row| {
            row.depth == Depth::Branch && row.title == "main"
        });
        assert_eq!(
            span_style(&host, "github.com"),
            span_style(&home, "github.com")
        );
        assert_eq!(
            span_style(&owner, "aymericbeaumet"),
            span_style(&home, "aymericbeaumet")
        );
        assert_eq!(span_style(&repo, "bonsai"), span_style(&home, "bonsai"));
        assert_eq!(
            span_style(&owner, "├"),
            span_style(&owner, "aymericbeaumet")
        );
        assert_eq!(span_style(&repo, "└"), span_style(&repo, "bonsai"));
        assert_eq!(span_style(&dev, "untracked"), span_style(&home, "changed"));
        assert_eq!(span_style(&home, "github.com").fg, Some(Color::LightBlue));
        assert_eq!(
            span_style(&home, "aymericbeaumet").fg,
            Some(Color::LightMagenta)
        );
        assert_eq!(span_style(&home, "bonsai").fg, Some(Color::LightGreen));
        let header = table::header(&home_layout, width);
        assert_eq!(span_style(&header, "host").fg, Some(Color::LightBlue));
        assert_eq!(span_style(&header, "owner").fg, Some(Color::LightMagenta));
        assert_eq!(span_style(&header, "project").fg, Some(Color::LightGreen));

        model.on_key(press(KeyCode::Enter));
        let layout = table::layout(Kind::Checkouts, &model.rows, width);
        let detail = |title: &str| {
            table::line(
                model.rows.iter().find(|row| row.title == title).unwrap(),
                &layout,
                width,
            )
        };
        assert_eq!(
            span_style(&dev, "ab/dev"),
            span_style(&detail("ab/dev"), "ab/dev")
        );
        assert_eq!(
            span_style(&main, "main"),
            span_style(&detail("main"), "main")
        );
        assert_eq!(span_style(&dev, "├"), span_style(&dev, "ab/dev"));
        assert_eq!(span_style(&main, "└"), span_style(&main, "main"));
    }

    #[test]
    fn readline_edits_the_filter_and_the_command_line() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "ab dev");
        model.on_key(ctrl('w'));
        assert_eq!(model.filter.text(), "ab ");
        model.on_key(ctrl('b'));
        model.on_key(ctrl('k'));
        assert_eq!(model.filter.text(), "ab");
        model.on_key(press(KeyCode::Esc));

        model.on_key(press(KeyCode::Char(':')));
        typed(&mut model, "pru extra");
        model.on_key(ctrl('w'));
        assert!(matches!(
            model.on_key(press(KeyCode::Enter)),
            Effect::Run(Action::Foreground(task)) if task.label == "bonsai prune" && task.pause
        ));
    }

    #[test]
    fn the_command_line_ranks_names_takes_arguments_and_completes() {
        let mut model = Model::from_snapshot(sample());
        let first = |model: &mut Model, text: &str| {
            model.open_palette(text);
            palette_matches(model)[0].cmd
        };
        assert_eq!(first(&mut model, "rm"), Cmd::Remove);
        assert_eq!(first(&mut model, "q"), Cmd::Quit);
        assert_eq!(first(&mut model, "pr"), Cmd::Prune);
        assert_eq!(first(&mut model, "merged"), Cmd::Clean);
        model.open_palette("pru");
        model.on_key(press(KeyCode::Tab));
        assert_eq!(model.palette.as_ref().unwrap().query.text(), "prune ");
        typed(&mut model, "all");
        assert!(matches!(
            model.on_key(press(KeyCode::Enter)),
            Effect::Run(Action::Foreground(task)) if task.label == "bonsai prune -a" && task.project.is_none()
        ));
        model.open_palette("sort size");
        model.on_key(press(KeyCode::Enter));
        assert_eq!(model.sort, Sort::Size);
        model.open_palette("zzz");
        assert!(palette_matches(&model).is_empty());
        assert!(matches!(model.on_key(press(KeyCode::Enter)), Effect::None));
        assert!(model.palette.is_some());
    }

    #[test]
    fn a_asks_for_a_branch_and_adds_it_to_the_selected_project() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('a')));
        assert_eq!(model.palette.as_ref().unwrap().query.text(), "add ");
        assert!(model.status_line().contains("branch name"));
        typed(&mut model, "feat-x");
        let spec = background(model.on_key(press(KeyCode::Enter)));
        assert_eq!(spec.after, jobs::After::SelectPrinted);
        assert_eq!(spec.dir, Path::new("/trees/bonsai"));
        assert_eq!(arguments(&spec)[2..], ["add", "feat-x"]);
        assert_eq!(arguments(&spec)[..2], ["-C", "/trees/bonsai"]);
    }

    #[test]
    fn dd_asks_before_removing_and_keeps_the_branch() {
        let mut model = Model::from_snapshot(sample());
        typed(&mut model, "dd");
        assert!(model.confirm.is_none());
        assert!(model.status_line().contains("select a checkout"));
        model.on_key(press(KeyCode::Enter));
        typed(&mut model, "dd");
        let confirm = model.confirm.as_ref().expect("a confirmation");
        assert_eq!(confirm.title, "Remove the ab/dev worktree?");
        let text: Vec<String> = confirm.lines.iter().map(ToString::to_string).collect();
        assert!(text.iter().any(|line| line.contains("refuse")), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("ab/dev is kept")),
            "{text:?}"
        );
        assert!(matches!(
            model.on_key(press(KeyCode::Char('n'))),
            Effect::None
        ));
        assert!(model.confirm.is_none());
        typed(&mut model, "dd");
        let spec = background(model.on_key(press(KeyCode::Char('y'))));
        assert_eq!(arguments(&spec)[2..], ["remove", "/trees/bonsai/ab/dev"]);
        assert_eq!(spec.label, "bonsai remove ab/dev");

        model.on_key(press(KeyCode::Char('j')));
        typed(&mut model, "dd");
        assert!(model.confirm.is_none());
        assert!(
            model
                .status_line()
                .contains("main checkout cannot be removed")
        );
    }

    #[test]
    fn delete_explains_when_bonsai_will_keep_the_branch() {
        let confirm_text = |configure: fn(&mut WorktreeEntry)| {
            let mut snapshot = sample();
            configure(&mut snapshot.projects[0].worktrees[0]);
            let mut model = Model::from_snapshot(snapshot);
            model.on_key(press(KeyCode::Enter));
            model.on_key(press(KeyCode::Char('D')));
            let confirm = model.confirm.as_ref().expect("a confirmation");
            assert_eq!(confirm.title, "Remove ab/dev and delete its branch?");
            confirm
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let unpushed = confirm_text(|worktree| {
            worktree.upstream = Some("origin/ab/dev".to_string());
            worktree.ahead = 3;
        });
        assert!(
            unpushed.contains("3 commits are not on origin/ab/dev, so bonsai will refuse"),
            "{unpushed}"
        );
        let pushed = confirm_text(|worktree| worktree.upstream = Some("origin/ab/dev".into()));
        assert!(pushed.contains("can be deleted"), "{pushed}");
        let gone = confirm_text(|worktree| {
            worktree.upstream = Some("origin/ab/dev".into());
            worktree.upstream_gone = true;
        });
        assert!(gone.contains("use C to clean"), "{gone}");
        let local = confirm_text(|_| {});
        assert!(local.contains("only if it is merged"), "{local}");

        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Enter));
        model.on_key(press(KeyCode::Char('D')));
        let spec = background(model.on_key(press(KeyCode::Char('y'))));
        assert_eq!(
            arguments(&spec)[2..],
            ["remove", "-d", "/trees/bonsai/ab/dev"]
        );
    }

    const PLAN: &str = r#"{"dry_run":true,"planned":[{"branch":"ab/dev","reason":"squash-merged","path":"/trees/bonsai/ab/dev"}],
        "skipped_dirty":[],"skipped_missing":[{"branch":"gone","reason":"merged","path":"/x"}],
        "removed":[],"deleted_branches":[],"failures":[],"recovery":null}"#;

    #[test]
    fn clean_shows_the_plan_and_checks_it_again_before_removing_anything() {
        let mut model = Model::from_snapshot(sample());
        let spec = background(model.on_key(press(KeyCode::Char('C'))));
        assert_eq!(arguments(&spec)[2..], ["clean", "--dry-run", "--json"]);
        assert!(matches!(spec.after, jobs::After::CleanPlan { .. }));

        let project = Path::new("/trees/bonsai");
        model.offer_clean_plan(project, PLAN);
        let confirm = model.confirm.as_ref().expect("a plan");
        assert_eq!(confirm.title, "Clean bonsai?");
        let text: Vec<String> = confirm.lines.iter().map(ToString::to_string).collect();
        assert!(
            text.iter()
                .any(|line| line.contains("ab/dev") && line.contains("squash-merged"))
        );
        assert!(
            text.iter()
                .any(|line| line.contains("1 whose directory was deleted"))
        );
        // Keys typed before the plan appeared do not answer it.
        assert!(matches!(
            model.on_key(press(KeyCode::Char('y'))),
            Effect::None
        ));
        assert!(model.confirm.is_some());
        model.confirm.as_mut().unwrap().ready_at = None;
        let spec = background(model.on_key(press(KeyCode::Char('y'))));
        assert_eq!(
            arguments(&spec)[2..],
            ["clean", "--dry-run", "--json", "--no-fetch"]
        );
        let jobs::After::CleanVerify { expected, .. } = &spec.after else {
            panic!("expected a verification, got {:?}", spec.after);
        };
        assert_eq!(expected, &[PathBuf::from("/trees/bonsai/ab/dev")]);

        // The fresh plan matches: clean runs.
        let Some(Action::Background { spec, .. }) = model.verified_clean(project, expected, PLAN)
        else {
            panic!("expected the clean to run");
        };
        assert_eq!(arguments(&spec)[2..], ["clean", "--yes", "--no-fetch"]);
        // It changed: show the new plan instead.
        let changed = PLAN
            .replace("ab/dev\"", "ab/other\"")
            .replace("ab/dev\"}", "ab/other\"}");
        let changed = changed.replace("/trees/bonsai/ab/dev", "/trees/bonsai/ab/other");
        assert!(model.verified_clean(project, expected, &changed).is_none());
        assert!(model.confirm.is_some());
        assert_eq!(
            model.status_line(),
            "the plan changed since you reviewed it"
        );

        model.confirm = None;
        model.open_clean_plan(
            project,
            r#"{"dry_run":true,"planned":[],"skipped_dirty":[{"branch":"x","reason":"merged","path":"/x"}],"skipped_missing":[]}"#,
            None,
        );
        assert!(model.confirm.is_none());
        assert_eq!(
            model.status_line(),
            "nothing to clean in bonsai; kept 1 with uncommitted changes"
        );
    }

    #[test]
    fn a_plan_that_arrives_while_typing_waits_for_c() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "b");
        model.offer_clean_plan(Path::new("/trees/bonsai"), PLAN);
        assert!(model.confirm.is_none());
        assert!(model.status_line().contains("press C to review it"));
        typed(&mut model, "y");
        assert!(model.confirm.is_none());
        assert_eq!(model.filter.text(), "by");
        model.on_key(press(KeyCode::Esc));
        model.on_key(press(KeyCode::Char('C')));
        let confirm = model.confirm.as_ref().expect("the ready plan");
        assert_eq!(confirm.title, "Clean bonsai?");
        assert!(confirm.ready_at.is_none());
    }

    #[test]
    fn project_rows_keep_their_selection_while_the_list_reorders() {
        let mut model = Model::from_snapshot(sample());
        model.command(Cmd::Sort, "size");
        model.on_key(press(KeyCode::Char('j')));
        assert_eq!(selected(&model), "other");
        // Sizes stream in and reorder the checkouts inside bonsai.
        model
            .footprints
            .insert("/trees/bonsai/main".into(), sized(9_000));
        model.rebuild();
        assert_eq!(model.rows[0].project, "bonsai");
        assert_eq!(selected(&model), "other");
        model
            .footprints
            .insert("/trees/other/feature".into(), sized(20_000));
        model.rebuild();
        assert_eq!(model.rows[0].project, "other");
        assert_eq!(selected(&model), "other");
    }

    #[test]
    fn deleted_checkouts_do_not_count_toward_sizes() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].missing = true;
        let mut model = Model::from_snapshot(snapshot);
        model
            .footprints
            .insert("/trees/bonsai/ab/dev".into(), sized(7_000));
        model
            .footprints
            .insert("/trees/bonsai/main".into(), sized(1_000));
        model.rebuild();
        let bonsai = model
            .rows
            .iter()
            .find(|row| row.project == "bonsai")
            .unwrap();
        assert_eq!(table::cell(bonsai, Col::Size)[0].content, "1 kB");
        assert_eq!(model.overall().2, 1_000);
    }

    #[test]
    fn leaving_a_removed_directory_returns_to_the_main_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::default();
        let mut source = Source {
            config: &config,
            initial: Some(directory.path().to_path_buf()),
            cwd: Some(directory.path().join("gone")),
            all: false,
        };
        assert_eq!(source.recovery(), Some(directory.path().to_path_buf()));
        source.cwd = Some(directory.path().to_path_buf());
        assert_eq!(source.recovery(), None);
        let flags = source.flags();
        assert_eq!(flags[0], "--root");
    }

    #[test]
    fn checkout_commands_use_the_selected_checkout() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Enter));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('A'))),
            Effect::Run(Action::Foreground(task))
                if task.dir == Path::new("/trees/bonsai/ab/dev") && task.label == "bonsai start"
        ));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('R'))),
            Effect::Run(Action::Foreground(task)) if task.label == "bonsai resume ab/dev"
        ));
        typed(&mut model, "yy");
        assert!(matches!(
            model.on_key(press(KeyCode::Char('y'))),
            Effect::None
        ));
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Enter));
        model.on_key(press(KeyCode::Char('y')));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('y'))),
            Effect::Copy(path) if path == "/trees/bonsai/ab/dev"
        ));
        let spec = background(model.on_key(press(KeyCode::Char('f'))));
        assert_eq!(spec.label, "git fetch --prune");
        assert_eq!(spec.dir, Path::new("/trees/bonsai"));
    }

    #[test]
    fn help_lists_every_command_key() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('?')));
        assert!(model.help);
        let (screen, _) = paint(&mut model, 120, 50);
        assert!(screen.contains("bonsai hq keys"));
        for spec in COMMANDS {
            assert!(
                view::HELP
                    .iter()
                    .flat_map(|(_, keys)| keys.iter())
                    .any(|(keys, _)| keys.split_whitespace().any(|key| key == spec.key)),
                "help is missing {} ({})",
                spec.key,
                spec.name
            );
        }
        model.on_key(press(KeyCode::Char('j')));
        assert!(model.help);
        model.on_key(press(KeyCode::Esc));
        assert!(!model.help);
    }

    #[test]
    fn fuzzy_find_matches_a_subsequence() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "adv");
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(paths, vec![PathBuf::from("/trees/bonsai/ab/dev")]);
    }

    #[test]
    fn recent_projects_and_worktrees_sort_first() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].last_activity = Some(100);
        snapshot.projects[0].worktrees[1].last_activity = Some(500);
        snapshot.projects[1].worktrees[0].last_activity = Some(900);
        let mut model = Model::from_snapshot(snapshot);
        let paths = |model: &Model| -> Vec<PathBuf> {
            model
                .rows
                .iter()
                .filter_map(|row| row.path.clone())
                .collect()
        };
        assert_eq!(model.sort, Sort::Name);
        assert_eq!(
            paths(&model),
            vec![
                PathBuf::from("/trees/bonsai/ab/dev"),
                PathBuf::from("/trees/other/feature"),
            ]
        );
        model.on_key(press(KeyCode::Char('s')));
        assert_eq!(model.sort, Sort::Recent);
        assert_eq!(
            paths(&model),
            vec![
                PathBuf::from("/trees/other/feature"),
                PathBuf::from("/trees/bonsai/main"),
            ]
        );
        model.on_key(press(KeyCode::Char('j')));
        model.on_key(press(KeyCode::Enter));
        assert_eq!(
            paths(&model),
            vec![
                PathBuf::from("/trees/bonsai/main"),
                PathBuf::from("/trees/bonsai/ab/dev"),
            ]
        );
        model.on_key(press(KeyCode::Char('s')));
        assert_eq!(model.sort, Sort::Size);
        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('s')));
        assert_eq!(model.filter.text(), "s");
        assert_eq!(model.sort, Sort::Size);
    }

    #[test]
    fn size_order_puts_the_largest_first() {
        let mut model = Model::from_snapshot(sample());
        model
            .footprints
            .insert("/trees/other/feature".into(), sized(5_000));
        model
            .footprints
            .insert("/trees/bonsai/main".into(), sized(1_000));
        model
            .footprints
            .insert("/trees/bonsai/ab/dev".into(), sized(3_000));
        model.on_key(press(KeyCode::Char('s')));
        model.on_key(press(KeyCode::Char('s')));
        assert_eq!(model.sort, Sort::Size);
        assert_eq!(model.rows[0].project, "other");
        model.on_key(press(KeyCode::Char('j')));
        model.on_key(press(KeyCode::Enter));
        assert_eq!(model.rows[0].title, "ab/dev");
        assert_eq!(model.rows[1].title, "main");
    }

    #[test]
    fn enter_on_an_empty_filter_does_nothing_and_r_refreshes() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "missing");
        assert!(matches!(model.on_key(press(KeyCode::Enter)), Effect::None));
        assert!(matches!(model.view, View::List));
        model.on_key(press(KeyCode::Esc));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('r'))),
            Effect::Refresh
        ));
    }

    #[test]
    fn checkout_cells_spell_out_their_state() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].external = true;
        snapshot.projects[0].worktrees[0].modified = 2;
        snapshot.projects[0].worktrees[1].upstream = Some("origin/main".to_string());
        snapshot.projects[0].worktrees[1].behind = 4;
        snapshot.projects[1].worktrees[0].upstream = Some("origin/feature".to_string());
        snapshot.projects[1].worktrees[0].upstream_gone = true;
        let mut model = Model::from_snapshot(snapshot);
        model.filter.set("trees");
        model.rebuild();
        assert_eq!(
            words(row_for(&model, "ab/dev")),
            "2 modified, 1 untracked | no upstream | external"
        );
        assert_eq!(
            words(row_for(&model, "bonsai/main")),
            "clean | 4 behind | main checkout"
        );
        assert_eq!(
            words(row_for(&model, "feature")),
            "clean | upstream gone | main checkout"
        );
        assert!(model.status_line().contains("unreadable"));
    }

    #[test]
    fn pending_status_reads_checking_instead_of_a_guess() {
        let mut snapshot = sample();
        snapshot.projects[1].worktrees[0].dirty = None;
        let mut model = Model::from_snapshot(snapshot);
        model.filter.set("trees");
        model.pending.insert(PathBuf::from("/trees/other/feature"));
        model.pending.insert(PathBuf::from("/trees/bonsai/ab/dev"));
        model.rebuild();
        assert_eq!(
            words(row_for(&model, "feature")),
            "checking | main checkout"
        );
        assert_eq!(words(row_for(&model, "ab/dev")), "checking");
        model.pending.clear();
        model.rebuild();
        assert!(words(row_for(&model, "feature")).contains("status unavailable"));
    }

    #[test]
    fn colon_runs_the_matching_maintenance_command() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char(':')));
        typed(&mut model, "pru");
        assert!(matches!(
            model.on_key(press(KeyCode::Enter)),
            Effect::Run(Action::Foreground(task))
                if task.label == "bonsai prune" && task.project.as_deref() == Some("bonsai")
        ));
        assert!(model.palette.is_none());
    }

    #[test]
    fn status_updates_fill_changes_and_ignore_stale_generations() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Enter));
        let path = PathBuf::from("/trees/bonsai/main");
        model.generation = 3;
        model.pending.insert(path.clone());
        model.snapshot.projects[0].worktrees[1].dirty = None;
        let update = |generation| inventory::StatusUpdate {
            generation,
            path: path.clone(),
            dirty: Some(true),
            added: 0,
            modified: 1,
            deleted: 0,
            untracked: 0,
            ahead: 2,
            behind: 0,
            upstream: Some("origin/main".to_string()),
            upstream_gone: false,
            warning: None,
        };
        assert!(!model.apply_status(update(1)));
        assert!(model.pending.contains(&path));
        assert!(model.apply_status(update(3)));
        assert!(!model.pending.contains(&path));
        assert_eq!(
            words(row_for(&model, "bonsai/main")),
            "1 modified | 2 ahead | main checkout"
        );
        model.on_key(press(KeyCode::Char('j')));
        assert!(model.status_line().contains("2 ahead of origin/main"));
    }

    #[test]
    fn column_titles_align_with_their_cells() {
        let model = Model::from_snapshot(sample());
        let layout = table::layout(Kind::Projects, &model.rows, 100);
        let header = table::header(&layout, 100).to_string();
        let row = table::line(&model.rows[0], &layout, 100).to_string();
        assert_eq!(header.find("host"), row.find("github.com"));
        assert_eq!(header.find("owner"), row.find("aymericbeaumet"));
        assert_eq!(header.find("project"), row.find("bonsai"));
        assert!(header.contains("checkouts"));
        assert!(header.contains("changes"));
        assert!(!header.contains("branch"));
        // Right-aligned numbers end under their title.
        let end = |text: &str, word: &str| text.find(word).map(|at| at + word.len());
        assert_eq!(end(&header, "checkouts"), row.find(" 2  ").map(|at| at + 2));

        let mut detail = Model::from_snapshot(sample());
        detail.on_key(press(KeyCode::Enter));
        let layout = table::layout(Kind::Checkouts, &detail.rows, 100);
        let header = table::header(&layout, 100).to_string();
        let row = table::line(&detail.rows[0], &layout, 100).to_string();
        assert_eq!(header.find("branch"), row.find("ab/dev"));
        assert_eq!(header.find("changes"), row.find("1 untracked"));
        assert!(!header.contains("host"));
        assert!(!header.contains("owner"));
        assert!(!header.contains("project"));
    }

    #[test]
    fn narrow_terminals_drop_the_least_useful_columns_first() {
        let mut model = Model::from_snapshot(sample());
        model
            .footprints
            .insert("/trees/bonsai/ab/dev".into(), sized(3_000));
        model.rebuild();
        let wide = table::layout(Kind::Projects, &model.rows, 140);
        for col in [
            Col::Host,
            Col::Owner,
            Col::Project,
            Col::Size,
            Col::Stack,
            Col::Agents,
        ] {
            assert!(wide.has(col), "{col:?} in {wide:?}");
        }
        let narrow = table::layout(Kind::Projects, &model.rows, 50);
        assert!(narrow.has(Col::Project));
        assert!(narrow.has(Col::Size));
        assert!(!narrow.has(Col::Stack));
        assert!(!narrow.has(Col::Host));
        let line = table::line(&model.rows[0], &narrow, 50).to_string();
        assert!(line.chars().count() <= 50, "{line}");
    }

    #[test]
    fn draws_the_selected_project_and_keys() {
        let mut model = Model::from_snapshot(sample());
        let (screen, _) = painted(&mut model);
        let host_at = screen.find("github.com").expect("host column");
        let owner_at = screen.find("aymericbeaumet").expect("owner column");
        assert!(host_at < owner_at);
        // Without any activity yet the age column has nothing to show.
        assert!(!screen.contains(" age"));
        for title in ["host", "owner", "project", "checkouts", "changes"] {
            assert!(screen.contains(title), "missing {title}");
        }
        assert!(screen.contains("interfaceinc"));
        assert!(screen.contains("1 changed"));
        assert!(!screen.contains("feature"));
        assert!(screen.contains("enter open"));
        assert!(screen.contains("? help"));
        assert!(screen.contains(" bonsai · 2 projects · 3 checkouts · sorted by name "));
        assert!(screen.contains("/trees/bonsai  ·  2 checkouts, 1 with uncommitted changes"));
        assert!(!screen.contains("measuring"));
        assert!(!screen.contains("┌─"));
        assert!(!screen.contains("├─"));
        let row = screen
            .lines()
            .find(|line| line.contains("aymericbeaumet"))
            .unwrap();
        assert!(!row.contains('·'), "no bare counts after names: {row}");
    }

    #[test]
    fn expanded_views_use_a_ruled_frame() {
        let mut model = Model::from_snapshot(sample());
        let (root, root_buf) = painted(&mut model);
        assert!(root.contains("host"));
        assert!(!root.contains("┌─"));
        assert_eq!(root_buf[(0, 0)].style().fg, Some(Color::Reset));
        let y = row_index(&root, "github.com  aymericbeaumet");
        let home = root_buf[(1, y)].style();
        assert!(home.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(home.bg, Some(Color::Reset));

        model.on_key(press(KeyCode::Char('t')));
        let (tree, tree_buf) = painted(&mut model);
        assert!(tree.contains(" bonsai · tree "));
        assert!(tree.contains("┌─ github.com"));
        assert!(tree.contains("│  ├─ aymericbeaumet"));
        assert!(tree.contains("│  │  └─ bonsai "));
        assert!(tree.contains("│  │     ├─ ab/dev"));
        assert!(tree.contains("│  │     └─ main"));
        assert!(tree.contains("│  └─ interfaceinc"));
        assert!(tree.contains("│     └─ other "));
        assert!(tree.contains("│        └─ feature"));
        let owner_at = tree.find("├─ aymericbeaumet").expect("owner");
        let repo_at = tree.find("└─ bonsai").expect("repository");
        let branch_at = tree.find("├─ ab/dev").expect("checkout");
        assert!(owner_at < repo_at && repo_at < branch_at);
        assert!(!tree.contains("host"));
        assert!(!tree.contains("owner"));
        assert_eq!(tree_buf[(0, 0)].style().fg, Some(Color::Cyan));
        let y = row_index(&tree, "├─ ab/dev");
        let selected = tree_buf[(1, y)].style();
        assert_eq!(selected.bg, Some(Color::Cyan));
        assert_eq!(selected.fg, Some(Color::Black));
        assert!(!selected.add_modifier.contains(Modifier::DIM));
        let y = row_index(&tree, "┌─ github.com");
        assert_eq!(tree_buf[(1, y)].style().bg, Some(Color::Reset));

        model.on_key(press(KeyCode::Enter));
        let (detail, detail_buf) = painted(&mut model);
        assert!(detail.contains(" bonsai · bonsai · 2 checkouts · sorted by name "));
        assert!(detail.contains("git@github.com:aymericbeaumet/bonsai.git"));
        let lines: Vec<&str> = detail.lines().collect();
        let path_at = lines
            .iter()
            .position(|line| line.contains("/trees/bonsai"))
            .expect("project path");
        assert!(lines[path_at + 1].contains("2 checkouts · 1 with uncommitted changes"));
        let rule = lines[path_at + 2];
        assert!(rule.chars().filter(|ch| *ch == '─').count() >= 30);
        assert!(rule.chars().all(|ch| matches!(ch, '─' | '│')));
        assert!(detail.contains(" branch"));
        assert!(detail.contains(" ab/dev"));
        assert!(detail.contains(" main"));
        assert!(!detail.contains("┌─"));
        assert!(!detail.contains("host"));
        assert_eq!(detail_buf[(0, 0)].style().fg, Some(Color::Cyan));
        let y = row_index(&detail, " ab/dev ");
        assert_eq!(detail_buf[(1, y)].style().bg, Some(Color::Cyan));
        assert_eq!(detail_buf[(1, y)].style().fg, Some(Color::Black));

        model.on_key(press(KeyCode::Esc));
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "feat");
        let (found, found_buf) = painted(&mut model);
        assert!(found.contains("feature"));
        assert!(found.contains("host"));
        assert!(found.contains("branch"));
        assert!(!found.contains("┌─"));
        assert!(!found.contains("├─"));
        assert_eq!(found_buf[(0, 0)].style().fg, Some(Color::Reset));
        let y = row_index(&found, "feature");
        let matched = found_buf[(1, y)].style();
        assert!(matched.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(matched.bg, Some(Color::Reset));
    }

    #[test]
    fn project_rows_and_groups_sum_their_checkouts() {
        let mut model = Model::from_snapshot(sample());
        model
            .footprints
            .insert("/trees/bonsai/ab/dev".into(), sized(1_500));
        model
            .footprints
            .insert("/trees/bonsai/main".into(), sized(2_500));
        model
            .footprints
            .insert("/trees/other/feature".into(), sized(6_000));
        model.rebuild();
        let bonsai = model
            .rows
            .iter()
            .find(|row| row.project == "bonsai")
            .unwrap();
        assert_eq!(words(bonsai), "2 | 1 changed | 4 kB | rust | claude, grok");
        let (screen, _) = painted(&mut model);
        assert!(
            screen.contains("2 projects · 3 checkouts · 10 kB"),
            "{screen}"
        );

        model.on_key(press(KeyCode::Char('t')));
        let host = model
            .rows
            .iter()
            .find(|row| row.depth == Depth::Host)
            .unwrap();
        assert!(words(host).contains("10 kB"), "{}", words(host));
        let owner = model
            .rows
            .iter()
            .find(|row| row.depth == Depth::Owner && row.owner == "aymericbeaumet")
            .unwrap();
        assert!(words(owner).contains("4 kB"), "{}", words(owner));
    }

    #[test]
    fn sizes_stream_in_without_shrinking_a_remembered_size() {
        let mut model = Model::from_snapshot(sample());
        model
            .footprints
            .insert("/trees/bonsai/main".into(), sized(9_000));
        model.generation = 2;
        model.measuring = ["/trees/bonsai/main", "/trees/bonsai/ab/dev"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        model.rebuild();
        assert!(model.status_line().contains("measuring sizes, 0 of 2 done"));
        assert!(model.apply_footprint(measured(&model, "/trees/bonsai/main", 4_000, false)));
        assert_eq!(
            model.footprints[Path::new("/trees/bonsai/main")].bytes,
            9_000
        );
        assert!(model.apply_footprint(measured(&model, "/trees/bonsai/ab/dev", 1_000, false)));
        assert_eq!(
            model.footprints[Path::new("/trees/bonsai/ab/dev")].bytes,
            1_000
        );
        let bonsai = model
            .rows
            .iter()
            .find(|row| row.project == "bonsai")
            .unwrap();
        let size = table::cell(bonsai, Col::Size);
        assert_eq!(size[0].content, "10 kB");
        assert!(size[0].style.add_modifier.contains(Modifier::DIM));

        assert!(model.apply_footprint(measured(&model, "/trees/bonsai/main", 5_000, true)));
        assert_eq!(
            model.footprints[Path::new("/trees/bonsai/main")].bytes,
            5_000
        );
        assert!(model.cached.contains_key(Path::new("/trees/bonsai/main")));
        assert!(model.status_line().contains("1 of 2 done"));
        let stale = footprint::Measured {
            generation: 1,
            ..measured(&model, "/trees/bonsai/ab/dev", 7_000, true)
        };
        assert!(!model.apply_footprint(stale));
        assert!(model.apply_footprint(measured(&model, "/trees/bonsai/ab/dev", 2_000, true)));
        assert!(!model.status_line().contains("measuring"));
        let bonsai = model
            .rows
            .iter()
            .find(|row| row.project == "bonsai")
            .unwrap();
        let size = table::cell(bonsai, Col::Size);
        assert_eq!(size[0].content, "7 kB");
        assert!(!size[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_measured_footprint_is_filterable() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Enter));
        assert!(model.apply_footprint(measured(&model, "/trees/bonsai/main", 1_536, true)));
        model.on_key(press(KeyCode::Char('j')));
        let main = row_for(&model, "bonsai/main");
        assert_eq!(
            words(main),
            "clean | no upstream | 1.5 kB | target 768 B | rust | claude, grok | main checkout"
        );
        assert!(
            model
                .status_line()
                .contains("1.5 kB, of which target 768 B")
        );
        assert!(model.status_line().contains("unreadable"));
        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "claude");
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(paths, vec![PathBuf::from("/trees/bonsai/main")]);
    }

    #[test]
    fn the_home_list_is_one_row_per_project() {
        let model = Model::from_snapshot(sample());
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/trees/bonsai/ab/dev"),
                PathBuf::from("/trees/other/feature"),
            ]
        );
        let checkouts = |row: &Row| match &row.info {
            Info::Group(totals) => totals.checkouts,
            Info::Checkout(_) => panic!("a project row"),
        };
        assert_eq!(model.rows[0].host, "github.com");
        assert_eq!(model.rows[0].owner, "aymericbeaumet");
        assert_eq!(model.rows[0].project, "bonsai");
        assert_eq!(model.rows[0].title, "");
        assert!(!model.tree);
        assert_eq!(checkouts(&model.rows[0]), 2);
        assert_eq!(model.rows[1].host, "github.com");
        assert_eq!(model.rows[1].owner, "interfaceinc");
        assert_eq!(model.rows[1].title, "");
        assert_eq!(checkouts(&model.rows[1]), 1);
    }

    #[test]
    fn the_host_column_keeps_a_host_that_has_no_owner() {
        let mut snapshot = sample();
        snapshot.projects[1].remote = Some("git@gitlab.com:interfaceinc/other.git".to_string());
        snapshot.projects.push(Project {
            id: "local".to_string(),
            name: "local".to_string(),
            path: "/trees/local".into(),
            remote: Some("git@example:bonsai.git".to_string()),
            worktrees: vec![worktree("main", "/trees/local/main", true, Some(false))],
        });
        let mut model = Model::from_snapshot(snapshot);
        let other = model
            .rows
            .iter()
            .find(|row| row.project == "other")
            .unwrap();
        assert_eq!(other.host, "gitlab.com");
        assert_eq!(other.owner, "interfaceinc");
        let local = model
            .rows
            .iter()
            .find(|row| row.project == "local")
            .unwrap();
        assert_eq!(local.host, "example");
        assert_eq!(local.owner, "");

        model.on_key(press(KeyCode::Char('t')));
        assert!(model.rows.iter().any(|row| {
            row.depth == Depth::Host && row.host == "gitlab.com" && row.owner.is_empty()
        }));
        assert!(model.rows.iter().any(|row| {
            row.depth == Depth::Owner && row.owner == "interfaceinc" && row.project.is_empty()
        }));
        assert!(
            model
                .rows
                .iter()
                .any(|row| row.depth == Depth::Host && row.host == "example")
        );
        assert!(model.rows.iter().any(|row| {
            row.depth == Depth::Repo && row.project == "local" && row.owner.is_empty()
        }));
        assert!(
            !model
                .rows
                .iter()
                .any(|row| { row.depth == Depth::Owner && row.host == "example" })
        );
        assert!(model.rows.iter().any(|row| row.depth == Depth::Branch
            && row.host.is_empty()
            && row.title == "feature"));

        model.on_key(press(KeyCode::Char('/')));
        typed(&mut model, "gitlab");
        assert!(model.rows.iter().all(|row| row.host == "gitlab.com"));
        assert!(model.rows.iter().any(|row| row.title == "feature"));
    }

    #[test]
    fn the_tree_is_host_then_owner_then_repository() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].last_activity = Some(50);
        snapshot.projects[0].worktrees[1].last_activity = Some(100);
        snapshot.projects[1].worktrees[0].last_activity = Some(900);
        let mut tools = worktree("main", "/trees/tools/main", true, Some(false));
        tools.last_activity = Some(80);
        snapshot.projects.push(Project {
            id: "tools".to_string(),
            name: "tools".to_string(),
            path: "/trees/tools".into(),
            remote: Some("git@github.com:aymericbeaumet/tools.git".to_string()),
            worktrees: vec![tools],
        });
        snapshot.projects.push(Project {
            id: "plain".to_string(),
            name: "plain".to_string(),
            path: "/trees/plain".into(),
            remote: None,
            worktrees: vec![worktree("main", "/trees/plain/main", true, Some(false))],
        });
        let mut model = Model::from_snapshot(snapshot);
        model.on_key(press(KeyCode::Char('t')));

        let open_owner = child_prefix(TREE_RAIL, true);
        let shut_owner = child_prefix(TREE_RAIL, false);
        let shape: Vec<_> = model
            .rows
            .iter()
            .map(|row| {
                let label = match row.depth {
                    Depth::Host => row.host.clone(),
                    Depth::Owner => row.owner.clone(),
                    Depth::Repo => row.project.clone(),
                    Depth::Branch | Depth::Flat => row.title.clone(),
                };
                (row.depth, label, row.guide, row.prefix.clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            vec![
                (Depth::Repo, "plain".to_string(), "", String::new()),
                (
                    Depth::Branch,
                    "main".to_string(),
                    "└ ",
                    TREE_RAIL.to_string(),
                ),
                (Depth::Host, "github.com".to_string(), "", String::new()),
                (
                    Depth::Owner,
                    "aymericbeaumet".to_string(),
                    "├ ",
                    TREE_RAIL.to_string(),
                ),
                (Depth::Repo, "bonsai".to_string(), "├ ", open_owner.clone()),
                (
                    Depth::Branch,
                    "ab/dev".to_string(),
                    "├ ",
                    child_prefix(&open_owner, true),
                ),
                (
                    Depth::Branch,
                    "main".to_string(),
                    "└ ",
                    child_prefix(&open_owner, true),
                ),
                (Depth::Repo, "tools".to_string(), "└ ", open_owner.clone()),
                (
                    Depth::Branch,
                    "main".to_string(),
                    "└ ",
                    child_prefix(&open_owner, false),
                ),
                (
                    Depth::Owner,
                    "interfaceinc".to_string(),
                    "└ ",
                    TREE_RAIL.to_string(),
                ),
                (Depth::Repo, "other".to_string(), "└ ", shut_owner.clone()),
                (
                    Depth::Branch,
                    "feature".to_string(),
                    "└ ",
                    child_prefix(&shut_owner, false),
                ),
            ]
        );
        model.on_key(press(KeyCode::Char('s')));
        let labels: Vec<_> = model
            .rows
            .iter()
            .map(|row| match row.depth {
                Depth::Host => row.host.clone(),
                Depth::Owner => row.owner.clone(),
                Depth::Repo => row.project.clone(),
                Depth::Branch | Depth::Flat => row.title.clone(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "github.com",
                "interfaceinc",
                "other",
                "feature",
                "aymericbeaumet",
                "bonsai",
                "main",
                "ab/dev",
                "tools",
                "main",
                "plain",
                "main",
            ]
        );
    }

    #[test]
    fn t_opens_and_closes_the_worktree_tree() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('t')));
        assert!(model.tree);
        assert!(matches!(model.view, View::List));
        assert!(
            model
                .rows
                .iter()
                .any(|row| row.depth == Depth::Repo && row.project == "bonsai")
        );
        assert!(
            model
                .rows
                .iter()
                .any(|row| row.guide == "├ " && row.title == "ab/dev")
        );
        assert!(
            model
                .rows
                .iter()
                .any(|row| row.guide == "└ " && row.title == "main")
        );
        assert!(model.rows.iter().any(|row| row.title == "feature"));
        assert!(model.rows[model.cursor].path.is_some());
        let selected = model.selected_path();
        model.on_key(press(KeyCode::Enter));
        assert!(matches!(model.view, View::Detail { .. }));
        model.on_key(press(KeyCode::Char('t')));
        assert!(!model.tree);
        assert!(matches!(model.view, View::List));
        assert_eq!(model.rows.len(), 2);
        assert!(model.rows.iter().all(|row| row.title.is_empty()));
        assert_eq!(model.selected_path(), selected);

        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('t')));
        assert_eq!(model.filter.text(), "t");
        assert!(!model.tree);
    }

    #[test]
    fn owner_prefixes_filter_by_kind_and_search_lists_branches() {
        let mut model = Model::from_snapshot(sample());
        model.owners.insert(
            "github.com/aymericbeaumet".to_string(),
            owner::OwnerKind::User,
        );
        model.owners.insert(
            "github.com/interfaceinc".to_string(),
            owner::OwnerKind::Organisation,
        );
        model.filter.set("organisation:interface");
        model.rebuild();
        assert_eq!(
            model
                .rows
                .iter()
                .filter_map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("/trees/other/feature")]
        );
        assert_eq!(model.rows[0].title, "feature");
        assert_eq!(model.rows[0].owner, "interfaceinc");

        model.filter.set("user:aymeric");
        model.rebuild();
        assert!(model.rows.iter().all(|row| row.owner == "aymericbeaumet"));
        assert!(model.rows.iter().any(|row| row.title == "ab/dev"));
        assert!(model.rows.iter().any(|row| row.title == "main"));

        model.filter.set("organisation:aymeric");
        model.rebuild();
        assert!(model.rows.is_empty());

        model.owners.clear();
        model.filter.set("user:interface");
        model.rebuild();
        assert_eq!(
            model
                .rows
                .iter()
                .filter_map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("/trees/other/feature")]
        );
    }

    #[test]
    fn a_missing_checkout_is_counted_and_spelled_out() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].missing = true;
        snapshot.projects[0].worktrees[0].prunable = true;
        snapshot.projects[0].worktrees[0].dirty = None;
        snapshot
            .warnings
            .push("/trees/bonsai/ab/dev: missing checkout".to_string());
        let mut model = Model::from_snapshot(snapshot);
        let bonsai = model
            .rows
            .iter()
            .find(|row| row.project == "bonsai")
            .unwrap();
        assert!(words(bonsai).contains("1 missing"), "{}", words(bonsai));
        assert!(
            !model.status_line().contains("warnings"),
            "{}",
            model.status_line()
        );
        model.on_key(press(KeyCode::Enter));
        assert_eq!(words(row_for(&model, "ab/dev")), "directory deleted");
        // Deleted checkouts sort after the ones that still exist.
        assert_eq!(model.rows.last().unwrap().title, "ab/dev");
        model.on_key(press(KeyCode::Char('G')));
        assert!(matches!(model.on_key(press(KeyCode::Enter)), Effect::None));
        assert!(model.status_line().contains("press P to prune"));
        let (screen, _) = paint(&mut model, 120, 24);
        assert!(screen.contains("1 deleted by hand (press P to prune it)"));
    }

    #[test]
    fn several_warnings_point_at_the_log() {
        let mut snapshot = sample();
        snapshot.warnings.push("second problem".to_string());
        let model = Model::from_snapshot(snapshot);
        let line = model.status_line();
        assert!(line.contains("2 warnings"), "{line}");
        assert!(line.contains("hq.log"), "{line}");
        assert!(!line.contains("unreadable"), "{line}");
    }

    #[test]
    fn warnings_append_to_the_log_as_one_line() {
        let path = std::env::temp_dir().join(format!(
            "bonsai-hq-log-{}-{}",
            std::process::id(),
            now_secs()
        ));
        append_log_to(
            &path,
            "fatal: cannot change to '/tmp/gone': No such file\nor directory",
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(text.contains("fatal: cannot change to '/tmp/gone': No such file or directory"));
        assert_eq!(text.lines().count(), 1, "{text}");
    }

    #[test]
    fn a_finished_add_selects_the_new_checkout() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees.push(worktree(
            "feat-x",
            "/trees/bonsai/feat-x",
            false,
            Some(false),
        ));
        let mut model = Model::from_snapshot(snapshot);
        assert!(matches!(model.view, View::List));
        model.reveal(Path::new("/trees/bonsai/feat-x"));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        assert!(model.selected_path().unwrap().ends_with("feat-x"));
    }
}
