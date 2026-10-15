//! Git worktree manager used by the `bonsai` command.
//!
//! The `bonsai-cli` package builds that command and embeds the Agent Skill.
//! This crate is the library behind it, imported as `bonsai`.

pub mod build_cache;
pub mod cli;
pub mod commands;
pub mod config;
pub mod edit;
pub mod git;
pub mod hq;
pub mod output;
pub mod parallel;
pub mod paths;
pub mod picker;
pub mod pm;
pub mod process;
pub mod repo;
pub mod scope;
pub mod shell;
pub mod workspace;
pub mod worktree;
