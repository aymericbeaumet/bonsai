use std::io::{IsTerminal, stderr, stdin};

use anyhow::{Context, Result, bail};
use inquire::{Autocomplete, Confirm, MultiSelect, Select, Text};

/// inquire renders on stderr and reads the tty, so pickers stay interactive
/// even while stdout is captured by the shell wrapper — but both ends must be
/// terminals.
fn ensure_tty(what: &str) -> Result<()> {
    if !stdin().is_terminal() || !stderr().is_terminal() {
        bail!("{what} required but not running in a terminal; pass it as an argument");
    }
    Ok(())
}

/// A picker option rendered with ANSI styling while filtering runs on the
/// plain text, so escape codes never pollute fuzzy matching.
pub struct StyledOption {
    pub plain: String,
    pub styled: String,
}

impl std::fmt::Display for StyledOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.styled)
    }
}

/// Like `select`, but returns the picked index and filters on each option's
/// plain text via inquire's default (fuzzy) scorer.
pub fn select_styled(
    prompt: &str,
    options: Vec<StyledOption>,
    initial_filter: Option<&str>,
) -> Result<usize> {
    ensure_tty(prompt)?;
    let scorer = |input: &str, option: &StyledOption, _value: &str, idx: usize| {
        Select::<String>::DEFAULT_SCORER(input, &option.plain, &option.plain, idx)
    };
    let mut select = Select::new(prompt, options).with_scorer(&scorer);
    if let Some(filter) = initial_filter {
        select = select.with_starting_filter_input(filter);
    }
    select
        .raw_prompt()
        .map(|picked| picked.index)
        .context("selection cancelled")
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
    use super::fuzzy_match;

    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy_match("feature/login", "ftlog"));
        assert!(fuzzy_match("feature/login", ""));
        assert!(!fuzzy_match("main", "z"));
        assert!(!fuzzy_match("ab", "ba"));
    }
}
