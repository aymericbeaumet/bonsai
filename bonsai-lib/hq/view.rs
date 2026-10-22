//! Drawing: the framed list, the project header, the output pane, the key
//! hints and status line, and the help, confirmation, and command overlays.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use super::keys::Pager;
use super::rows::Info;
use super::table::{self, Kind};
use super::{Model, View, footprint, jobs, palette_matches, path_label, truncate};

pub(super) fn draw_message(frame: &mut Frame, message: &str) {
    frame.render_widget(Paragraph::new(message), frame.area());
}

pub(super) fn draw(frame: &mut Frame, model: &mut Model) {
    let area = frame.area();
    if area.width < 24 || area.height < 6 {
        model.viewport = 0;
        frame.render_widget(
            Paragraph::new("bonsai\nEnlarge the terminal.\nq quit"),
            area,
        );
        return;
    }
    let pane_height = model
        .pane
        .as_ref()
        .map_or(0, |_| (area.height / 3).clamp(4, 12));
    let layout = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(pane_height),
        Constraint::Length(2),
    ])
    .split(area);
    let mut block = Block::bordered().title(title(model));
    // The views `t` and `T` step through, on the top border when they fit.
    if let Some(tabs) = tabs(model)
        && title(model).width() + tabs.width() + 4 <= usize::from(area.width)
    {
        block = block.title(tabs.right_aligned());
    }
    if framed(model) {
        block = block.border_style(Style::new().fg(Color::Cyan));
    }
    let inner = block.inner(layout[0]);
    let header = if model.palette.is_none() && matches!(model.view, View::Detail { .. }) {
        detail_header(model, usize::from(inner.width))
    } else {
        Vec::new()
    };
    let split = Layout::vertical([
        Constraint::Length(u16::try_from(header.len()).unwrap_or(0)),
        Constraint::Min(1),
    ])
    .split(inner);
    let (header_area, list_area) = (split[0], split[1]);
    let width = usize::from(list_area.width);
    let columns = table::layout(kind(model), &model.rows, width);
    let show_columns = model.palette.is_none() && !model.rows.is_empty() && list_area.height > 1;
    let (title_area, list_area) = if show_columns {
        let split = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(list_area);
        (Some(split[0]), split[1])
    } else {
        (None, list_area)
    };
    model.viewport = list_area.height;
    let (selected, len, stored) = list_window(model);
    let scroll = super::scroll_top(
        stored,
        selected,
        usize::from(list_area.height),
        len,
        model.scrolloff,
    );
    set_scroll(model, scroll);
    let items: Vec<ListItem> = if model.palette.is_some() {
        palette_items(model, usize::from(list_area.width))
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
            .map(|row| ListItem::new(table::line(row, &columns, width)))
            .collect()
    };
    let mut state = ListState::default().with_offset(scroll);
    if len > 0 {
        state.select(Some(selected));
    }
    frame.render_widget(&block, layout[0]);
    if !header.is_empty() {
        frame.render_widget(Paragraph::new(header), header_area);
    }
    if let Some(title_area) = title_area {
        frame.render_widget(
            Paragraph::new(table::header(&columns, usize::from(title_area.width))),
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
    if let Some(pane) = &model.pane {
        draw_pane(frame, layout[1], pane);
    }
    let footer_width = usize::from(layout[2].width);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(truncate(&keys(model), footer_width)),
            Line::styled(
                truncate(&model.status_line(), footer_width),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ]),
        layout[2],
    );
    // Overlays leave the key hints and status line in view.
    let above_footer = Rect {
        height: area.height.saturating_sub(2),
        ..area
    };
    if let (Some(pager), Some(pane)) = (model.output.as_mut(), model.pane.as_ref()) {
        draw_output(frame, above_footer, pane, pager);
    }
    if let Some(confirm) = model.confirm.as_mut() {
        draw_confirm(frame, above_footer, confirm);
    }
    if let Some(pager) = model.help.as_mut() {
        draw_help(frame, above_footer, pager);
    }
    if let Some(choice) = model.chooser.as_mut() {
        super::choice::draw(frame, above_footer, choice);
    }
}

