//! Drawing: the framed list, the project header, the output pane, the key
//! hints and status line, and the help, confirmation, and command overlays.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

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
    if let Some(confirm) = &model.confirm {
        draw_confirm(frame, area, confirm);
    }
    if model.help {
        draw_help(frame, area);
    }
}

fn kind(model: &Model) -> Kind {
    let searching = !model.filter.text().trim().is_empty();
    match &model.view {
        View::Detail { .. } => Kind::Checkouts,
        View::List if searching => Kind::Search,
        View::List if model.tree => Kind::Tree,
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
            }
            parts
        }
        View::List => {
            let mut parts = Vec::new();
            if model.tree {
                parts.push("tree".to_string());
            }
            let (projects, checkouts, bytes, sized) = model.overall();
            parts.push(plural(projects, "project"));
            parts.push(plural(checkouts, "checkout"));
            if sized > 0 {
                parts.push(footprint::format_bytes(bytes));
            }
            parts
        }
    };
    Line::from(format!(" bonsai · {} · {sort} ", parts.join(" · ")))
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
        && model.tree
        && model.filter.is_empty()
}

fn keys(model: &Model) -> String {
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
    match &model.view {
        View::Detail { .. } => keys.extend([
            "enter cd",
            "a add",
            "dd remove",
            "D delete",
            "e edit",
            "A agent",
            "C clean",
            "esc back",
        ]),
        View::List if !model.filter.is_empty() => {
            keys.extend(["enter open", "c cd", "dd remove", "/ edit", "esc clear"]);
        }
        View::List if model.tree => {
            keys.extend([
                "enter open",
                "c cd",
                "a add",
                "dd remove",
                "t table",
                "s sort",
            ]);
        }
        View::List => keys.extend([
            "enter open",
            "/ search",
            "a add",
            "C clean",
            "P prune",
            "t tree",
            "s sort",
        ]),
    }
    keys.extend(["? help", "q quit"]);
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

fn draw_pane(frame: &mut Frame, area: Rect, pane: &jobs::Pane) {
    let (state, style) = match &pane.state {
        jobs::State::Running => ("running".to_string(), Style::new().fg(Color::Yellow)),
        jobs::State::Succeeded => ("finished".to_string(), Style::new().fg(Color::Green)),
        jobs::State::Failed(reason) => (format!("failed: {reason}"), Style::new().fg(Color::Red)),
    };
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

fn draw_confirm(frame: &mut Frame, area: Rect, confirm: &super::Confirm) {
    let mut lines = confirm.lines.clone();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" y {} ", confirm.yes),
            Style::new()
                .fg(Color::Black)
                .bg(confirm.tone)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   n cancel"),
    ]));
    let width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(confirm.title.chars().count() + 4)
        + 4;
    let rect = centered(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(confirm.tone))
                .title(format!(" {} ", confirm.title)),
        ),
        rect,
    );
}

/// Keys and commands, grouped as in the documentation.
pub(super) const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Move",
        &[
            ("j k  ↓ ↑", "down / up; a count repeats: 5j"),
            ("gg  G", "first / last row; 3G goes to the third"),
            ("ctrl-d  ctrl-u", "half a page down / up"),
            ("ctrl-f  ctrl-b", "a page down / up"),
            ("ctrl-e  ctrl-y", "scroll the list, keeping the cursor"),
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
            ("/", "search; user: and org: match owners"),
            ("t  s  r", "tree view, sort (name, recent, size), refresh"),
        ],
    ),
    (
        "Act on the selection",
        &[
            ("a", "add a worktree: type the branch name"),
            ("dd", "remove the worktree and keep its branch"),
            ("D", "remove the worktree and delete its branch"),
            ("C  P", "clean merged worktrees / prune deleted ones"),
            ("f", "fetch the project's remote"),
            ("e", "open the checkout in $VISUAL or $EDITOR"),
            ("A  R", "start / resume a coding agent there"),
            ("yy", "copy the checkout's path"),
            (":", "command line; tab completes, :add <branch>"),
            ("q  ?", "quit, this help"),
        ],
    ),
];

fn draw_help(frame: &mut Frame, area: Rect) {
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
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(Color::Cyan))
                .title(" bonsai hq keys "),
        ),
        rect,
    );
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
            Some(parts.join("  ·  "))
        }
        Info::Group(totals) => {
            let mut parts = Vec::new();
            if let Some(project) = model
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == row.project_id)
            {
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
            Some(parts.join("  ·  "))
        }
    }
}
