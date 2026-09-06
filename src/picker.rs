use std::borrow::Cow;
use std::io::{IsTerminal, stderr, stdin};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use inquire::{Autocomplete, Confirm, MultiSelect, Text};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use skim::{DisplayContext, Skim, SkimItem, options::SkimOptionsBuilder};

/// inquire renders on stderr and reads the tty, so pickers stay interactive
/// even while stdout is captured by the shell wrapper — but both ends must be
/// terminals.
fn ensure_tty(what: &str) -> Result<()> {
    if !stdin().is_terminal() || !stderr().is_terminal() {
        bail!("{what} required but not running in a terminal; pass it as an argument");
    }
    Ok(())
}

/// A picker option whose searchable text is independent from its styled row.
pub struct StyledOption {
    /// Text searched by the fuzzy scorer. It may include useful hidden fields
    /// (such as a session ID) that do not belong in the rendered row.
    pub plain: String,
    display: String,
    age: Option<(String, Style)>,
}

/// One recent-first picker row. Columns are aligned across all rows and the
/// final relative-age column uses the same freshness colors in every picker.
pub struct RecentRow {
    pub columns: Vec<String>,
    pub search: String,
    pub last_change: Option<SystemTime>,
}

impl SkimItem for StyledOption {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.plain)
    }

    fn display(&self, context: DisplayContext) -> Line<'_> {
        let mut line = Line::from(Span::styled(&self.display, context.base_style));
        if let Some((age, style)) = &self.age {
            line.push_span(Span::styled(age, context.base_style.patch(*style)));
        }
        line
    }
}

/// A fast asynchronous fuzzy picker that preserves the caller's ordering.
pub fn select_styled(
    prompt: &str,
    options: Vec<StyledOption>,
    initial_filter: Option<&str>,
) -> Result<usize> {
    ensure_tty(prompt)?;
    let picker_options = skim_options(prompt, initial_filter, options.len())?;
    let output = Skim::run_items(picker_options, options)
        .map_err(|error| anyhow!("fuzzy picker failed: {error}"))?;
    if output.is_abort {
        bail!("selection cancelled");
    }
    let picked = output
        .selected_items
        .first()
        .context("selection cancelled")?;
    usize::try_from(picked.rank.index).context("selected option index is invalid")
}

fn skim_options(
    prompt: &str,
    initial_filter: Option<&str>,
    option_count: usize,
) -> Result<skim::SkimOptions> {
    SkimOptionsBuilder::default()
        .height(option_count.saturating_add(2).clamp(5, 15).to_string())
        .min_height("5")
        .reverse(true)
        .no_sort(true)
        .no_info(true)
        .cycle(true)
        .prompt(format!("{prompt} "))
        .header("↑↓/ctrl-p,n move  enter select  ctrl-w delete word  esc cancel")
        .query(initial_filter.unwrap_or_default())
        .build()
        .context("invalid fuzzy picker configuration")
}

pub fn recent_options(rows: &[RecentRow]) -> Vec<StyledOption> {
    let column_count = rows.iter().map(|row| row.columns.len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..column_count)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.columns.get(column))
                .map(|value| value.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let color = stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let now = SystemTime::now();

    rows.iter()
        .map(|row| {
            let display = row
                .columns
                .iter()
                .enumerate()
                .map(|(column, value)| {
                    let padding = widths[column].saturating_sub(value.chars().count());
                    format!("{value}{}", " ".repeat(padding))
                })
                .collect::<Vec<_>>()
                .join("  ");
            let elapsed = row
                .last_change
                .and_then(|changed| now.duration_since(changed).ok());
            let age = elapsed.map(|elapsed| {
                let style = if color {
                    age_style(elapsed)
                } else {
                    Style::default()
                };
                (format!("  {}", format_age(elapsed)), style)
            });
            StyledOption {
                plain: format!("{display} {}", row.search),
                display,
                age,
            }
        })
        .collect()
}

/// Compact relative age: 42s, 5m, 3h, 2d, 4w, 6mo, 1y.
fn format_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        86_400..604_800 => format!("{}d", secs / 86_400),
        604_800..2_592_000 => format!("{}w", secs / 604_800),
        2_592_000..31_536_000 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_536_000),
    }
}

/// Freshness gradient: bright green (< 1h), green (today), yellow (this
/// week), dim (older).
fn age_style(elapsed: Duration) -> Style {
    match elapsed.as_secs() {
        0..3_600 => Style::default().fg(Color::LightGreen),
        3_600..86_400 => Style::default().fg(Color::Green),
        86_400..604_800 => Style::default().fg(Color::Yellow),
        _ => Style::default().add_modifier(Modifier::DIM),
    }
}

