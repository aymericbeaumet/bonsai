//! Columns of the headquarters list: which ones each view shows, how wide
//! they are, and the words in each cell. Every value is spelled out under a
//! heading; nothing relies on symbols.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::footprint::{self, Sharing};
use super::rows::{Checkout, Depth, Info, Note, Row, Status, Sync, Totals};
use crate::picker;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Col {
    Host,
    Owner,
    Project,
    Branch,
    /// The tree's indented label.
    Name,
    Checkouts,
    Age,
    Changes,
    Upstream,
    Size,
    Deps,
    Stack,
    /// Coding agents with sessions there.
    Agents,
    /// Coding agents running there now.
    Running,
    Notes,
}

struct Spec {
    title: &'static str,
    right: bool,
    /// Columns with the lowest value are dropped first when space runs out.
    keep: u8,
    cap: usize,
}

fn spec(col: Col) -> Spec {
    let (title, right, keep, cap) = match col {
        Col::Host => ("host", false, 50, 24),
        Col::Owner => ("owner", false, 65, 20),
        Col::Project => ("project", false, 100, 32),
        Col::Branch => ("branch", false, 100, 40),
        Col::Name => ("name", false, 100, 60),
        Col::Checkouts => ("checkouts", true, 80, 9),
        Col::Age => ("age", true, 85, 4),
        Col::Changes => ("changes", false, 90, 34),
        Col::Upstream => ("upstream", false, 75, 20),
        Col::Size => ("size", true, 95, 9),
        Col::Deps => ("deps", false, 45, 30),
        Col::Stack => ("stack", false, 40, 18),
        Col::Agents => ("agents", false, 55, 30),
        // Rarely shown, and then it matters: a removal or trim skips it.
        Col::Running => ("running", false, 88, 24),
        Col::Notes => ("notes", false, 70, 24),
    };
    Spec {
        title,
        right,
        keep,
        cap,
    }
}

/// Which kind of rows a view lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    /// The home list: one row per project.
    Projects,
    /// An open project: its checkouts.
    Checkouts,
    /// Search results: checkouts of any project.
    Search,
    Tree,
}

fn candidates(kind: Kind) -> &'static [Col] {
    match kind {
        Kind::Projects => &[
            Col::Host,
            Col::Owner,
            Col::Project,
            Col::Checkouts,
            Col::Age,
            Col::Changes,
            Col::Size,
            Col::Stack,
            Col::Agents,
            Col::Running,
            Col::Notes,
        ],
        Kind::Checkouts => &[
            Col::Branch,
            Col::Age,
            Col::Changes,
            Col::Upstream,
            Col::Size,
            Col::Deps,
            Col::Agents,
            Col::Running,
            Col::Notes,
        ],
        Kind::Search => &[
            Col::Host,
            Col::Owner,
            Col::Project,
            Col::Branch,
            Col::Age,
            Col::Changes,
            Col::Upstream,
            Col::Size,
            Col::Agents,
            Col::Running,
            Col::Notes,
        ],
        Kind::Tree => &[
            Col::Name,
            Col::Checkouts,
            Col::Age,
            Col::Changes,
            Col::Upstream,
            Col::Size,
            Col::Deps,
            Col::Agents,
            Col::Running,
            Col::Notes,
        ],
    }
}

const GAP: usize = 2;
const MARGIN: usize = 1;
const MIN_FLEX: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub(super) cols: Vec<(Col, usize)>,
}

impl Layout {
    #[cfg(test)]
    pub(super) fn has(&self, col: Col) -> bool {
        self.cols.iter().any(|(item, _)| *item == col)
    }
}

