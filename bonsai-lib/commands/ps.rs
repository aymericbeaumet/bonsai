use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Result;
use serde::Serialize;
use tabwriter::TabWriter;

use crate::agents::{self, Agent};
use crate::picker::format_age;
use crate::scope::Scope;

/// A checkout agents can work in, with its display label.
struct Place {
    path: PathBuf,
    label: String,
    branch: Option<String>,
    repo: Option<String>,
}

#[derive(Serialize)]
struct Entry<'a> {
    #[serde(flatten)]
    agent: &'a Agent,
    /// The checkout the agent works in, when it is a known one.
    worktree: Option<&'a PathBuf>,
    branch: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<&'a str>,
}

/// One project: agents working in its checkouts. Every project (`-a`, or
/// outside a repository): every agent on the machine, labelled by checkout
/// when it works in a known one and by directory otherwise.
pub fn run(scope: &Scope, json: bool) -> Result<()> {
    let running = agents::running();
    let mut places = Vec::new();
    for project in scope.projects() {
        let id = scope.is_all().then(|| project.id());
        for entry in project.repo.project_worktrees(&project.config)? {
            if entry.worktree.is_bare {
                continue;
            }
            let label = entry.label();
            places.push(Place {
                label: match &id {
                    Some(id) => format!("{id}: {label}"),
                    None => label,
                },
                path: entry.worktree.path,
                branch: entry.worktree.branch,
                repo: id.clone(),
            });
        }
    }
    let paths: Vec<PathBuf> = places.iter().map(|place| place.path.clone()).collect();
    let entries: Vec<(&Agent, Option<&Place>)> = running
        .iter()
        .zip(agents::assign(&running, &paths))
        .map(|(agent, owner)| (agent, owner.map(|index| &places[index])))
        .filter(|(_, place)| scope.is_all() || place.is_some())
        .collect();

    if json {
        let entries: Vec<Entry> = entries
            .iter()
            .map(|(agent, place)| Entry {
                agent,
                worktree: place.map(|place| &place.path),
                branch: place.and_then(|place| place.branch.as_deref()),
                repo: place.and_then(|place| place.repo.as_deref()),
            })
            .collect();
        return crate::output::line(format_args!("{}", serde_json::to_string_pretty(&entries)?));
    }
    if entries.is_empty() {
        eprintln!("bonsai: no coding agents running");
        return Ok(());
    }
    let now = SystemTime::now();
    let home = std::env::home_dir();
    let mut table = TabWriter::new(Vec::new()).padding(2);
    for (agent, place) in entries {
        let location = match place {
            Some(place) => place.label.clone(),
            None => match home
                .as_ref()
                .and_then(|home| agent.cwd.strip_prefix(home).ok())
            {
                Some(rest) => format!("~/{}", rest.display()),
                None => agent.cwd.display().to_string(),
            },
        };
        let uptime = now
            .duration_since(agent.started)
            .map(format_age)
            .unwrap_or_default();
        writeln!(
            table,
            "{}\t{location}\t{}\t{uptime}",
            agent.kind.name(),
            agent.pid
        )?;
    }
    let table = String::from_utf8(table.into_inner()?)?;
    crate::output::write(format_args!("{table}"))
}