pub fn multi_select_all_checked(prompt: &str, options: Vec<String>) -> Result<Vec<String>> {
    ensure_tty(prompt)?;
    let all: Vec<usize> = (0..options.len()).collect();
    MultiSelect::new(prompt, options)
        .with_default(&all)
        .prompt()
        .context("selection cancelled")
}

pub fn multi_select_none_checked(prompt: &str, options: Vec<String>) -> Result<Vec<String>> {
    ensure_tty(prompt)?;
    MultiSelect::new(prompt, options)
        .prompt()
        .context("selection cancelled")
}

pub fn confirm(prompt: &str) -> Result<bool> {
    ensure_tty(prompt)?;
    Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .context("confirmation cancelled")
}

/// Free-text prompt with fuzzy suggestions; typing a novel value is allowed
/// (that is how `bonsai add` creates new branches).
pub fn text_with_suggestions(prompt: &str, help: &str, suggestions: Vec<String>) -> Result<String> {
    ensure_tty(prompt)?;
    let value = Text::new(prompt)
        .with_autocomplete(Suggestions(suggestions))
        .with_help_message(help)
        .prompt()
        .context("input cancelled")?;
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("empty input");
    }
    Ok(value)
}

#[derive(Clone)]
struct Suggestions(Vec<String>);

impl Autocomplete for Suggestions {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, inquire::CustomUserError> {
        let needle = input.to_lowercase();
        Ok(self
            .0
            .iter()
            .filter(|s| fuzzy_match(&s.to_lowercase(), &needle))
            .cloned()
            .collect())
    }

    fn get_completion(
        &mut self,
        input: &str,
        highlighted: Option<String>,
    ) -> Result<inquire::autocompletion::Replacement, inquire::CustomUserError> {
        let _ = input;
        Ok(highlighted)
    }
}

/// Subsequence match: every char of `needle` appears in order in `haystack`.
fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use super::{age_style, format_age, fuzzy_match, skim_options};
    use ratatui::style::{Color, Modifier};
    use skim::{binds::parse_key, tui::actions::Action};
    use std::time::Duration;

    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy_match("feature/login", "ftlog"));
        assert!(fuzzy_match("feature/login", ""));
        assert!(!fuzzy_match("main", "z"));
        assert!(!fuzzy_match("ab", "ba"));
    }

    #[test]
    fn recent_picker_preserves_order_and_has_shell_editing_keys() {
        let options = skim_options("Session:", Some("parser"), 20).unwrap();
        assert!(options.no_sort);
        assert_eq!(options.height, "15");
        assert_eq!(options.prompt, "Session: ");
        assert_eq!(options.query.as_deref(), Some("parser"));
        assert_eq!(
            options.keymap.get(&parse_key("ctrl-w").unwrap()),
            Some(&vec![Action::UnixWordRubout])
        );
        assert_eq!(
            options.keymap.get(&parse_key("ctrl-a").unwrap()),
            Some(&vec![Action::BeginningOfLine])
        );
        assert_eq!(
            options.keymap.get(&parse_key("ctrl-e").unwrap()),
            Some(&vec![Action::EndOfLine])
        );
        assert_eq!(
            options.keymap.get(&parse_key("ctrl-u").unwrap()),
            Some(&vec![Action::UnixLineDiscard])
        );
    }

    #[test]
    fn recent_picker_uses_a_compact_inline_viewport() {
        assert_eq!(skim_options("Worktree:", None, 1).unwrap().height, "5");
        assert_eq!(skim_options("Worktree:", None, 8).unwrap().height, "10");
        assert_eq!(skim_options("Worktree:", None, 100).unwrap().height, "15");
    }

    #[test]
    fn ages_format_compactly() {
        let cases = [
            (30, "30s"),
            (90, "1m"),
            (3 * 3_600, "3h"),
            (2 * 86_400, "2d"),
            (13 * 86_400, "1w"),
            (40 * 86_400, "1mo"),
            (2 * 31_536_000, "2y"),
        ];
        for (secs, expected) in cases {
            assert_eq!(format_age(Duration::from_secs(secs)), expected);
        }
    }

    #[test]
    fn age_colors_follow_freshness() {
        assert_eq!(
            age_style(Duration::from_secs(60)).fg,
            Some(Color::LightGreen)
        );
        assert_eq!(
            age_style(Duration::from_secs(2 * 3_600)).fg,
            Some(Color::Green)
        );
        assert_eq!(
            age_style(Duration::from_secs(3 * 86_400)).fg,
            Some(Color::Yellow)
        );
        assert!(
            age_style(Duration::from_secs(30 * 86_400))
                .add_modifier
                .contains(Modifier::DIM)
        );
    }
}