fn kind(model: &Model) -> Kind {
    let searching = !model.filter.text().trim().is_empty();
    match &model.view {
        View::Detail { .. } => Kind::Checkouts,
        View::List if searching => Kind::Search,
        View::List if model.grouping.is_tree() => Kind::Tree,
        View::List => Kind::Projects,
    }
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
        return model.filter.titled(" bonsai  /", model.filtering);
    }
    let sort = format!("sorted by {}", model.sort.label());
    let parts = match &model.view {
        View::Detail { project } => {
            let found = model
                .snapshot
                .projects
                .iter()
                .find(|item| &item.id == project);
            let name = found.map_or("project", |item| item.name.as_str());
            let mut parts = vec![name.to_string()];
            if let Some(project) = found {
                let totals = super::rows::project_totals(model, project);
                parts.push(plural(totals.checkouts, "checkout"));
                if totals.sized > 0 {
                    parts.push(footprint::format_bytes(totals.bytes));
                }
                if totals.running > 0 {
                    parts.push(agents_running(totals.running));
                }
            }
            parts
        }
        View::List => {
            let mut parts = Vec::new();
            let (projects, checkouts, bytes, sized) = model.overall();
            parts.push(plural(projects, "project"));
            parts.push(plural(checkouts, "checkout"));
            if sized > 0 {
                parts.push(footprint::format_bytes(bytes));
            }
            let running = model.running_total();
            if running > 0 {
                parts.push(agents_running(running));
            }
            parts
        }
    };
    Line::from(format!(" bonsai · {} · {sort} ", parts.join(" · ")))
}

/// `table  projects  owners  hosts`, the current view highlighted. Shown on
/// the home list, where the grouping applies.
fn tabs(model: &Model) -> Option<Line<'static>> {
    if model.palette.is_some()
        || model.filtering
        || !model.filter.is_empty()
        || !matches!(model.view, View::List)
    {
        return None;
    }
    let mut spans = Vec::new();
    for grouping in super::Grouping::ALL {
        spans.push(Span::raw(" "));
        let style = if grouping == model.grouping {
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        spans.push(Span::styled(format!(" {} ", grouping.tab()), style));
    }
    spans.push(Span::raw(" "));
    Some(Line::from(spans))
}

/// `1 agent running`, `3 agents running`.
pub(super) fn agents_running(count: usize) -> String {
    format!("{} running", plural(count, "agent"))
}

pub(super) fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// An open project, or the home tree while search is closed.
pub(super) fn framed(model: &Model) -> bool {
    model.palette.is_none() && (matches!(model.view, View::Detail { .. }) || tree_frame(model))
}

fn tree_frame(model: &Model) -> bool {
    model.palette.is_none()
        && matches!(model.view, View::List)
        && model.grouping.is_tree()
        && model.filter.is_empty()
}

