mod cli;
mod commands;
mod config;
mod git;
mod output;
mod parallel;
mod paths;
mod picker;
mod pm;
mod process;
mod repo;
mod shell;
mod workspace;
mod worktree;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use crate::cli::{Cli, Commands};
use crate::config::Config;

fn main() {
    shell::warn_if_stale_integration();
    let cli = Cli::parse();
    let json_report = matches!(&cli.command, Commands::Clean { json: true, .. });
    let result = run(cli).and_then(|target| match target {
        Some(path) => emit_cd(&path),
        None => Ok(()),
    });
    if let Err(err) = result {
        if output::is_broken_pipe(&err) {
            return;
        }
        if let Some(failure) = err.downcast_ref::<commands::remove::RemovalFailure>()
            && let Some(path) = &failure.recovery
            && (!json_report || std::env::var_os(shell::WRAPPED_ENV).is_some())
        {
            let _ = emit_cd(path);
        }
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "bonsai: {err:#}");
        let status = err
            .downcast_ref::<process::Interrupted>()
            .map_or(1, |signal| 128 + signal.0);
        std::process::exit(status);
    }
}

fn run(cli: Cli) -> Result<Option<PathBuf>> {
    // Shell plumbing needs no repo or config.
    match &cli.command {
        Commands::Init { shell } => {
            output::write(format_args!("{}", shell::init_script(*shell)))?;
            return Ok(None);
        }
        Commands::Completions { shell } => {
            let mut generated = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "bonsai", &mut generated);
            output::write(format_args!("{}", String::from_utf8(generated)?))?;
            return Ok(None);
        }
        Commands::Agents => {
            commands::agents::run()?;
            return Ok(None);
        }
        Commands::Skill { action } => {
            match action {
                None => commands::skill::show()?,
                Some(cli::SkillAction::Install { all }) => commands::skill::install(*all)?,
            }
            return Ok(None);
        }
        _ => {}
    }

    // .bonsai.toml is read from the current worktree's checkout when it has
    // one (your branch's version wins), falling back to the main worktree.
    let repo = repo::Repo::discover()?;
    let main_root = repo.as_ref().map(|repo| &repo.main_root);
    let cwd_toplevel = repo.as_ref().and_then(|repo| repo.current_root.clone());
    let toml_dir = match (&cwd_toplevel, main_root) {
        (Some(top), _) if top.join(".bonsai.toml").is_file() => Some(top.clone()),
        (_, Some(root)) => Some(root.clone()),
        (top, None) => top.clone(),
    };
    // Git::new() reads the effective git config from the cwd: the repo's
    // `bonsai.*` keys when inside one, the user's global ones otherwise.
    let mut config = Config::load(toml_dir.as_deref(), &git::Git::new())?;
    // CLI flags sit at the top of the precedence chain.
    if let Some(root) = cli.root {
        config.root = root;
    }
    if let Some(remote) = cli.remote {
        config.remote = Some(remote);
    }
    let require_repo = || repo.as_ref().context("not inside a git repository");

    match cli.command {
        Commands::Add {
            branch,
            base,
            fetch,
            path,
        } => commands::add::run(&config, require_repo()?, branch, base, fetch, path),
        Commands::List { all, status, json } => {
            commands::list::run(&config, repo.as_ref(), all, status, json).map(|_| None)
        }
        Commands::Remove {
            branches,
            delete_branch,
            force,
        } => commands::remove::run(&config, require_repo()?, branches, delete_branch, force),
        Commands::Prune { all, yes } => {
            commands::prune::run(&config, repo.as_ref(), all, yes).map(|_| None)
        }
        Commands::Clean {
            dry_run,
            yes,
            no_fetch,
            json,
        } => commands::clean::run(&config, require_repo()?, dry_run, yes, no_fetch, json).map(
            |target| {
                if json && std::env::var_os(shell::WRAPPED_ENV).is_none() {
                    None
                } else {
                    target
                }
            },
        ),
        Commands::Cd { query } => commands::cd::run(&config, repo.as_ref(), query),
        Commands::Resume { query } => {
            commands::resume::run(&config, repo.as_ref(), query)?;
            Ok(None)
        }
        Commands::Workspace { all } => {
            let file = if all {
                workspace::sync_global(&config)?
            } else {
                workspace::sync(require_repo()?, &config)?
            };
            if !file.exists() {
                anyhow::bail!("no bonsai worktrees found; run 'bonsai add' first");
            }
            output::line(format_args!("{}", file.display()))?;
            Ok(None)
        }
        Commands::Init { .. }
        | Commands::Completions { .. }
        | Commands::Agents
        | Commands::Skill { .. } => unreachable!(),
    }
}

/// Wrapped (shell function capturing stdout): emit the cd sentinel as the
/// final line. Unwrapped: print the bare path so `cd "$(bonsai cd foo)"`
/// composes.
fn emit_cd(path: &std::path::Path) -> Result<()> {
    if std::env::var_os(shell::WRAPPED_ENV).is_some() {
        output::line(format_args!("{}{}", shell::CD_SENTINEL, path.display()))
    } else {
        output::line(format_args!("{}", path.display()))
    }
}