/// Columns that have something to show, as wide as their content, dropping
/// the least useful ones until the row fits.
pub(super) fn layout(kind: Kind, rows: &[Row], width: usize) -> Layout {
    let mut cols: Vec<(Col, usize)> = candidates(kind)
        .iter()
        .filter_map(|col| {
            let spec = spec(*col);
            let content = rows
                .iter()
                .map(|row| spans_width(&cell(row, *col)))
                .max()
                .unwrap_or(0);
            let flex = is_flex(*col);
            if content == 0 && !flex {
                return None;
            }
            Some((*col, content.max(spec.title.len()).min(spec.cap)))
        })
        .collect();
    let total = |cols: &[(Col, usize)]| {
        MARGIN
            + cols.iter().map(|(_, width)| width).sum::<usize>()
            + GAP * cols.len().saturating_sub(1)
    };
    while total(&cols) > width {
        let Some(drop) = cols
            .iter()
            .enumerate()
            .filter(|(_, (col, _))| !is_flex(*col))
            .min_by_key(|(_, (col, _))| spec(*col).keep)
            .map(|(index, _)| index)
        else {
            break;
        };
        // Keep the identifying columns while a lower-value one is left.
        if spec(cols[drop].0).keep >= 100 {
            break;
        }
        cols.remove(drop);
    }
    let overflow = total(&cols).saturating_sub(width);
    if overflow > 0
        && let Some((_, flex)) = cols.iter_mut().rev().find(|(col, _)| is_flex(*col))
    {
        *flex = flex.saturating_sub(overflow).max(MIN_FLEX);
    }
    Layout { cols }
}

fn is_flex(col: Col) -> bool {
    matches!(col, Col::Project | Col::Branch | Col::Name)
}

pub(super) fn header(layout: &Layout, width: usize) -> Line<'static> {
    let quiet = Style::new().add_modifier(Modifier::BOLD | Modifier::DIM);
    let spans = layout
        .cols
        .iter()
        .map(|(col, width)| {
            let style = match col {
                Col::Host => host_style().add_modifier(Modifier::BOLD),
                Col::Owner => owner_style().add_modifier(Modifier::BOLD),
                Col::Project => project_style(),
                _ => quiet,
            };
            let spec = spec(*col);
            (vec![Span::styled(spec.title, style)], *width, spec.right)
        })
        .collect();
    join(spans, width)
}

pub(super) fn line(row: &Row, layout: &Layout, width: usize) -> Line<'static> {
    let spans = layout
        .cols
        .iter()
        .map(|(col, width)| (cell(row, *col), *width, spec(*col).right))
        .collect();
    join(spans, width)
}

fn join(cells: Vec<(Vec<Span<'static>>, usize, bool)>, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(MARGIN))];
    let last = cells.len().saturating_sub(1);
    for (index, (cell, column, right)) in cells.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
        }
        let cell = fit(cell, column);
        let room = column.saturating_sub(spans_width(&cell));
        if right {
            spans.push(Span::raw(" ".repeat(room)));
            spans.extend(cell);
        } else {
            spans.extend(cell);
            if index != last {
                spans.push(Span::raw(" ".repeat(room)));
            }
        }
    }
    Line::from(fit(spans, width))
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

/// Truncate spans to `width` characters, ending in `…` when cut.
pub(super) fn fit(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= width {
        return spans;
    }
    let mut used = 0;
    let mut kept = Vec::new();
    for span in spans {
        let count = span.content.chars().count();
        if used + count < width {
            used += count;
            kept.push(span);
            continue;
        }
        let take = width.saturating_sub(used + 1);
        let mut text: String = span.content.chars().take(take).collect();
        if width > used {
            text.push('…');
        }
        kept.push(Span::styled(text, span.style));
        break;
    }
    kept
}

pub(super) fn cell(row: &Row, col: Col) -> Vec<Span<'static>> {
    let one = |text: String, style: Style| {
        if text.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(text, style)]
        }
    };
    match col {
        Col::Host => one(row.host.clone(), host_style()),
        Col::Owner => one(row.owner.clone(), owner_style()),
        Col::Project => one(row.project.clone(), project_style()),
        Col::Branch => one(row.title.clone(), branch_style(row)),
        Col::Name => name_cell(row),
        Col::Age => match row.age {
            Some(elapsed) => one(picker::format_age(elapsed), picker::age_style(elapsed)),
            None => Vec::new(),
        },
        _ => match &row.info {
            Info::Group(totals) => one_of(group_cell(totals, col)),
            Info::Checkout(checkout) => one_of(checkout_cell(checkout, col)),
        },
    }
}

fn one_of(cell: Option<(String, Style)>) -> Vec<Span<'static>> {
    match cell {
        Some((text, style)) if !text.is_empty() => vec![Span::styled(text, style)],
        _ => Vec::new(),
    }
}