fn keys(model: &Model) -> String {
    let scroll = "j k  ctrl-d ctrl-u  ctrl-f ctrl-b  gg G  scroll";
    if model.chooser.is_some() {
        return format!("enter choose checkout   esc cancel   {scroll}");
    }
    if model.help.is_some() {
        return format!("{scroll}   ? or esc close");
    }
    if model.confirm.is_some() {
        return format!("y confirm   n or esc cancel   {scroll}");
    }
    if model.output.is_some() {
        return format!("{scroll}   O or esc close");
    }
    if model.palette.is_some() {
        return "enter run   tab complete   ↑↓ choose   ctrl-w word   esc cancel".to_string();
    }
    if model.filtering {
        return "enter done   esc clear   ↑↓ move   ctrl-w word   ctrl-c quit".to_string();
    }
    let mut keys = Vec::new();
    if model.pane.as_ref().is_some_and(|pane| !pane.running()) {
        keys.push("esc close output");
    }
    keys.extend([
        "a add",
        "dd remove",
        "D delete",
        "C clean",
        "P prune",
        "x trim",
        "f fetch",
        ": commands",
        "? help",
        "enter open/cd",
        "c cd",
        "e edit",
        "A agent",
        "R resume",
        "yy copy",
        "O full output",
        "/ search",
        "p running",
        "t next view",
        "T previous view",
        "s sort",
        "r refresh",
        "esc back",
        "q quit",
    ]);
    keys.join("   ")
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
    let totals = super::rows::project_totals(model, project);
    let mut summary = vec![Span::raw(plural(totals.checkouts, "checkout"))];
    if totals.changed > 0 {
        summary.push(Span::raw(" · "));
        summary.push(Span::styled(
            format!("{} with uncommitted changes", totals.changed),
            Style::new().fg(Color::Yellow),
        ));
    }
    if totals.missing > 0 {
        summary.push(Span::raw(" · "));
        summary.push(Span::styled(
            format!(
                "{} deleted by hand (press P to prune {})",
                totals.missing,
                if totals.missing == 1 { "it" } else { "them" }
            ),
            Style::new().fg(Color::Red),
        ));
    }
    if totals.sized > 0 {
        summary.push(Span::raw(" · "));
        let mut size = footprint::format_bytes(totals.bytes);
        if totals.measuring {
            size.push_str(" so far, measuring");
        }
        summary.push(Span::raw(size));
    }
    let trimmable = trimmable_bytes(model, project);
    if trimmable > 0 {
        summary.push(Span::raw(" · "));
        summary.push(Span::styled(
            trim_hint(trimmable),
            Style::new().fg(Color::Cyan),
        ));
    }
    if totals.running > 0 {
        summary.push(Span::raw(" · "));
        summary.push(Span::styled(
            agents_running(totals.running),
            table::running_style(),
        ));
    }
    let text = width.saturating_sub(1);
    summary.insert(0, Span::raw(" "));
    vec![
        Line::styled(format!(" {}", truncate(&remote, text)), remote_style),
        Line::styled(
            format!(" {}", truncate(&path_label(&project.path), text)),
            Style::new().fg(Color::DarkGray),
        ),
        Line::from(table::fit(summary, width)),
        Line::styled("─".repeat(width), table::rule_style()),
    ]
}

/// `3 GB of dependencies in worktrees (press x to trim)`.
fn trim_hint(bytes: u64) -> String {
    format!(
        "{} of dependencies in worktrees (press x to trim)",
        footprint::format_bytes(bytes)
    )
}

/// Dependency directories of the project's linked worktrees that live in the
/// checkout itself: roughly what `x` frees. Shared stores free nothing, and
/// the main checkout is never trimmed.
pub(super) fn trimmable_bytes(model: &Model, project: &super::Project) -> u64 {
    project
        .worktrees
        .iter()
        .filter(|worktree| !worktree.main && !worktree.missing)
        .filter_map(|worktree| model.footprints.get(&worktree.path))
        .flat_map(|item| &item.dependencies)
        .filter(|dependency| dependency.sharing == footprint::Sharing::Local)
        .filter_map(|dependency| dependency.bytes)
        .sum()
}

fn palette_items(model: &Model, width: usize) -> Vec<ListItem<'static>> {
    let matches = palette_matches(model);
    if matches.is_empty() {
        return vec![ListItem::new(" No matching command.")];
    }
    let usage = |spec: &super::CommandSpec| {
        if spec.args.is_empty() {
            spec.name.to_string()
        } else {
            format!("{} {}", spec.name, spec.args)
        }
    };
    let usage_width = super::COMMANDS
        .iter()
        .map(|spec| usage(spec).chars().count())
        .max()
        .unwrap_or(0);
    let hint_width = super::COMMANDS
        .iter()
        .map(|spec| spec.hint.chars().count())
        .max()
        .unwrap_or(0);
    matches
        .into_iter()
        .map(|spec| {
            let usage = usage(spec);
            let key = if spec.key.is_empty() {
                String::new()
            } else {
                format!("key {}", spec.key)
            };
            let spans = vec![
                Span::raw(" "),
                Span::styled(
                    super::pad(usage, usage_width),
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(super::pad(spec.hint.to_string(), hint_width + 2)),
                Span::styled(key, Style::new().add_modifier(Modifier::DIM)),
            ];
            ListItem::new(Line::from(table::fit(spans, width)))
        })
        .collect()
}

