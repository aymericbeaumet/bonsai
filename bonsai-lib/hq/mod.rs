mod footprint;
mod inventory;
mod owner;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use inventory::{Project, Snapshot, WorktreeEntry};

use crate::config::Config;
use crate::edit::LineInput;
use crate::picker;

/// Browse discovered worktrees. `--json` prints the inventory and returns.
/// `all` scans the whole root; otherwise only the current project is shown.
pub fn run(config: &Config, json: bool, all: bool) -> Result<()> {
    std::fs::create_dir_all(config.root_dir()).context("cannot create the bonsai root")?;
    let initial = std::env::current_dir().ok();
    if json {
        let snapshot = inventory::snapshot(config, initial.as_deref(), all)?;
        print_inventory(&snapshot)?;
        return Ok(());
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("hq needs a terminal; pass --json to print the inventory");
    }
    session(config, initial.as_deref(), all)
}

fn session(config: &Config, initial: Option<&Path>, all: bool) -> Result<()> {
    let mut terminal = ratatui::try_init().context("cannot start the terminal interface")?;
    let result = (|| -> Result<()> {
        terminal.draw(|frame| draw_message(frame, "Loading worktrees…"))?;
        let mut model = Model::from_snapshot(inventory::discover(config, initial, all)?);
        model.scrolloff = config.hq.scrolloff;
        if let Some(path) = owner::cache_path() {
            model.owners = owner::load(&path);
            model.owner_cache = Some(path);
        }
        let mut jobs = begin_jobs(&mut model);
        loop {
            terminal.draw(|frame| draw(frame, &mut model))?;
            loop {
                let mut changed = false;
                while let Some(update) = jobs.status.try_recv() {
                    changed |= model.apply_status(update);
                }
                while let Some(update) = jobs.measure.try_recv() {
                    changed |= model.apply_footprint(update);
                }
                while let Some(update) = jobs.owners.try_recv() {
                    changed |= model.apply_owner(update);
                }
                if changed {
                    break;
                }
                if !event::poll(Duration::from_millis(50))? {
                    continue;
                }
                match event::read()? {
                    Event::Key(key) => match model.on_key(key) {
                        Effect::None => break,
                        Effect::Quit => return Ok(()),
                        Effect::Refresh => {
                            model.notice = Some("Refreshing…".to_string());
                            terminal.draw(|frame| draw(frame, &mut model))?;
                            if let Err(error) = reload(&mut model, config, initial, all) {
                                model.note_error(&error);
                                model.notice = Some(error);
                            } else {
                                model.notice = None;
                                jobs = begin_jobs(&mut model);
                            }
                            break;
                        }
                        Effect::Maintain(command) => {
                            let anchor = model.selected_anchor();
                            suspend_terminal(&mut terminal)?;
                            let outcome = run_maintenance(config, anchor.as_ref(), command);
                            pause_for_return();
                            resume_terminal(&mut terminal)?;
                            model.notice = Some("Refreshing…".to_string());
                            terminal.draw(|frame| draw(frame, &mut model))?;
                            let message = match &outcome {
                                Ok(message) => message.clone(),
                                Err(error) => {
                                    let text = format!("{error:#}");
                                    model.note_error(&text);
                                    text
                                }
                            };
                            match reload(&mut model, config, initial, all) {
                                Ok(()) => {
                                    jobs = begin_jobs(&mut model);
                                    model.notice = Some(message);
                                }
                                Err(error) => {
                                    model.note_error(&error);
                                    model.notice = Some(format!("{message}  {error}"));
                                }
                            }
                            break;
                        }
                    },
                    Event::Resize(_, _) => break,
                    _ => {}
                }
            }
        }
    })();
    drop(terminal);
    result
}

fn suspend_terminal(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let _ = terminal.show_cursor();
    crossterm::terminal::disable_raw_mode().context("cannot leave the terminal interface")?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)
        .context("cannot leave the terminal interface")?;
    Ok(())
}

fn resume_terminal(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    crossterm::terminal::enable_raw_mode().context("cannot restore the terminal interface")?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)
        .context("cannot restore the terminal interface")?;
    terminal
        .clear()
        .context("cannot restore the terminal interface")?;
    let _ = terminal.hide_cursor();
    Ok(())
}

fn pause_for_return() {
    let _ = writeln!(std::io::stderr(), "\nbonsai: press enter to return");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

struct Jobs {
    measure: footprint::MeasureJob,
    status: inventory::StatusJob,
    owners: owner::OwnerJob,
}

fn begin_jobs(model: &mut Model) -> Jobs {
    model.generation = model.generation.saturating_add(1);
    model.footprints.clear();
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
        }
    }
    let measure_paths = model
        .snapshot
        .projects
        .iter()
        .flat_map(|project| {
            project
                .worktrees
                .iter()
                .filter(|worktree| !worktree.missing)
                .map(|worktree| worktree.path.clone())
        })
        .collect();
    model.rebuild();
    let pending_owners = unknown_github_owners(&model.snapshot, &model.owners);
    Jobs {
        measure: footprint::start(measure_paths, model.generation),
        status: inventory::start_status(paths, model.generation),
        owners: owner::start(pending_owners),
    }
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

fn reload(
    model: &mut Model,
    config: &Config,
    initial: Option<&Path>,
    all: bool,
) -> Result<(), String> {
    match inventory::discover(config, initial, all) {
        Ok(snapshot) => {
            model.set_snapshot(snapshot);
            Ok(())
        }
        Err(error) => Err(format!("refresh failed: {error}")),
    }
}

fn draw_message(frame: &mut Frame, message: &str) {
    frame.render_widget(Paragraph::new(message), frame.area());
}

fn draw(frame: &mut Frame, model: &mut Model) {
    let area = frame.area();
    if area.width < 24 || area.height < 6 {
        model.viewport = 0;
        frame.render_widget(
            Paragraph::new("bonsai\nEnlarge the terminal.\nq quit"),
            area,
        );
        return;
    }
    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
    let mut block = Block::bordered().title(title(model));
    if framed(model) {
        block = block.border_style(Style::new().fg(Color::Cyan));
    }
    let inner = block.inner(layout[0]);
    let show_detail = model.palette.is_none() && matches!(model.view, View::Detail { .. });
    let (header_area, list_area) = if show_detail {
        let split = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(inner);
        (Some(split[0]), split[1])
    } else {
        (None, inner)
    };
    let width = usize::from(list_area.width);
    let columns = columns_for(&model.rows, width);
    // The tree's project banners replace the index headings.
    let show_columns = model.palette.is_none()
        && !model.rows.is_empty()
        && list_area.height > 1
        && !tree_frame(model);
    let (title_area, list_area) = if show_columns {
        let split = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(list_area);
        (Some(split[0]), split[1])
    } else {
        (None, list_area)
    };
    model.viewport = list_area.height;
    let (selected, len, stored) = list_window(model);
    let scroll = scroll_top(
        stored,
        selected,
        usize::from(list_area.height),
        len,
        model.scrolloff,
    );
    set_scroll(model, scroll);
    let items: Vec<ListItem> = if model.palette.is_some() {
        palette_items(model, list_area.width)
    } else if model.rows.is_empty() {
        vec![ListItem::new(if model.filter.is_empty() {
            " No worktrees."
        } else {
            " No matches."
        })]
    } else {
        model
            .rows
            .iter()
            .map(|row| ListItem::new(row_lines(row, width, model, columns)))
            .collect()
    };
    let mut state = ListState::default().with_offset(scroll);
    if len > 0 {
        state.select(Some(selected));
    }
    frame.render_widget(&block, layout[0]);
    if let Some(header_area) = header_area {
        frame.render_widget(
            Paragraph::new(detail_header(model, usize::from(header_area.width))),
            header_area,
        );
    }
    if let Some(title_area) = title_area {
        frame.render_widget(
            Paragraph::new(column_header(
                columns,
                usize::from(title_area.width),
                model.rows.iter().any(is_detail_row),
            )),
            title_area,
        );
    }
    let highlight = if framed(model) {
        Style::new()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .remove_modifier(Modifier::all())
    } else {
        Style::new().add_modifier(Modifier::REVERSED)
    };
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(highlight)
            .highlight_symbol(""),
        list_area,
        &mut state,
    );
    // Ratatui clamps the offset to the rows that fit. Keep that clamp.
    set_scroll(model, state.offset());
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(keys(model)),
            Line::styled(
                truncate(&model.status_line(), usize::from(layout[1].width)),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ]),
        layout[1],
    );
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

