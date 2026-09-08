use std::io::{IsTerminal, stderr, stdin};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;

use crate::git::Git;
use crate::picker;
use crate::repo::Repo;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, ValueEnum)]
pub enum Provider {
    Claude,
    Codex,
    #[value(name = "opencode")]
    OpenCode,
}

impl Provider {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }

    pub(super) fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    fn start_args(self, prompt: Option<&str>) -> Vec<String> {
        match (self, prompt) {
            (_, None) => Vec::new(),
            // A prompt such as "resume" or "--continue" must stay prompt text.
            (Self::Claude | Self::Codex, Some(prompt)) => vec!["--".into(), prompt.into()],
            (Self::OpenCode, Some(prompt)) => vec![format!("--prompt={prompt}")],
        }
    }
}

pub fn run(provider: Option<Provider>, prompt: Option<String>) -> Result<()> {
    Repo::require()?;
    ensure!(
        Git::new().out(&["rev-parse", "--is-inside-work-tree"])? == "true",
        "start requires a Git worktree, not a bare repository"
    );
    let (provider, program) = select_provider(provider)?;
    let mut command = Command::new(crate::paths::canonicalize_or_self(&program));
    command.args(provider.start_args(prompt.as_deref()));
    launch_interactive(command, provider)
}

pub(super) fn launch_interactive(mut command: Command, provider: Provider) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The harness must own its signals: a waiting Bonsai parent would
        // die on Ctrl-C even when the interactive harness handles it.
        Err(command.exec()).with_context(|| format!("failed to launch {}", provider.executable()))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .with_context(|| format!("failed to launch {}", provider.executable()))?;
        ensure!(
            status.success(),
            "{} exited with status {status}",
            provider.executable()
        );
        Ok(())
    }
}

fn select_provider(requested: Option<Provider>) -> Result<(Provider, PathBuf)> {
    if let Some(provider) = requested {
        let program = crate::pm::find_program(provider.executable())
            .with_context(|| format!("'{}' was not found on PATH", provider.executable()))?;
        return Ok((provider, program));
    }

    let mut available = [Provider::Claude, Provider::Codex, Provider::OpenCode]
        .into_iter()
        .filter_map(|provider| {
            crate::pm::find_program(provider.executable()).map(|program| (provider, program))
        })
        .collect::<Vec<_>>();
    let selected = match available.len() {
        0 => bail!("no coding harness found on PATH; install Claude Code, Codex, or OpenCode"),
        1 => 0,
        _ => {
            ensure!(
                stdin().is_terminal() && stderr().is_terminal(),
                "multiple coding harnesses are installed; choose one with bonsai start <claude|codex|opencode>"
            );
            let rows = available
                .iter()
                .map(|(provider, _)| picker::RecentRow {
                    columns: vec![provider.label().into(), provider.executable().into()],
                    search: format!("{} {}", provider.label(), provider.executable()),
                    last_change: None,
                })
                .collect::<Vec<_>>();
            picker::select_styled(
                "Start a coding session:",
                picker::recent_options(&rows),
                None,
            )?
        }
    };
    Ok(available.swap_remove(selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_sessions_keep_prompt_text_separate_from_commands_and_flags() {
        for prompt in [
            "explain this project",
            "resume",
            "--dangerously-skip-permissions",
            "first line\nsecond line",
        ] {
            for provider in [Provider::Claude, Provider::Codex] {
                assert_eq!(provider.start_args(Some(prompt)), ["--", prompt]);
            }
            assert_eq!(
                Provider::OpenCode.start_args(Some(prompt)),
                [format!("--prompt={prompt}")]
            );
        }
    }

    #[test]
    fn no_prompt_opens_a_fresh_interactive_session() {
        for provider in [Provider::Claude, Provider::Codex, Provider::OpenCode] {
            assert!(provider.start_args(None).is_empty());
        }
    }
}