fn pane_state(pane: &jobs::Pane) -> (String, Style) {
    match &pane.state {
        jobs::State::Running => ("running".to_string(), Style::new().fg(Color::Yellow)),
        jobs::State::Succeeded => ("finished".to_string(), Style::new().fg(Color::Green)),
        jobs::State::Failed(reason) => (format!("failed: {reason}"), Style::new().fg(Color::Red)),
    }
}

fn draw_pane(frame: &mut Frame, area: Rect, pane: &jobs::Pane) {
    let (state, style) = pane_state(pane);
    let block = Block::bordered()
        .border_style(style)
        .title(format!(" {} · {state} ", pane.label));
    let inner = block.inner(area);
    let height = usize::from(inner.height);
    let width = usize::from(inner.width);
    let lines: Vec<Line> = pane
        .lines
        .iter()
        .skip(pane.lines.len().saturating_sub(height))
        .map(|line| Line::raw(truncate(line, width)))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Lines of a scrolled text view, with its position for the border.
fn scrolled(lines: Vec<Line<'static>>, pager: &mut Pager, height: usize) -> Vec<Line<'static>> {
    pager.fit(height, lines.len());
    lines.into_iter().skip(pager.top).take(height).collect()
}

fn position_title(pager: &Pager) -> String {
    pager
        .position()
        .map(|position| format!(" {position} · j k scroll "))
        .unwrap_or_default()
}

fn draw_confirm(frame: &mut Frame, area: Rect, confirm: &mut super::Confirm) {
    let buttons = Line::from(vec![
        Span::styled(
            format!(" y {} ", confirm.yes),
            Style::new()
                .fg(Color::Black)
                .bg(confirm.tone)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   n cancel"),
    ]);
    let width = confirm
        .lines
        .iter()
        .chain(std::iter::once(&buttons))
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(confirm.title.chars().count() + 4)
        + 4;
    let rect = centered(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(confirm.lines.len() + 4).unwrap_or(u16::MAX),
    );
    // The answers stay in view; the body scrolls when it does not fit.
    let body_height = usize::from(rect.height).saturating_sub(4).max(1);
    let mut lines = scrolled(confirm.lines.clone(), &mut confirm.pager, body_height);
    lines.push(Line::raw(""));
    lines.push(buttons);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(confirm.tone))
                .title(format!(" {} ", confirm.title))
                .title_bottom(position_title(&confirm.pager)),
        ),
        rect,
    );
}

/// Keys and commands, grouped as in the documentation.
pub(super) const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Move (also scrolls this help, dialogs, and the output view)",
        &[
            ("j k  ↓ ↑", "down / up; a count repeats: 5j"),
            ("gg  G", "first / last row; 3G goes to the third"),
            ("ctrl-d  ctrl-u", "half a page down / up"),
            ("ctrl-f  ctrl-b", "a page down / up"),
            ("ctrl-e  ctrl-y", "scroll, keeping the cursor"),
            ("ctrl-n  ctrl-p", "down / up, also while typing a search"),
            ("H  M  L", "top / middle / bottom of the screen"),
            (
                "zz  zt  zb",
                "put the cursor row at the center / top / bottom",
            ),
            ("{  }", "previous / next project"),
        ],
    ),
    (
        "Open",
        &[
            (
                "enter  o",
                "open a project; on a checkout, cd into it and quit",
            ),
            ("l  →  tab", "open a project"),
            ("c", "cd into the selected checkout and quit"),
            ("h  ←  esc", "back"),
            ("/", "search; enter keeps the results, esc clears them"),
            ("p", "only checkouts where a coding agent is running"),
            (
                "t  T",
                "next / previous view: table, projects, owners, hosts; each adds a level",
            ),
            ("s  r", "sort by name, recent activity, or size; refresh"),
        ],
    ),
    (
        "Act on the selection",
        &[
            ("a", "add a worktree: type the branch name"),
            ("dd", "remove selected worktrees and keep their branches"),
            ("D", "remove selected worktrees and delete their branches"),
            ("C  P", "clean merged worktrees / prune deleted ones"),
            (
                "x",
                "trim: delete ignored files such as node_modules and target",
            ),
            ("f", "fetch selected projects' remotes"),
            ("e", "open the checkout in $VISUAL or $EDITOR"),
            ("A  R", "start / resume a coding agent there"),
            ("yy", "copy the checkout's path"),
            ("O", "the latest command's output, full screen"),
            (":", "command line; tab completes, :add <branch>"),
            ("q  ?", "quit, this help"),
        ],
    ),
    (
        "Typing in / and :",
        &[
            ("ctrl-a  ctrl-e", "start / end of the line"),
            ("ctrl-w  ctrl-u", "delete a word / to the start"),
            ("↑ ↓", "choose a match or a command"),
            ("enter  esc", "leave the line"),
        ],
    ),
];

