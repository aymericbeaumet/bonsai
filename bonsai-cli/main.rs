use std::path::PathBuf;

use anyhow::{Context, Result};
use bonsai::cli::{Cli, Commands, SkillAction};
use bonsai::commands;
use bonsai::config::Config;
use bonsai::output;
use bonsai::process;
use bonsai::{git, hq, repo, scope, shell, workspace};
use clap::{CommandFactory, Parser};

/// The canonical skill, embedded so `cargo install` distributes it.
const SKILL_MD: &str = include_str!("../skills/bonsai/SKILL.md");

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
    for directory in &cli.cwd {
        std::env::set_current_dir(directory)
            .with_context(|| format!("cannot change to '{}'", directory.display()))?;
    }
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
            commands::agents::run(SKILL_MD)?;
            return Ok(None);
        }
        Commands::Skill { action } => {
            match action {
                None => commands::skill::show(SKILL_MD)?,
                Some(SkillAction::Install { all }) => commands::skill::install(SKILL_MD, *all)?,
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
    if let Some(remote) = &cli.remote {
        config.remote = Some(remote.clone());
    }
    let in_repo = repo.is_some();
    let current = repo.map(|repo| scope::Project {
        repo,
        config: config.clone(),
    });
    let scope = |all: bool| scope::Scope::resolve(&config, current, all, cli.remote.as_deref());

    match cli.command {
        Commands::Hq { json, all } => {
            hq::run(&config, json, all || !in_repo)?;
            Ok(None)
        }
        Commands::Start {
            provider,
            prompt,
            all,
        } => {
            commands::start::run(&scope(all), provider, prompt)?;
            Ok(None)
        }
        Commands::Add {
            branch,
            base,
            fetch,
            path,
            all,
        } => {
            let project = scope(all).into_project()?;
            commands::add::run(&project.config, &project.repo, branch, base, fetch, path)
        }
        Commands::List { all, status, json } => {
            commands::list::run(&config, &scope(all), status, json).map(|_| None)
        }
        Commands::Remove {
            branches,
            delete_branch,
            force,
            all,
        } => commands::remove::run(&config, &scope(all), branches, delete_branch, force),
        Commands::Prune { all, yes } => {
            commands::prune::run(&config, &scope(all), yes).map(|_| None)
        }
        Commands::Clean {
            dry_run,
            yes,
            no_fetch,
            json,
            all,
        } => {
            commands::clean::run(&config, &scope(all), dry_run, yes, no_fetch, json).map(|target| {
                if json && std::env::var_os(shell::WRAPPED_ENV).is_none() {
                    None
                } else {
                    target
                }
            })
        }
        Commands::Cd { query, all } => commands::cd::run(&scope(all), query),
        Commands::Resume {
            query,
            provider,
            session,
            all,
        } => {
            commands::resume::run(&scope(all), query, provider, session)?;
            Ok(None)
        }
        Commands::Workspace { all } => {
            // The global file needs no project discovery.
            let file = if all || !in_repo {
                workspace::sync_global(&config)?
            } else {
                let project = scope(false).into_project()?;
                workspace::sync(&project.repo, &project.config)?
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