fn group_cell(totals: &Totals, col: Col) -> Option<(String, Style)> {
    match col {
        Col::Checkouts => {
            (totals.checkouts > 0).then(|| (totals.checkouts.to_string(), Style::new()))
        }
        Col::Changes => {
            (totals.changed > 0).then(|| (format!("{} changed", totals.changed), changed_style()))
        }
        Col::Size => size_cell((totals.sized > 0).then_some(totals.bytes), totals.measuring),
        Col::Stack => Some((totals.stack.join(", "), Style::new().fg(Color::Cyan))),
        Col::Agents => Some((totals.agents.join(", "), agent_style())),
        Col::Running => (totals.running > 0).then(|| {
            (
                super::view::plural(totals.running, "agent"),
                running_style(),
            )
        }),
        Col::Notes => {
            let mut notes = Vec::new();
            if totals.unavailable {
                notes.push("unavailable".to_string());
            }
            if totals.missing > 0 {
                notes.push(format!("{} missing", totals.missing));
            }
            Some((notes.join(", "), Style::new().fg(Color::Red)))
        }
        _ => None,
    }
}

fn checkout_cell(checkout: &Checkout, col: Col) -> Option<(String, Style)> {
    match col {
        Col::Changes => Some(status_cell(checkout.status)),
        Col::Upstream => sync_cell(checkout.sync),
        Col::Size => size_cell(checkout.bytes, checkout.measuring),
        Col::Deps => {
            let labels: Vec<String> = checkout
                .dependencies
                .iter()
                .filter_map(dependency_label)
                .collect();
            Some((labels.join(", "), Style::new().add_modifier(Modifier::DIM)))
        }
        Col::Stack => Some((checkout.stack.join(", "), Style::new().fg(Color::Cyan))),
        Col::Agents => Some((checkout.agents.join(", "), agent_style())),
        Col::Running => Some((super::running::spell(&checkout.running), running_style())),
        Col::Notes => {
            let words: Vec<&str> = checkout.notes.iter().map(|note| note_word(*note)).collect();
            let style = match checkout.notes.first() {
                Some(Note::External) => Style::new().fg(Color::Magenta),
                Some(Note::Locked) => Style::new().fg(Color::Yellow),
                Some(Note::Prunable) => Style::new().fg(Color::Red),
                _ => Style::new().fg(Color::Cyan).add_modifier(Modifier::DIM),
            };
            Some((words.join(", "), style))
        }
        _ => None,
    }
}