fn list_window(model: &Model) -> (usize, usize, usize) {
    if let Some(palette) = &model.palette {
        return (palette.cursor, palette_matches(model).len(), palette.scroll);
    }
    if model.rows.is_empty() {
        return (0, 0, model.scroll);
    }
    (model.cursor, model.rows.len(), model.scroll)
}

fn set_scroll(model: &mut Model, scroll: usize) {
    if let Some(palette) = model.palette.as_mut() {
        palette.scroll = scroll;
    } else {
        model.scroll = scroll;
    }
}

fn title(model: &Model) -> Line<'static> {
    if let Some(palette) = &model.palette {
        return palette.query.titled(" bonsai  :", true);
    }
    if model.filtering || !model.filter.is_empty() {
        model.filter.titled(" bonsai  /", model.filtering)
    } else if let View::Detail { project } = &model.view {
        let name = model
            .snapshot
            .projects
            .iter()
            .find(|item| &item.id == project)
            .map(|item| item.name.as_str())
            .unwrap_or("project");
        Line::from(format!(" bonsai · {name} · {} ", model.sort.label()))
    } else if model.tree {
        Line::from(format!(" bonsai · tree · {} ", model.sort.label()))
    } else {
        Line::from(format!(" bonsai · {} ", model.sort.label()))
    }
}

/// An open project, or the home tree while search is closed.
fn framed(model: &Model) -> bool {
    model.palette.is_none() && (matches!(model.view, View::Detail { .. }) || tree_frame(model))
}

fn tree_frame(model: &Model) -> bool {
    model.palette.is_none()
        && matches!(model.view, View::List)
        && model.tree
        && model.filter.is_empty()
}

fn keys(model: &Model) -> &'static str {
    if model.palette.is_some() {
        "ctrl-b/f move   ctrl-w word   enter run   ctrl-o/esc up   ctrl-c quit"
    } else if model.filtering {
        "ctrl-b/f move   ctrl-w word   ctrl-i in   ctrl-o/esc up   ctrl-c quit"
    } else if matches!(model.view, View::Detail { .. }) {
        if model.filter.is_empty() {
            "j/k gg/G   t tree   s sort   ctrl-o/esc up   : command   r refresh   q quit"
        } else {
            "j/k gg/G   / edit   s sort   ctrl-o/esc up   : command   q quit"
        }
    } else if model.filter.is_empty() {
        "j/k gg/G  ctrl-i/enter in  ctrl-o/esc up  /  t tree  s sort  : command  q quit"
    } else {
        "j/k gg/G   / edit   ctrl-i/enter in   ctrl-o/esc up   s sort   q quit"
    }
}

fn detail_header(model: &Model, width: usize) -> Vec<Line<'static>> {
    let View::Detail { project } = &model.view else {
        return Vec::new();
    };
    let Some(project) = model
        .snapshot
        .projects
        .iter()
        .find(|item| &item.id == project)
    else {
        return Vec::new();
    };
    let remote = project
        .remote
        .clone()
        .unwrap_or_else(|| "no remote".to_string());
    let remote_style = if project.remote.is_some() {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    };
    vec![
        Line::styled(truncate(&remote, width), remote_style),
        Line::styled(
            truncate(&path_label(&project.path), width),
            Style::new().fg(Color::DarkGray),
        ),
        Line::styled("─".repeat(width), rule_style()),
    ]
}

fn palette_items(model: &Model, width: u16) -> Vec<ListItem<'static>> {
    let matches = palette_matches(model);
    if matches.is_empty() {
        return vec![ListItem::new(" No matches.")];
    }
    matches
        .into_iter()
        .map(|spec| {
            let name = pad(spec.name.to_string(), 12.min(usize::from(width)));
            let hint_width = usize::from(width).saturating_sub(name.chars().count() + 1);
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {name}"),
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(spec.hint, hint_width),
                    Style::new().add_modifier(Modifier::DIM),
                ),
            ]))
        })
        .collect()
}

fn row_lines(row: &Row, width: usize, model: &Model, columns: Columns) -> Vec<Line<'static>> {
    vec![summary_line(row, width, model, columns)]
}

#[derive(Clone, Copy)]
struct Columns {
    host: usize,
    owner: usize,
    project: usize,
    branch: usize,
}

fn columns_for(rows: &[Row], width: usize) -> Columns {
    let host = column_width(rows.iter().map(|row| row.host.chars().count()), "host", 32);
    let owner = column_width(
        rows.iter().map(|row| row.owner.chars().count()),
        "owner",
        24,
    );
    let project = column_width(
        rows.iter().map(|row| project_label(row).chars().count()),
        "project",
        32,
    );
    let branch = column_width(
        rows.iter()
            .filter(|row| row.depth != Depth::Branch)
            .map(|row| row.title.chars().count()),
        "branch",
        42,
    );
    let mut columns = Columns {
        host,
        owner,
        project,
        branch,
    };
    let gaps = [host, owner, project, branch]
        .into_iter()
        .filter(|width| *width > 0)
        .count()
        .saturating_sub(1)
        * 2;
    // Detail rows carry a rail before the branch column.
    let rail = if rows.iter().any(is_detail_row) {
        DETAIL_RAIL.chars().count()
    } else {
        0
    };
    let budget = width
        .saturating_sub(6)
        .saturating_sub(rail)
        .saturating_sub(gaps);
    let mut overflow = columns
        .host
        .saturating_add(columns.owner)
        .saturating_add(columns.project)
        .saturating_add(columns.branch)
        .saturating_sub(budget);
    for width in [
        &mut columns.branch,
        &mut columns.project,
        &mut columns.owner,
        &mut columns.host,
    ] {
        let cut = overflow.min(*width);
        *width -= cut;
        overflow -= cut;
    }
    columns
}

fn column_width(widths: impl Iterator<Item = usize>, title: &str, cap: usize) -> usize {
    match widths.max().unwrap_or(0) {
        0 => 0,
        content => content.max(title.chars().count()).min(cap),
    }
}

fn column_header(columns: Columns, width: usize, rail: bool) -> Line<'static> {
    let quiet = Style::new().add_modifier(Modifier::BOLD | Modifier::DIM);
    let mut spans = Vec::new();
    push_column(
        &mut spans,
        "host",
        columns.host,
        host_style().add_modifier(Modifier::BOLD),
    );
    push_column(
        &mut spans,
        "owner",
        columns.owner,
        owner_style().add_modifier(Modifier::BOLD),
    );
    push_column(&mut spans, "project", columns.project, project_style());
    push_column(&mut spans, "branch", columns.branch, quiet);
    spans.push(Span::styled(age_cell("age"), quiet));
    if rail {
        spans.insert(0, rail_span());
    }
    Line::from(fit_spans(spans, width))
}

fn age_cell(text: &str) -> String {
    format!(" {text:>4}")
}

fn project_label(row: &Row) -> String {
    if row.project.is_empty() {
        return String::new();
    }
    if row.count > 0 {
        format!("{} · {}", row.project, row.count)
    } else {
        row.project.clone()
    }
}

fn is_detail_row(row: &Row) -> bool {
    row.depth == Depth::Flat && row.project.is_empty() && !row.title.is_empty()
}

/// Inset from the frame so the branch rail does not sit on the border.
const DETAIL_RAIL: &str = " │ ";

fn rule_style() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::DIM)
}

fn rail_span() -> Span<'static> {
    Span::styled(DETAIL_RAIL, rule_style())
}

