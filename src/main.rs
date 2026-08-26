mod cli;
mod commands;
mod config;
mod git;
mod picker;
mod repo;
mod shell;
mod worktree;

use std::path::PathBuf;

use anyhow::Result;
use clap::{CommandFactory, Parser};

use crate::cli::{Cli, Commands};
use crate::config::Config;

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(cd_target) => {
            if let Some(path) = cd_target {
                emit_cd(&path);
            }
        }
        Err(err) => {
            eprintln!("bonsai: {err:#}");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<Option<PathBuf>> {
    // Shell plumbing needs no repo or config.
    match &cli.command {
        Commands::Init { shell } => {
            print!("{}", shell::init_script(*shell));
            return Ok(None);
        }
        Commands::Completions { shell } => {
            clap_complete::generate(
                *shell,
                &mut Cli::command(),
                "bonsai",
                &mut std::io::stdout(),
            );
            return Ok(None);
        }
        _ => {}
    }

    // .bonsai.toml is read from the current worktree's checkout when it has
    // one (your branch's version wins), falling back to the main worktree.
    let main_root = repo::Repo::discover()?.map(|r| r.main_root);
    let cwd_toplevel = git::Git::new()
        .out(&["rev-parse", "--show-toplevel"])
        .ok()
        .map(std::path::PathBuf::from);
    let toml_dir = match (&cwd_toplevel, &main_root) {
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

    match cli.command {
        Commands::Add {
            branch,
            base,
            fetch,
            path,
        } => commands::add::run(&config, branch, base, fetch, path),
        Commands::List { all, status } => commands::list::run(&config, all, status).map(|_| None),
        Commands::Remove {
            branches,
            delete_branch,
            force,
        } => commands::remove::run(&config, branches, delete_branch, force),
        Commands::Prune { all, yes } => commands::prune::run(&config, all, yes).map(|_| None),
        Commands::Clean {
            dry_run,
            yes,
            no_fetch,
        } => commands::clean::run(&config, dry_run, yes, no_fetch),
        Commands::Cd { query } => commands::cd::run(&config, query),
        Commands::Init { .. } | Commands::Completions { .. } => unreachable!(),
    }
}

/// Wrapped (shell function capturing stdout): emit the cd sentinel as the
/// final line. Unwrapped: print the bare path so `cd "$(bonsai cd foo)"`
/// composes.
fn emit_cd(path: &std::path::Path) {
    if std::env::var_os(shell::WRAPPED_ENV).is_some() {
        println!("{}{}", shell::CD_SENTINEL, path.display());
    } else {
        println!("{}", path.display());
    }
}