pub(super) fn status_words(status: Status) -> String {
    match status {
        Status::Checking => "checking".to_string(),
        Status::Clean => "clean".to_string(),
        Status::Unavailable => "status unavailable".to_string(),
        Status::Missing => "directory deleted".to_string(),
        Status::Changed {
            added,
            modified,
            deleted,
            untracked,
        } => [
            (modified, "modified"),
            (added, "added"),
            (deleted, "deleted"),
            (untracked, "untracked"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect::<Vec<_>>()
        .join(", "),
    }
}

fn status_cell(status: Status) -> (String, Style) {
    let style = match status {
        Status::Checking | Status::Clean => Style::new().add_modifier(Modifier::DIM),
        Status::Changed { .. } => changed_style(),
        Status::Unavailable => Style::new().fg(Color::Yellow),
        Status::Missing => Style::new().fg(Color::Red),
    };
    (status_words(status), style)
}

pub(super) fn sync_words(sync: Sync) -> Option<String> {
    Some(match sync {
        Sync::Unknown => return None,
        Sync::Detached => "detached".to_string(),
        Sync::NoUpstream => "no upstream".to_string(),
        Sync::Gone => "upstream gone".to_string(),
        Sync::Tracking {
            ahead: 0,
            behind: 0,
        } => "up to date".to_string(),
        Sync::Tracking { ahead, behind: 0 } => format!("{ahead} ahead"),
        Sync::Tracking { ahead: 0, behind } => format!("{behind} behind"),
        Sync::Tracking { ahead, behind } => format!("{ahead} ahead, {behind} behind"),
    })
}

fn sync_cell(sync: Sync) -> Option<(String, Style)> {
    let style = match sync {
        Sync::Tracking {
            ahead: 0,
            behind: 0,
        }
        | Sync::Detached => Style::new().add_modifier(Modifier::DIM),
        Sync::Tracking { behind: 0, .. } => Style::new().fg(Color::Green),
        Sync::NoUpstream => Style::new().fg(Color::Yellow).add_modifier(Modifier::DIM),
        Sync::Gone => Style::new().fg(Color::Magenta),
        _ => Style::new().fg(Color::Yellow),
    };
    sync_words(sync).map(|words| (words, style))
}

fn size_cell(bytes: Option<u64>, measuring: bool) -> Option<(String, Style)> {
    let quiet = Style::new().add_modifier(Modifier::DIM);
    match bytes {
        Some(bytes) if measuring => Some((footprint::format_bytes(bytes), quiet)),
        Some(bytes) => Some((footprint::format_bytes(bytes), Style::new())),
        None if measuring => Some(("measuring".to_string(), quiet)),
        None => None,
    }
}

pub(super) fn dependency_label(dependency: &footprint::Dependency) -> Option<String> {
    let shared = dependency.sharing == Sharing::Shared;
    match (dependency.bytes, dependency.sharing) {
        (None, Sharing::None) => None,
        (None, _) => Some(format!("{} shared", dependency.name)),
        (Some(bytes), _) if shared => Some(format!(
            "{} {} shared",
            dependency.name,
            footprint::format_bytes(bytes)
        )),
        (Some(bytes), _) => Some(format!(
            "{} {}",
            dependency.name,
            footprint::format_bytes(bytes)
        )),
    }
}

pub(super) fn note_word(note: Note) -> &'static str {
    match note {
        Note::Main => "main checkout",
        Note::External => "external",
        Note::Locked => "locked",
        Note::Prunable => "prunable",
    }
}

/// The label of a tree row, in parts with their own colours. A joined label
/// spells out its ancestors, such as `github.com/owner/project`.
fn label_parts(row: &Row) -> Vec<(String, Style)> {
    let host = (row.host.clone(), host_style());
    let owner = (row.owner.clone(), owner_style());
    let project = (row.project.clone(), project_style());
    let parts = match (row.depth, row.joined) {
        (Depth::Host, _) => vec![host],
        (Depth::Owner, false) => vec![owner],
        (Depth::Owner, true) => vec![host, owner],
        (Depth::Repo, false) => vec![project],
        (Depth::Repo, true) => vec![host, owner, project],
        (Depth::Branch | Depth::Flat, _) => vec![(row.title.clone(), branch_style(row))],
    };
    parts
        .into_iter()
        .filter(|(text, _)| !text.is_empty())
        .collect()
}

fn name_cell(row: &Row) -> Vec<Span<'static>> {
    let parts = label_parts(row);
    let lead = parts.first().map(|(_, style)| *style).unwrap_or_default();
    let mut spans = Vec::new();
    if row.prefix.is_empty() && row.guide.is_empty() {
        // A banner: a top-level group of the tree.
        spans.push(Span::styled("┌─ ", lead));
    } else {
        // The table margin already provides the rail's leading space.
        let prefix = row.prefix.strip_prefix(' ').unwrap_or(&row.prefix);
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix.to_string(), rule_style()));
        }
        let mark = if row.guide.starts_with('└') {
            "└─ "
        } else {
            "├─ "
        };
        spans.push(Span::styled(mark, lead));
    }
    for (index, (text, style)) in parts.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("/", Style::new().add_modifier(Modifier::DIM)));
        }
        spans.push(Span::styled(text, style));
    }
    spans
}

pub(super) fn rule_style() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::DIM)
}

pub(super) fn host_style() -> Style {
    Style::new().fg(Color::LightBlue)
}

pub(super) fn owner_style() -> Style {
    Style::new().fg(Color::LightMagenta)
}

pub(super) fn project_style() -> Style {
    Style::new()
        .fg(Color::LightGreen)
        .add_modifier(Modifier::BOLD)
}

fn agent_style() -> Style {
    Style::new().fg(Color::Magenta)
}

pub(super) fn running_style() -> Style {
    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
}

fn changed_style() -> Style {
    Style::new().fg(Color::Yellow)
}

pub(super) fn branch_style(row: &Row) -> Style {
    if row.main {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}