fn host_style() -> Style {
    Style::new().fg(Color::LightBlue)
}

fn owner_style() -> Style {
    Style::new().fg(Color::LightMagenta)
}

fn project_style() -> Style {
    Style::new()
        .fg(Color::LightGreen)
        .add_modifier(Modifier::BOLD)
}

fn branch_style(row: &Row) -> Style {
    if row.main {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

fn plain_g(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('g') && key.modifiers.is_empty()
}

fn shift_g(key: &KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return false;
    }
    matches!(key.code, KeyCode::Char('G'))
        || (key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::SHIFT))
}

fn ctrl_letter(key: &KeyEvent, ch: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char(got) if got.eq_ignore_ascii_case(&ch))
}

fn summary_line(row: &Row, width: usize, model: &Model, columns: Columns) -> Line<'static> {
    if matches!(row.depth, Depth::Host | Depth::Owner | Depth::Repo) {
        return group_line(row, width);
    }
    if row.depth == Depth::Branch {
        return nested_line(row, width, model);
    }
    let mut spans = Vec::new();
    push_column(&mut spans, &row.host, columns.host, host_style());
    push_column(&mut spans, &row.owner, columns.owner, owner_style());
    push_column(
        &mut spans,
        &project_label(row),
        columns.project,
        project_style(),
    );
    push_column(&mut spans, &row.title, columns.branch, branch_style(row));
    push_tail(&mut spans, row, model);
    if is_detail_row(row) {
        spans.insert(0, rail_span());
    }
    Line::from(fit_spans(spans, width))
}

fn group_line(row: &Row, width: usize) -> Line<'static> {
    let (label, style) = match row.depth {
        Depth::Host => (row.host.clone(), host_style()),
        Depth::Owner => (row.owner.clone(), owner_style()),
        Depth::Repo => (project_label(row), project_style()),
        Depth::Flat | Depth::Branch => return Line::from(""),
    };
    if row.prefix.is_empty() && row.guide.is_empty() {
        return banner(&label, style, width);
    }
    node_line(&row.prefix, row.guide, &label, style, width)
}

fn banner(label: &str, style: Style, width: usize) -> Line<'static> {
    let head = format!(" ┌─ {label} ");
    let head_width = head.chars().count();
    if head_width >= width {
        return Line::from(fit_spans(vec![Span::styled(head, style)], width));
    }
    Line::from(vec![
        Span::styled(head, style),
        Span::styled("─".repeat(width - head_width), style),
    ])
}

fn node_line(prefix: &str, guide: &str, label: &str, style: Style, width: usize) -> Line<'static> {
    let mut spans = tree_mark(prefix, guide, style);
    spans.push(Span::styled(label.to_string(), style));
    Line::from(fit_spans(spans, width))
}

fn tree_mark(prefix: &str, guide: &str, elbow: Style) -> Vec<Span<'static>> {
    let mark = if guide.starts_with('└') {
        "└─ "
    } else {
        "├─ "
    };
    let mut spans = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix.to_string(), rule_style()));
    }
    spans.push(Span::styled(mark, elbow));
    spans
}

fn nested_line(row: &Row, width: usize, model: &Model) -> Line<'static> {
    let style = branch_style(row);
    let mut spans = tree_mark(&row.prefix, row.guide, style);
    spans.push(Span::styled(row.title.clone(), style));
    push_tail(&mut spans, row, model);
    Line::from(fit_spans(spans, width))
}

fn push_tail(spans: &mut Vec<Span<'static>>, row: &Row, model: &Model) {
    let age = match row.age {
        Some(elapsed) => age_cell(&picker::format_age(elapsed)),
        None => age_cell(""),
    };
    let age_style = row
        .age
        .map(picker::age_style)
        .unwrap_or_else(|| Style::new().add_modifier(Modifier::DIM));
    spans.push(Span::styled(age, age_style));
    for mark in &row.marks {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(mark_label(mark), mark_style(mark)));
    }
    if let Some(path) = &row.path
        && let Some(item) = model.footprints.get(path)
    {
        spans.push(Span::raw("  "));
        spans.push(Span::raw(footprint::format_bytes(item.bytes)));
        if !item.ecosystems.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                item.ecosystems.join("+"),
                Style::new().fg(Color::Cyan),
            ));
        }
        if !item.harnesses.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                item.harnesses.join(" "),
                Style::new().fg(Color::Magenta),
            ));
        }
        for dependency in &item.dependencies {
            let style = match dependency.sharing {
                footprint::Sharing::Shared => Style::new().fg(Color::Yellow),
                footprint::Sharing::Local => Style::new().add_modifier(Modifier::DIM),
                footprint::Sharing::None => Style::new().fg(Color::DarkGray),
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(dependency.label(), style));
        }
    }
}

fn push_column(spans: &mut Vec<Span<'static>>, text: &str, width: usize, style: Style) {
    if width == 0 {
        return;
    }
    if !spans.is_empty() {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(pad(truncate(text, width), width), style));
}

fn mark_label(mark: &Mark) -> String {
    match mark {
        Mark::Dirty => "dirty".to_string(),
        Mark::Unknown => "unknown".to_string(),
        Mark::Ahead(count) => format!("+{count}"),
        Mark::Behind(count) => format!("-{count}"),
        Mark::External => "external".to_string(),
        Mark::Locked => "locked".to_string(),
        Mark::Prunable => "prunable".to_string(),
        Mark::Missing => "missing".to_string(),
    }
}

fn mark_style(mark: &Mark) -> Style {
    match mark {
        Mark::Dirty | Mark::Locked => Style::new().fg(Color::Yellow),
        Mark::Unknown => Style::new().add_modifier(Modifier::DIM),
        Mark::Ahead(_) => Style::new().fg(Color::Green),
        Mark::Behind(_) | Mark::Prunable | Mark::Missing => Style::new().fg(Color::Red),
        Mark::External => Style::new().fg(Color::Magenta),
    }
}

fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut kept = Vec::new();
    for span in spans {
        let count = span.content.chars().count();
        if used + count <= width {
            used += count;
            kept.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            let take = room.saturating_sub(1);
            let mut text: String = span.content.chars().take(take.max(1).min(room)).collect();
            if room > 1 && text.chars().count() < room {
                text.push('…');
            }
            kept.push(Span::styled(text, span.style));
        }
        break;
    }
    kept
}

struct Model {
    snapshot: Snapshot,
    filter: LineInput,
    filtering: bool,
    /// First `g` of `gg`. A second plain `g` jumps to the top.
    pending_g: bool,
    /// Home list nests host, owner, repository, then each checkout.
    tree: bool,
    /// `s` cycles this. Alphabetical is the order the list opens in.
    sort: Sort,
    view: View,
    palette: Option<Palette>,
    rows: Vec<Row>,
    cursor: usize,
    notice: Option<String>,
    footprints: HashMap<PathBuf, footprint::Footprint>,
    pending: HashSet<PathBuf>,
    owners: BTreeMap<String, owner::OwnerKind>,
    owner_cache: Option<PathBuf>,
    generation: u64,
    logged: HashSet<String>,
    /// Visible list rows from the last draw. Half-page motion uses this.
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
}