fn draw_help(frame: &mut Frame, area: Rect, pager: &mut Pager) {
    let mut lines = Vec::new();
    for (group, keys) in HELP {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            *group,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        for (key, meaning) in *keys {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}", super::pad((*key).to_string(), 16)),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(*meaning),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Press ? or esc to close.",
        Style::new().add_modifier(Modifier::DIM),
    ));
    let width = lines.iter().map(Line::width).max().unwrap_or(0) + 4;
    let rect = centered(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    let lines = scrolled(lines, pager, usize::from(rect.height).saturating_sub(2));
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(Color::Cyan))
                .title(" bonsai hq keys ")
                .title_bottom(position_title(pager)),
        ),
        rect,
    );
}

/// The latest command's output over the whole list, like a pager.
fn draw_output(frame: &mut Frame, area: Rect, pane: &jobs::Pane, pager: &mut Pager) {
    let (state, style) = pane_state(pane);
    let block = Block::bordered()
        .border_style(style)
        .title(format!(" {} · {state} ", pane.label));
    let inner = block.inner(area);
    let width = usize::from(inner.width);
    let lines: Vec<Line> = pane
        .lines
        .iter()
        .map(|line| Line::raw(truncate(line, width)))
        .collect();
    let lines = scrolled(lines, pager, usize::from(inner.height));
    let block = block.title_bottom(position_title(pager));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// How a checkout relates to its upstream, naming the upstream.
pub(super) fn sync_sentence(sync: super::rows::Sync, upstream: Option<&str>) -> Option<String> {
    use super::rows::Sync;
    let Some(upstream) = upstream else {
        return table::sync_words(sync);
    };
    Some(match sync {
        Sync::Unknown => return None,
        Sync::Gone => format!("upstream {upstream} is gone"),
        Sync::Tracking {
            ahead: 0,
            behind: 0,
        } => format!("up to date with {upstream}"),
        Sync::Tracking { ahead, behind: 0 } => format!("{ahead} ahead of {upstream}"),
        Sync::Tracking { ahead: 0, behind } => format!("{behind} behind {upstream}"),
        Sync::Tracking { ahead, behind } => format!("{ahead} ahead, {behind} behind {upstream}"),
        Sync::Detached | Sync::NoUpstream => return table::sync_words(sync),
    })
}

/// The selected row in words, for the status line.
pub(super) fn describe(model: &Model) -> Option<String> {
    let row = model.rows.get(model.cursor)?;
    match &row.info {
        Info::Checkout(checkout) => {
            let path = row.path.as_ref()?;
            let mut parts = vec![path_label(path)];
            if let Some(words) = sync_sentence(checkout.sync, checkout.upstream.as_deref()) {
                parts.push(words);
            }
            if let Some(bytes) = checkout.bytes {
                let mut size = footprint::format_bytes(bytes);
                let deps: Vec<String> = checkout
                    .dependencies
                    .iter()
                    .filter_map(table::dependency_label)
                    .collect();
                if !deps.is_empty() {
                    size = format!("{size}, of which {}", deps.join(", "));
                }
                parts.push(size);
            }
            if let Some(running) = model.working.get(path).filter(|agents| !agents.is_empty()) {
                let running: Vec<&crate::agents::Agent> = running.iter().collect();
                parts.push(format!("{} running", crate::agents::describe(&running)));
            }
            Some(parts.join("  ·  "))
        }
        Info::Group(totals) => {
            let mut parts = Vec::new();
            let project = model
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == row.project_id);
            if let Some(project) = project {
                parts.push(path_label(&project.path));
            } else {
                parts.push(
                    [row.host.as_str(), row.owner.as_str()]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
            let mut counts = plural(totals.checkouts, "checkout");
            if totals.changed > 0 {
                counts.push_str(&format!(", {} with uncommitted changes", totals.changed));
            }
            if totals.missing > 0 {
                counts.push_str(&format!(", {} deleted", totals.missing));
            }
            parts.push(counts);
            if totals.sized > 0 {
                parts.push(footprint::format_bytes(totals.bytes));
            }
            // What the project's keys can act on, as the header says for P.
            if let Some(project) = project {
                let trimmable = trimmable_bytes(model, project);
                if trimmable > 0 {
                    parts.push(trim_hint(trimmable));
                }
            }
            if totals.running > 0 {
                parts.push(format!(
                    "{} (press p to show where)",
                    agents_running(totals.running)
                ));
            }
            Some(parts.join("  ·  "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hq::inventory::{Project, Snapshot, WorktreeEntry};
    use crate::hq::{Grouping, rows};

    fn model() -> Model {
        Model::from_snapshot(Snapshot {
            root: "/trees".into(),
            warnings: Vec::new(),
            projects: vec![Project {
                id: "project-id".to_string(),
                name: "scope-project".to_string(),
                path: "/repos/project".into(),
                remote: Some("git@github.com:owner/project.git".to_string()),
                worktrees: vec![WorktreeEntry {
                    path: "/external/feature".into(),
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
                }],
            }],
        })
    }

    fn screen(model: &mut Model, width: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| draw(frame, model)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn command_pane_keeps_the_list_visible_in_every_state() {
        for state in [
            jobs::State::Running,
            jobs::State::Succeeded,
            jobs::State::Failed("exit 1".to_string()),
        ] {
            let mut model = model();
            let mut pane = jobs::Pane::new("prune");
            pane.state = state;
            pane.lines.push_back("command progress".to_string());
            model.pane = Some(pane);
            let screen = screen(&mut model, 100).join("\n");
            assert!(screen.contains("scope-project"));
            assert!(screen.contains("command progress"));
            assert!(screen.contains("prune ·"));
            assert!(model.viewport > 0);
        }
    }

    #[test]
    fn full_screen_output_is_an_explicit_overlay() {
        let mut model = model();
        let mut pane = jobs::Pane::new("prune");
        pane.lines.push_back("command progress".to_string());
        model.pane = Some(pane);
        assert!(screen(&mut model, 100).join("\n").contains("scope-project"));
        model.output = Some(Pager::default());
        let screen = screen(&mut model, 100).join("\n");
        assert!(!screen.contains("scope-project"));
        assert!(screen.contains("command progress"));
    }

    #[test]
    fn action_hints_are_identical_in_grouped_detail_and_search_views() {
        let mut model = model();
        let expected = screen(&mut model, 500)[22].clone();
        for grouping in Grouping::ALL {
            for (detail, search) in [
                (false, ""),
                (true, ""),
                (false, "feature"),
                (true, "feature"),
            ] {
                model.grouping = grouping;
                model.view = if detail {
                    View::Detail {
                        project: "project-id".to_string(),
                    }
                } else {
                    View::List
                };
                model.filter.set(search);
                model.rows = rows::visible_rows(&model, 0);
                assert_eq!(screen(&mut model, 500)[22], expected);
            }
        }
    }
}