impl Sort {
    fn cycle(self) -> Self {
        match self {
            Self::Name => Self::Recent,
            Self::Recent => Self::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "a-z",
            Self::Recent => "recent",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Depth {
    Flat,
    Host,
    Owner,
    Repo,
    Branch,
}

struct Row {
    host: String,
    owner: String,
    project: String,
    count: usize,
    title: String,
    /// Tree elbow, `├ ` or `└ `. Empty on a banner and on index rows.
    guide: &'static str,
    /// Ancestor rails drawn before the elbow.
    prefix: String,
    depth: Depth,
    age: Option<Duration>,
    marks: Vec<Mark>,
    main: bool,
    path: Option<PathBuf>,
}

enum Mark {
    Dirty,
    Unknown,
    Ahead(usize),
    Behind(usize),
    External,
    Locked,
    Prunable,
    Missing,
}

#[derive(Clone, PartialEq, Eq)]
enum View {
    List,
    Detail { project: String },
}

struct Palette {
    query: LineInput,
    cursor: usize,
    scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Maintenance {
    Add,
    Remove,
    Clean,
    Prune,
    PruneAll,
}

struct CommandSpec {
    name: &'static str,
    hint: &'static str,
    command: Maintenance,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "add",
        hint: "create a worktree",
        command: Maintenance::Add,
    },
    CommandSpec {
        name: "remove",
        hint: "remove worktrees, keep branches",
        command: Maintenance::Remove,
    },
    CommandSpec {
        name: "clean",
        hint: "remove merged worktrees",
        command: Maintenance::Clean,
    },
    CommandSpec {
        name: "prune",
        hint: "drop stale registrations",
        command: Maintenance::Prune,
    },
    CommandSpec {
        name: "prune all",
        hint: "sweep every project",
        command: Maintenance::PruneAll,
    },
];

enum Effect {
    None,
    Quit,
    Refresh,
    Maintain(Maintenance),
}

struct Anchor {
    name: String,
    path: PathBuf,
}

impl Model {
    fn from_snapshot(snapshot: Snapshot) -> Self {
        let mut model = Self {
            snapshot,
            filter: LineInput::default(),
            filtering: false,
            pending_g: false,
            tree: false,
            sort: Sort::Name,
            view: View::List,
            palette: None,
            rows: Vec::new(),
            cursor: 0,
            notice: None,
            footprints: HashMap::new(),
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
        if self.palette.is_some() {
            return self.on_palette_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pending_g = self.pending_g;
        self.pending_g = false;
        if pending_g && plain_g(&key) && !self.filtering {
            self.jump(true);
            return Effect::None;
        }
        match key.code {
            KeyCode::Char('c') if ctrl => return Effect::Quit,
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Left if !self.filtering => self.move_selection(-1),
            KeyCode::Right if !self.filtering => self.move_selection(1),
            KeyCode::PageUp => self.move_rows(-self.page_delta(false)),
            KeyCode::PageDown => self.move_rows(self.page_delta(false)),
            KeyCode::Char(ch)
                if !self.filtering
                    && ctrl_letter(&key, ch)
                    && matches!(ch, 'd' | 'D' | 'e' | 'E' | 'u' | 'U' | 'y' | 'Y') =>
            {
                match ch.to_ascii_lowercase() {
                    'y' => self.move_selection(-1),
                    'e' => self.move_selection(1),
                    'u' => self.move_rows(-self.page_delta(true)),
                    'd' => self.move_rows(self.page_delta(true)),
                    _ => {}
                }
            }
            KeyCode::Home if !self.filtering => self.jump(true),
            KeyCode::End if !self.filtering => self.jump(false),
            KeyCode::Char('p') if ctrl => self.move_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_selection(1),
            KeyCode::Char('k') if !self.filtering => self.move_selection(-1),
            KeyCode::Char('j') if !self.filtering => self.move_selection(1),
            KeyCode::Char('g') if plain_g(&key) && !self.filtering => self.pending_g = true,
            KeyCode::Char('g' | 'G') if shift_g(&key) && !self.filtering => self.jump(false),
            KeyCode::Enter | KeyCode::Tab => self.go_in(),
            KeyCode::Char('i') if ctrl => self.go_in(),
            KeyCode::Esc => self.go_up(),
            KeyCode::Char('o') if ctrl => self.go_up(),
            _ if self.filtering && self.filter.edit(&key) => self.rebuild(),
            KeyCode::Char('/') if !self.filtering => self.filtering = true,
            KeyCode::Char('q') if !self.filtering && !ctrl => return Effect::Quit,
            KeyCode::Char('r') if !self.filtering && !ctrl => return Effect::Refresh,
            KeyCode::Char('t') if !self.filtering && !ctrl => {
                self.tree = !self.tree;
                self.view = View::List;
                self.rebuild();
            }
            KeyCode::Char('s') if !self.filtering && !ctrl => {
                self.sort = self.sort.cycle();
                self.rebuild();
            }
            KeyCode::Char('o') if !self.filtering && !ctrl => self.go_in(),
            KeyCode::Char(':') if !self.filtering && !ctrl => {
                self.palette = Some(Palette {
                    query: LineInput::default(),
                    cursor: 0,
                    scroll: 0,
                });
            }
            _ => {}
        }
        Effect::None
    }

    fn on_palette_key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Effect::Quit,
            KeyCode::Esc => self.go_up(),
            KeyCode::Char('o') if ctrl => self.go_up(),
            KeyCode::Enter => {
                let Some(command) = self.selected_command() else {
                    return Effect::None;
                };
                self.palette = None;
                return Effect::Maintain(command);
            }
            KeyCode::Up | KeyCode::Char('p') if ctrl || matches!(key.code, KeyCode::Up) => {
                self.move_palette(-1);
            }
            KeyCode::Down | KeyCode::Char('n') if ctrl || matches!(key.code, KeyCode::Down) => {
                self.move_palette(1);
            }
            KeyCode::PageUp => self.move_palette(-self.page_delta(false)),
            KeyCode::PageDown => self.move_palette(self.page_delta(false)),
            KeyCode::Home
                if self
                    .palette
                    .as_ref()
                    .is_some_and(|palette| palette.query.is_empty()) =>
            {
                self.jump_palette(true);
            }
            KeyCode::End
                if self
                    .palette
                    .as_ref()
                    .is_some_and(|palette| palette.query.is_empty()) =>
            {
                self.jump_palette(false);
            }
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

    fn apply_footprint(&mut self, update: footprint::Measured) -> bool {
        if update.generation != self.generation {
            return false;
        }
        self.footprints.insert(update.path, update.footprint);
        self.rebuild();
        true
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
        let head = if let Some(path) = self.selected_path() {
            let label = path_label(&path);
            if let Some(item) = self.footprints.get(&path) {
                format!("{label}  {}", item.summary())
            } else {
                label
            }
        } else {
            self.snapshot.root.display().to_string()
        };
        with_warnings(&head, &self.snapshot.warnings)
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
        self.rows = visible_rows(self, now_secs());
        self.cursor = self
            .rows
            .iter()
            .position(|row| row.path.is_some() && row.path == selected)
            .or_else(|| self.rows.iter().position(|row| row.path.is_some()))
            .unwrap_or(0);
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
        if let View::Detail { project } = &self.view {
            return self
                .snapshot
                .projects
                .iter()
                .find(|item| &item.id == project);
        }
        let path = self.selected_path()?;
        self.snapshot.projects.iter().find(|project| {
            project
                .worktrees
                .iter()
                .any(|worktree| worktree.path == path)
        })
    }

    fn selected_anchor(&self) -> Option<Anchor> {
        self.selected_project().map(|project| Anchor {
            name: project.name.clone(),
            path: project.path.clone(),
        })
    }

    /// Open the selected project. Enter, `o`, and Ctrl-I all do this.
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

    /// Leave one level: the command line, then a search, then a project.
    fn go_up(&mut self) {
        if self.palette.is_some() {
            self.palette = None;
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
        let next = if delta > 0 {
            selectable.iter().copied().find(|index| *index >= target)
        } else {
            selectable
                .iter()
                .copied()
                .rev()
                .find(|index| *index <= target)
        };
        self.cursor = next.unwrap_or(if delta > 0 {
            selectable[selectable.len() - 1]
        } else {
            fallback
        });
    }

    fn jump(&mut self, first: bool) {
        let selectable = self.selectable();
        if let Some(index) = if first {
            selectable.first()
        } else {
            selectable.last()
        } {
            self.cursor = *index;
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

    fn jump_palette(&mut self, first: bool) {
        let len = palette_matches(self).len();
        let Some(palette) = &mut self.palette else {
            return;
        };
        palette.cursor = if first || len == 0 { 0 } else { len - 1 };
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

    fn selected_command(&self) -> Option<Maintenance> {
        palette_matches(self)
            .get(self.palette.as_ref()?.cursor)
            .map(|spec| spec.command)
    }
}

fn palette_matches(model: &Model) -> Vec<&CommandSpec> {
    let Some(palette) = &model.palette else {
        return Vec::new();
    };
    let query = palette.query.text().trim().to_lowercase();
    COMMANDS
        .iter()
        .filter(|spec| {
            let haystack = format!("{} {}", spec.name, spec.hint).to_lowercase();
            picker::fuzzy_match(&haystack, &query)
        })
        .collect()
}

fn visible_rows(model: &Model, now: u64) -> Vec<Row> {
    let query = owner::parse_query(model.filter.text());
    let list = matches!(model.view, View::List) && query_is_empty(&query);
    let overview = list && !model.tree;
    let tree = list && model.tree;
    let detail = matches!(model.view, View::Detail { .. });
    let mut projects: Vec<&Project> = model.snapshot.projects.iter().collect();
    sort_projects(&mut projects, model.sort);
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
        sort_worktrees(&mut worktrees, model.sort);
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
        });
    }
    if tree {
        tree_rows(&shown, &model.pending, now)
    } else {
        list_rows(&shown, overview, detail, &model.pending, now)
    }
}

struct Shown<'a> {
    host: String,
    owner: String,
    project: &'a Project,
    matched: Vec<&'a WorktreeEntry>,
}

fn list_rows(
    shown: &[Shown<'_>],
    overview: bool,
    detail: bool,
    pending: &HashSet<PathBuf>,
    now: u64,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for item in shown {
        let worktrees = if overview {
            &item.matched[..1]
        } else {
            item.matched.as_slice()
        };
        for worktree in worktrees {
            let mut row = empty_row();
            if !detail {
                row.host.clone_from(&item.host);
                row.owner.clone_from(&item.owner);
                row.project.clone_from(&item.project.name);
            }
            row.count = if overview {
                item.project.worktrees.len()
            } else {
                0
            };
            if !overview {
                row.title = branch_label(worktree);
            }
            row.age = age_elapsed(worktree.last_activity, now);
            row.marks = marks_for(worktree, pending.contains(&worktree.path));
            row.main = worktree.main;
            row.path = Some(worktree.path.clone());
            rows.push(row);
        }
    }
    rows
}

/// Inset and continuing bar under a host or repository banner.
const TREE_RAIL: &str = " │  ";

fn tree_elbow(continued: bool) -> &'static str {
    if continued { "├ " } else { "└ " }
}

fn tree_rail(continued: bool) -> &'static str {
    if continued { "│  " } else { "   " }
}

fn child_prefix(prefix: &str, continued: bool) -> String {
    format!("{prefix}{}", tree_rail(continued))
}

enum Child<'a> {
    Owner {
        name: String,
        repos: Vec<&'a Shown<'a>>,
    },
    Repo(&'a Shown<'a>),
}

fn tree_rows(shown: &[Shown<'_>], pending: &HashSet<PathBuf>, now: u64) -> Vec<Row> {
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
                push_repo_banner(&mut rows, item, pending, now);
            }
            continue;
        }
        push_host(&mut rows, host);
        let children = host_children(&items);
        let last = children.len().saturating_sub(1);
        for (index, child) in children.into_iter().enumerate() {
            let continued = index != last;
            match child {
                Child::Owner { name, repos } => {
                    push_owner(&mut rows, host, &name, continued);
                    let prefix = child_prefix(TREE_RAIL, continued);
                    let repo_last = repos.len().saturating_sub(1);
                    for (repo_index, repo) in repos.iter().enumerate() {
                        push_repo(
                            &mut rows,
                            repo,
                            &prefix,
                            repo_index != repo_last,
                            pending,
                            now,
                        );
                    }
                }
                Child::Repo(item) => push_repo(&mut rows, item, TREE_RAIL, continued, pending, now),
            }
        }
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

fn push_host(rows: &mut Vec<Row>, host: &str) {
    let mut row = empty_row();
    row.depth = Depth::Host;
    row.host = host.to_string();
    rows.push(row);
}

fn push_owner(rows: &mut Vec<Row>, host: &str, owner: &str, continued: bool) {
    let mut row = empty_row();
    row.depth = Depth::Owner;
    row.host = host.to_string();
    row.owner = owner.to_string();
    row.prefix = TREE_RAIL.to_string();
    row.guide = tree_elbow(continued);
    rows.push(row);
}

fn push_repo_banner(rows: &mut Vec<Row>, item: &Shown<'_>, pending: &HashSet<PathBuf>, now: u64) {
    let mut row = empty_row();
    row.depth = Depth::Repo;
    row.project.clone_from(&item.project.name);
    row.count = item.matched.len();
    rows.push(row);
    push_branches(rows, item, TREE_RAIL, pending, now);
}

fn push_repo(
    rows: &mut Vec<Row>,
    item: &Shown<'_>,
    prefix: &str,
    continued: bool,
    pending: &HashSet<PathBuf>,
    now: u64,
) {
    let mut row = empty_row();
    row.depth = Depth::Repo;
    row.host.clone_from(&item.host);
    row.owner.clone_from(&item.owner);
    row.project.clone_from(&item.project.name);
    row.count = item.matched.len();
    row.prefix = prefix.to_string();
    row.guide = tree_elbow(continued);
    rows.push(row);
    push_branches(rows, item, &child_prefix(prefix, continued), pending, now);
}

fn push_branches(
    rows: &mut Vec<Row>,
    item: &Shown<'_>,
    prefix: &str,
    pending: &HashSet<PathBuf>,
    now: u64,
) {
    let last = item.matched.len().saturating_sub(1);
    for (index, worktree) in item.matched.iter().enumerate() {
        rows.push(branch_row(
            worktree,
            prefix,
            tree_elbow(index != last),
            pending,
            now,
        ));
    }
}

fn branch_row(
    worktree: &WorktreeEntry,
    prefix: &str,
    guide: &'static str,
    pending: &HashSet<PathBuf>,
    now: u64,
) -> Row {
    let mut row = empty_row();
    row.depth = Depth::Branch;
    row.title = branch_label(worktree);
    row.guide = guide;
    row.prefix = prefix.to_string();
    row.age = age_elapsed(worktree.last_activity, now);
    row.marks = marks_for(worktree, pending.contains(&worktree.path));
    row.main = worktree.main;
    row.path = Some(worktree.path.clone());
    row
}

fn empty_row() -> Row {
    Row {
        host: String::new(),
        owner: String::new(),
        project: String::new(),
        count: 0,
        title: String::new(),
        guide: "",
        prefix: String::new(),
        depth: Depth::Flat,
        age: None,
        marks: Vec::new(),
        main: false,
        path: None,
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

fn sort_projects(projects: &mut [&Project], sort: Sort) {
    match sort {
        Sort::Recent => projects.sort_by(|left, right| {
            project_activity(right)
                .cmp(&project_activity(left))
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        }),
        Sort::Name => projects.sort_by(|left, right| {
            host_key(left)
                .cmp(&host_key(right))
                .then_with(|| owner_key(left).cmp(&owner_key(right)))
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        }),
    }
}

fn sort_worktrees(worktrees: &mut [&WorktreeEntry], sort: Sort) {
    match sort {
        Sort::Recent => worktrees.sort_by(|left, right| {
            activity(right)
                .cmp(&activity(left))
                .then_with(|| branch_order(&left.branch, &right.branch))
                .then_with(|| left.path.cmp(&right.path))
        }),
        Sort::Name => worktrees.sort_by(|left, right| {
            branch_order(&left.branch, &right.branch).then_with(|| left.path.cmp(&right.path))
        }),
    }
}

fn age_elapsed(activity: Option<u64>, now: u64) -> Option<Duration> {
    activity.map(|activity| Duration::from_secs(now.saturating_sub(activity)))
}

fn marks_for(worktree: &WorktreeEntry, pending: bool) -> Vec<Mark> {
    if worktree.missing {
        let mut marks = vec![Mark::Missing];
        if worktree.external {
            marks.push(Mark::External);
        }
        if worktree.locked {
            marks.push(Mark::Locked);
        }
        return marks;
    }
    let mut marks = Vec::new();
    if !pending {
        match worktree.dirty {
            Some(true) => marks.push(Mark::Dirty),
            None => marks.push(Mark::Unknown),
            Some(false) => {}
        }
        if worktree.ahead > 0 {
            marks.push(Mark::Ahead(worktree.ahead));
        }
        if worktree.behind > 0 {
            marks.push(Mark::Behind(worktree.behind));
        }
    }
    if worktree.external {
        marks.push(Mark::External);
    }
    if worktree.locked {
        marks.push(Mark::Locked);
    }
    if worktree.prunable {
        marks.push(Mark::Prunable);
    }
    marks
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
    footprint: Option<&footprint::Footprint>,
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

fn branch_label(worktree: &WorktreeEntry) -> String {
    if let Some(branch) = &worktree.branch {
        return branch.clone();
    }
    match worktree.head.chars().take(7).collect::<String>() {
        short if short.is_empty() => "(detached)".to_string(),
        short => format!("(detached {short})"),
    }
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

fn run_maintenance(
    config: &Config,
    anchor: Option<&Anchor>,
    command: Maintenance,
) -> Result<String> {
    use crate::scope::{Project, Scope};
    if command == Maintenance::PruneAll {
        crate::commands::prune::run(config, &Scope::resolve(config, None, true, None), false)?;
        return Ok("pruned every project".to_string());
    }
    let anchor = anchor.context("select a project first")?;
    let scope = Scope::Current(Box::new(Project {
        repo: open_repo(anchor)?,
        config: config.clone(),
    }));
    match command {
        Maintenance::Add => {
            let Scope::Current(project) = &scope else {
                unreachable!("a single project is in scope")
            };
            let created =
                crate::commands::add::run(config, &project.repo, None, None, false, None)?;
            Ok(match created {
                Some(path) => format!("worktree ready at {}", path.display()),
                None => "add finished".to_string(),
            })
        }
        Maintenance::Remove => {
            crate::commands::remove::run(config, &scope, Vec::new(), false, false)?;
            Ok("remove finished".to_string())
        }
        Maintenance::Clean => {
            crate::commands::clean::run(config, &scope, false, false, false, false)?;
            Ok("clean finished".to_string())
        }
        Maintenance::Prune => {
            crate::commands::prune::run(config, &scope, false)?;
            Ok("pruned this project".to_string())
        }
        Maintenance::PruneAll => unreachable!("handled above"),
    }
}

fn open_repo(anchor: &Anchor) -> Result<crate::repo::Repo> {
    let git = crate::git::Git::at(&anchor.path);
    let bytes = git
        .out_bytes(&["worktree", "list", "--porcelain", "-z"])
        .with_context(|| format!("cannot read worktrees for {}", anchor.name))?;
    let registered = crate::worktree::Worktree::parse_list(&bytes);
    crate::repo::Repo::from_worktrees(git, registered, Some(&anchor.path))
        .with_context(|| format!("cannot open {}", anchor.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint(model: &mut Model, width: u16, height: u16) -> (String, ratatui::buffer::Buffer) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, model)).unwrap();
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
        paint(model, 80, 24)
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

    fn span_style(line: &Line<'_>, needle: &str) -> Style {
        line.spans
            .iter()
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle}"))
            .style
    }

    fn meta(row: &Row) -> String {
        row.marks
            .iter()
            .map(mark_label)
            .collect::<Vec<_>>()
            .join(" ")
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

    #[test]
    fn enter_and_o_open_the_same_project() {
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
        assert!(model.selected_path().unwrap().ends_with("main"));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('o'))),
            Effect::None
        ));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(press(KeyCode::Esc));
        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('o')));
        assert_eq!(model.filter.text(), "o");
        assert!(matches!(model.view, View::List));
    }

    #[test]
    fn ctrl_i_goes_into_the_project_and_ctrl_o_and_escape_go_up() {
        let mut model = Model::from_snapshot(sample());
        let ctrl_i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL);
        let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);

        model.on_key(ctrl_i);
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(ctrl_o);
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Tab));
        assert!(matches!(&model.view, View::Detail { project } if project == "bonsai"));
        model.on_key(press(KeyCode::Esc));
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('d')));
        model.on_key(ctrl_o);
        assert!(model.filter.is_empty());
        assert!(!model.filtering);
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Enter));
        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('m')));
        assert!(matches!(model.view, View::Detail { .. }));
        model.on_key(press(KeyCode::Esc));
        assert!(model.filter.is_empty());
        assert!(matches!(model.view, View::Detail { .. }));
        model.on_key(ctrl_o);
        assert!(matches!(model.view, View::List));

        model.on_key(press(KeyCode::Char(':')));
        assert!(model.palette.is_some());
        model.on_key(ctrl_o);
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
        for ch in ['d', 'e', 'v'] {
            model.on_key(press(KeyCode::Char(ch)));
        }
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
    fn arrows_and_vim_half_page_move_the_list() {
        let mut projects = Vec::new();
        for index in 0..20 {
            let name = format!("p{index:02}");
            projects.push(Project {
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
            });
        }
        let mut model = Model::from_snapshot(Snapshot {
            root: "/trees".into(),
            warnings: Vec::new(),
            projects,
        });
        model.viewport = 10;
        let selected = |model: &Model| model.rows[model.cursor].project.clone();
        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        assert_eq!(selected(&model), "p00");

        model.on_key(press(KeyCode::Down));
        model.on_key(press(KeyCode::Right));
        assert_eq!(selected(&model), "p02");
        model.on_key(press(KeyCode::Up));
        model.on_key(press(KeyCode::Left));
        assert_eq!(selected(&model), "p00");
        model.on_key(ctrl('e'));
        assert_eq!(selected(&model), "p01");
        model.on_key(ctrl('y'));
        assert_eq!(selected(&model), "p00");
        model.on_key(ctrl('d'));
        assert_eq!(selected(&model), "p05");
        model.on_key(ctrl('u'));
        assert_eq!(selected(&model), "p00");
        model.on_key(press(KeyCode::Char('G')));
        assert_eq!(selected(&model), "p19");
        model.on_key(press(KeyCode::Char('g')));
        assert_eq!(selected(&model), "p19");
        model.on_key(press(KeyCode::Char('g')));
        assert_eq!(selected(&model), "p00");
        model.on_key(press(KeyCode::Char('g')));
        model.on_key(press(KeyCode::Char('j')));
        assert_eq!(selected(&model), "p01");
        model.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT));
        assert_eq!(selected(&model), "p19");

        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('p')));
        model.on_key(press(KeyCode::Char('1')));
        model.on_key(ctrl('e'));
        model.on_key(ctrl('d'));
        assert_eq!(model.filter.text(), "p1");
        assert!(model.filtering);
        model.on_key(ctrl('a'));
        model.on_key(ctrl('d'));
        assert_eq!(model.filter.text(), "1");
        model.on_key(press(KeyCode::Left));
        assert_eq!(model.filter.text(), "1");
        model.on_key(press(KeyCode::Char('g')));
        model.on_key(press(KeyCode::Char('G')));
        assert_eq!(model.filter.text(), "gG1");
        assert!(model.filtering);
    }

    #[test]
    fn gg_and_half_page_move_through_the_tree() {
        let mut projects = Vec::new();
        for index in 0..20 {
            let name = format!("p{index:02}");
            projects.push(Project {
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
            });
        }
        let mut model = Model::from_snapshot(Snapshot {
            root: "/trees".into(),
            warnings: Vec::new(),
            projects,
        });
        model.on_key(press(KeyCode::Char('t')));
        model.viewport = 10;
        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        let path = |model: &Model| model.selected_path().unwrap();
        assert!(path(&model).ends_with("p00/main"));

        model.on_key(ctrl('e'));
        assert!(path(&model).ends_with("p01/main"));
        model.on_key(ctrl('y'));
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
        let mut projects = Vec::new();
        for index in 0..40 {
            let name = format!("p{index:02}");
            projects.push(Project {
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
            });
        }
        let mut model = Model::from_snapshot(Snapshot {
            root: "/trees".into(),
            warnings: Vec::new(),
            projects,
        });
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
        assert_eq!(model.rows[model.cursor].project, format!("p{last_safe:02}"));
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
        assert_eq!(model.rows[model.cursor].project, "p00");
    }

    #[test]
    fn tree_labels_use_the_table_colors() {
        let mut model = Model::from_snapshot(sample());
        let width = 120;
        let columns = columns_for(&model.rows, width);
        let home = summary_line(
            model
                .rows
                .iter()
                .find(|row| row.project == "bonsai")
                .unwrap(),
            width,
            &model,
            columns,
        );
        model.on_key(press(KeyCode::Char('t')));
        let line = |model: &Model, pred: fn(&Row) -> bool| {
            summary_line(
                model.rows.iter().find(|row| pred(row)).unwrap(),
                width,
                model,
                columns_for(&model.rows, width),
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
        assert_eq!(span_style(&dev, "dirty"), span_style(&home, "dirty"));
        assert_eq!(span_style(&home, "github.com").fg, Some(Color::LightBlue));
        assert_eq!(
            span_style(&home, "aymericbeaumet").fg,
            Some(Color::LightMagenta)
        );
        assert_eq!(span_style(&home, "bonsai").fg, Some(Color::LightGreen));
        let header = column_header(columns, width, false);
        assert_eq!(span_style(&header, "host").fg, Some(Color::LightBlue));
        assert_eq!(span_style(&header, "owner").fg, Some(Color::LightMagenta));
        assert_eq!(span_style(&header, "project").fg, Some(Color::LightGreen));

        model.on_key(press(KeyCode::Enter));
        let detail_dev = line(&model, |row| row.title == "ab/dev");
        let detail_main = line(&model, |row| row.title == "main");
        assert_eq!(
            span_style(&dev, "ab/dev"),
            span_style(&detail_dev, "ab/dev")
        );
        assert_eq!(span_style(&main, "main"), span_style(&detail_main, "main"));
        assert_eq!(span_style(&dev, "├"), span_style(&dev, "ab/dev"));
        assert_eq!(span_style(&main, "└"), span_style(&main, "main"));
    }

    #[test]
    fn readline_edits_the_filter_and_the_command_line() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        for ch in "ab dev".chars() {
            model.on_key(press(KeyCode::Char(ch)));
        }
        model.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(model.filter.text(), "ab ");
        model.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        model.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(model.filter.text(), "ab");
        model.on_key(press(KeyCode::Esc));

        model.on_key(press(KeyCode::Char(':')));
        for ch in "pru extra".chars() {
            model.on_key(press(KeyCode::Char(ch)));
        }
        model.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(matches!(
            model.on_key(press(KeyCode::Enter)),
            Effect::Maintain(Maintenance::Prune)
        ));
    }

    #[test]
    fn fuzzy_find_matches_a_subsequence() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        for ch in ['a', 'd', 'v'] {
            model.on_key(press(KeyCode::Char(ch)));
        }
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
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(model.sort, Sort::Name);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/trees/bonsai/ab/dev"),
                PathBuf::from("/trees/other/feature"),
            ]
        );
        model.on_key(press(KeyCode::Char('s')));
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(model.sort, Sort::Recent);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/trees/other/feature"),
                PathBuf::from("/trees/bonsai/main"),
            ]
        );
        model.on_key(press(KeyCode::Char('j')));
        model.on_key(press(KeyCode::Enter));
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/trees/bonsai/main"),
                PathBuf::from("/trees/bonsai/ab/dev"),
            ]
        );
        model.on_key(press(KeyCode::Char('s')));
        let paths: Vec<_> = model
            .rows
            .iter()
            .filter_map(|row| row.path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/trees/bonsai/ab/dev"),
                PathBuf::from("/trees/bonsai/main"),
            ]
        );
        model.on_key(press(KeyCode::Char('/')));
        model.on_key(press(KeyCode::Char('s')));
        assert_eq!(model.filter.text(), "s");
        assert_eq!(model.sort, Sort::Name);
    }

    #[test]
    fn enter_on_an_empty_filter_does_nothing_and_r_refreshes() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char('/')));
        for ch in "missing".chars() {
            model.on_key(press(KeyCode::Char(ch)));
        }
        assert!(matches!(model.on_key(press(KeyCode::Enter)), Effect::None));
        assert!(matches!(model.view, View::List));
        model.on_key(press(KeyCode::Esc));
        assert!(matches!(
            model.on_key(press(KeyCode::Char('r'))),
            Effect::Refresh
        ));
    }

    #[test]
    fn rows_mark_dirty_external_worktrees_and_warnings() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].external = true;
        let model = Model::from_snapshot(snapshot);
        let dev = model
            .rows
            .iter()
            .find(|row| {
                row.path
                    .as_ref()
                    .is_some_and(|path| path.ends_with("ab/dev"))
            })
            .unwrap();
        assert!(meta(dev).contains("dirty"));
        assert!(meta(dev).contains("external"));
        assert!(model.status_line().contains("unreadable"));
    }

    #[test]
    fn pending_status_omits_dirty_and_unknown() {
        let mut snapshot = sample();
        snapshot.projects[1].worktrees[0].dirty = None;
        let mut model = Model::from_snapshot(snapshot);
        let feature = PathBuf::from("/trees/other/feature");
        let dev = PathBuf::from("/trees/bonsai/ab/dev");
        model.pending.insert(feature.clone());
        model.pending.insert(dev.clone());
        model.rebuild();
        let feature_row = model
            .rows
            .iter()
            .find(|row| row.path.as_ref() == Some(&feature))
            .unwrap();
        let dev_row = model
            .rows
            .iter()
            .find(|row| row.path.as_ref() == Some(&dev))
            .unwrap();
        assert!(!meta(feature_row).contains("unknown"));
        assert!(!meta(dev_row).contains("dirty"));
    }

    #[test]
    fn colon_runs_the_matching_maintenance_command() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Char(':')));
        for ch in ['p', 'r', 'u'] {
            model.on_key(press(KeyCode::Char(ch)));
        }
        assert!(matches!(
            model.on_key(press(KeyCode::Enter)),
            Effect::Maintain(Maintenance::Prune)
        ));
        assert!(model.palette.is_none());
    }

    #[test]
    fn status_updates_fill_dirty_and_ignore_stale_generations() {
        let mut model = Model::from_snapshot(sample());
        model.on_key(press(KeyCode::Enter));
        let path = PathBuf::from("/trees/bonsai/main");
        model.generation = 3;
        model.pending.insert(path.clone());
        model.snapshot.projects[0].worktrees[1].dirty = None;
        assert!(!model.apply_status(inventory::StatusUpdate {
            generation: 1,
            path: path.clone(),
            dirty: Some(true),
            added: 0,
            modified: 1,
            deleted: 0,
            untracked: 0,
            ahead: 0,
            behind: 0,
            warning: None,
        }));
        assert!(model.pending.contains(&path));
        assert!(model.apply_status(inventory::StatusUpdate {
            generation: 3,
            path: path.clone(),
            dirty: Some(true),
            added: 0,
            modified: 1,
            deleted: 0,
            untracked: 0,
            ahead: 2,
            behind: 0,
            warning: None,
        }));
        assert!(!model.pending.contains(&path));
        let main = model
            .rows
            .iter()
            .find(|row| row.path.as_ref() == Some(&path))
            .unwrap();
        assert!(meta(main).contains("dirty"));
        assert!(meta(main).contains("+2"));
    }

    #[test]
    fn column_titles_align_with_their_cells() {
        let model = Model::from_snapshot(sample());
        let columns = columns_for(&model.rows, 80);
        let header = column_header(columns, 80, false).to_string();
        let row = summary_line(&model.rows[0], 80, &model, columns).to_string();
        assert_eq!(header.find("host"), row.find("github.com"));
        assert_eq!(header.find("owner"), row.find("aymericbeaumet"));
        assert_eq!(header.find("project"), row.find("bonsai"));
        assert!(!header.contains("branch"));

        let mut detail = Model::from_snapshot(sample());
        detail.on_key(press(KeyCode::Enter));
        let columns = columns_for(&detail.rows, 80);
        let header = column_header(columns, 80, true).to_string();
        let row = summary_line(&detail.rows[0], 80, &detail, columns).to_string();
        assert_eq!(header.find("branch"), row.find("ab/dev"));
        assert!(!header.contains("host"));
        assert!(!header.contains("owner"));
        assert!(!header.contains("project"));
        assert!(header.contains("age"));
    }

    #[test]
    fn draws_the_selected_worktree_and_keys() {
        let mut model = Model::from_snapshot(sample());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut model)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let host_at = screen.find("github.com").expect("host column");
        let owner_at = screen.find("aymericbeaumet").expect("owner column");
        assert!(host_at < owner_at);
        assert!(screen.contains("host"));
        assert!(screen.contains("owner"));
        assert!(screen.contains("project"));
        assert!(screen.contains("age"));
        assert!(screen.contains("aymericbeaumet"));
        assert!(screen.contains("interfaceinc"));
        assert!(screen.contains("bonsai"));
        assert!(screen.contains("other"));
        assert!(!screen.contains("feature"));
        assert!(screen.contains("ctrl-i/enter in"));
        assert!(screen.contains("ctrl-o/esc up"));
        assert!(screen.contains("/trees/bonsai/ab/dev"));
        assert!(!screen.contains("main"));
        assert!(!screen.contains("/trees/other"));
        assert!(!screen.contains("measuring"));
        assert!(!screen.contains("enter shell"));
        assert!(!screen.contains("┌─"));
        assert!(!screen.contains("├─"));
    }

    #[test]
    fn expanded_views_use_a_ruled_frame() {
        let mut model = Model::from_snapshot(sample());
        let (root, root_buf) = painted(&mut model);
        assert!(root.contains("host"));
        assert!(!root.contains("┌─"));
        assert_eq!(root_buf[(0, 0)].style().fg, Some(Color::Reset));
        let y = row_index(&root, "github.com");
        let home = root_buf[(1, y)].style();
        assert!(home.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(home.bg, Some(Color::Reset));

        model.on_key(press(KeyCode::Char('t')));
        let (tree, tree_buf) = painted(&mut model);
        assert!(tree.contains(" bonsai · tree "));
        assert!(tree.contains("┌─ github.com"));
        assert!(tree.contains("│  ├─ aymericbeaumet"));
        assert!(tree.contains("│  │  └─ bonsai · 2"));
        assert!(tree.contains("│  │     ├─ ab/dev"));
        assert!(tree.contains("│  │     └─ main"));
        assert!(tree.contains("│  └─ interfaceinc"));
        assert!(tree.contains("│     └─ other · 1"));
        assert!(tree.contains("│        └─ feature"));
        let owner_at = tree.find("├─ aymericbeaumet").expect("owner");
        let repo_at = tree.find("└─ bonsai · 2").expect("repository");
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
        assert!(detail.contains(" bonsai · bonsai "));
        assert!(detail.contains("git@github.com:aymericbeaumet/bonsai.git"));
        let lines: Vec<&str> = detail.lines().collect();
        let path_at = lines
            .iter()
            .position(|line| line.contains("/trees/bonsai"))
            .expect("project path");
        let rule = lines[path_at + 1];
        assert!(rule.chars().filter(|ch| *ch == '─').count() >= 30);
        assert!(rule.chars().all(|ch| matches!(ch, '─' | '│')));
        assert!(detail.contains("│ branch"));
        assert!(detail.contains("│ ab/dev"));
        assert!(detail.contains("│ main"));
        assert!(!detail.contains("┌─"));
        assert!(!detail.contains("host"));
        assert_eq!(detail_buf[(0, 0)].style().fg, Some(Color::Cyan));
        let y = row_index(&detail, "│ ab/dev");
        assert_eq!(detail_buf[(1, y)].style().bg, Some(Color::Cyan));
        assert_eq!(detail_buf[(1, y)].style().fg, Some(Color::Black));

        model.on_key(press(KeyCode::Esc));
        model.on_key(press(KeyCode::Char('/')));
        for ch in ['f', 'e', 'a', 't'] {
            model.on_key(press(KeyCode::Char(ch)));
        }
        let (found, found_buf) = painted(&mut model);
        assert!(found.contains("feature"));
        assert!(found.contains("host"));
        assert!(!found.contains("┌─"));
        assert!(!found.contains("├─"));
        assert_eq!(found_buf[(0, 0)].style().fg, Some(Color::Reset));
        let y = row_index(&found, "feature");
        let matched = found_buf[(1, y)].style();
        assert!(matched.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(matched.bg, Some(Color::Reset));
    }

    #[test]
    fn a_measured_footprint_is_filterable_and_stale_results_are_ignored() {
        let mut model = Model::from_snapshot(sample());
        model.generation = 2;
        let stale = footprint::Measured {
            generation: 1,
            path: "/trees/bonsai/main".into(),
            footprint: footprint::Footprint {
                ecosystems: vec!["rust".to_string()],
                bytes: 1536,
                dependencies: vec![],
                harnesses: vec!["claude".to_string(), "grok".to_string()],
            },
        };
        assert!(!model.apply_footprint(stale));
        assert!(model.footprints.is_empty());
        assert!(model.apply_footprint(footprint::Measured {
            generation: 2,
            path: "/trees/bonsai/main".into(),
            footprint: footprint::Footprint {
                ecosystems: vec!["rust".to_string()],
                bytes: 1536,
                dependencies: vec![],
                harnesses: vec!["claude".to_string(), "grok".to_string()],
            },
        }));
        let summary = model
            .footprints
            .get(Path::new("/trees/bonsai/main"))
            .unwrap()
            .summary();
        assert!(summary.contains("rust"));
        assert!(summary.contains("1.5K"));
        assert!(summary.contains("claude grok"));
        model.on_key(press(KeyCode::Enter));
        model.on_key(press(KeyCode::Char('j')));
        assert!(model.status_line().contains("claude grok"));
        assert!(model.status_line().contains("unreadable"));
        model.on_key(press(KeyCode::Char('/')));
        for ch in "claude".chars() {
            model.on_key(press(KeyCode::Char(ch)));
        }
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
        assert_eq!(model.rows[0].host, "github.com");
        assert_eq!(model.rows[0].owner, "aymericbeaumet");
        assert_eq!(model.rows[0].project, "bonsai");
        assert_eq!(model.rows[0].title, "");
        assert!(!model.tree);
        assert_eq!(model.rows[0].count, 2);
        assert_eq!(model.rows[1].host, "github.com");
        assert_eq!(model.rows[1].owner, "interfaceinc");
        assert_eq!(model.rows[1].title, "");
        assert_eq!(model.rows[1].count, 1);
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
        for ch in "gitlab".chars() {
            model.on_key(press(KeyCode::Char(ch)));
        }
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
    fn a_missing_checkout_is_marked_without_status_flags() {
        let mut snapshot = sample();
        snapshot.projects[0].worktrees[0].missing = true;
        snapshot.projects[0].worktrees[0].prunable = true;
        snapshot.projects[0].worktrees[0].dirty = None;
        let model = Model::from_snapshot(snapshot);
        let dev = model
            .rows
            .iter()
            .find(|row| {
                row.path
                    .as_ref()
                    .is_some_and(|path| path.ends_with("ab/dev"))
            })
            .unwrap();
        assert!(meta(dev).contains("missing"));
        assert!(!meta(dev).contains("prunable"));
        assert!(!meta(dev).contains("unknown"));
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
}
