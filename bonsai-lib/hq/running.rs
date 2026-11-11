//! Coding agents running in the listed checkouts.
//!
//! The process table is scanned off the input thread every two seconds, and
//! each agent is assigned to the deepest checkout containing its directory.
//! A scan is sent only when the assignment changed, so an idle list is not
//! redrawn.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::agents::{self, Agent};

/// Time between two scans of the process table.
const INTERVAL: Duration = Duration::from_secs(2);

/// Agents by the checkout they work in, keyed by the inventory's path.
pub(super) type Working = BTreeMap<PathBuf, Vec<Agent>>;

/// The background scan. Dropping it stops the thread.
pub(super) struct Watch {
    receiver: Receiver<Working>,
    /// Never sent on: the thread waits on it between scans and stops once
    /// it is dropped.
    _stop: Sender<()>,
}

impl Watch {
    /// Scan now, then every [`INTERVAL`], for agents in `checkouts`.
    pub(super) fn start(checkouts: Vec<PathBuf>) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let mut last: Option<Vec<(PathBuf, Vec<u32>)>> = None;
            loop {
                match agents::running() {
                    Ok(agents) => {
                        let working = by_checkout(&agents, &checkouts);
                        let signature = signature(&working);
                        if last.as_ref() != Some(&signature) {
                            if sender.send(working).is_err() {
                                return;
                            }
                            last = Some(signature);
                        }
                    }
                    Err(error) => {
                        if let Some(path) = super::log_file() {
                            super::append_log_to(&path, &format!("{error:#}"));
                        }
                    }
                }
                match stopped.recv_timeout(INTERVAL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Self {
            receiver,
            _stop: stop,
        }
    }

    /// The latest scan that arrived since the last call.
    pub(super) fn latest(&self) -> Option<Working> {
        self.receiver.try_iter().last()
    }
}

/// Which agents work in which checkout. Agents outside every checkout are
/// left out.
pub(super) fn by_checkout(running: &[Agent], checkouts: &[PathBuf]) -> Working {
    let mut working = Working::new();
    for (agent, owner) in running.iter().zip(agents::assign(running, checkouts)) {
        if let Some(index) = owner {
            working
                .entry(checkouts[index].clone())
                .or_default()
                .push(agent.clone());
        }
    }
    working
}

/// What a redraw depends on: each checkout's agents, by process.
fn signature(working: &Working) -> Vec<(PathBuf, Vec<u32>)> {
    working
        .iter()
        .map(|(path, agents)| (path.clone(), agents.iter().map(|agent| agent.pid).collect()))
        .collect()
}

/// The kind of each agent, sorted: what the `running` column spells out.
pub(super) fn kinds(agents: &[Agent]) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = agents.iter().map(|agent| agent.kind.name()).collect();
    kinds.sort_unstable();
    kinds
}

/// `claude, 2 codex`: one name per kind, counted when several run.
pub(super) fn spell(kinds: &[&str]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for kind in kinds {
        *counts.entry(kind).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(name, count)| {
            if count == 1 {
                name.to_string()
            } else {
                format!("{count} {name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
pub(super) fn agent(kind: agents::Kind, pid: u32, cwd: &str) -> Agent {
    Agent {
        kind,
        pid,
        cwd: PathBuf::from(cwd),
        started: std::time::UNIX_EPOCH,
        caller: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Kind;

    #[test]
    fn agents_are_grouped_by_the_deepest_checkout() {
        let running = [
            agent(Kind::Claude, 1, "/trees/bonsai/main/src"),
            agent(Kind::Codex, 2, "/trees/bonsai/main/.worktrees/feat"),
            agent(Kind::Codex, 3, "/trees/bonsai/main/.worktrees/feat/pkg"),
            agent(Kind::Gemini, 4, "/elsewhere"),
        ];
        let checkouts = [
            PathBuf::from("/trees/bonsai/main"),
            PathBuf::from("/trees/bonsai/main/.worktrees/feat"),
        ];
        let working = by_checkout(&running, &checkouts);
        assert_eq!(working.len(), 2);
        assert_eq!(spell(&kinds(&working[&checkouts[0]])), "claude");
        assert_eq!(spell(&kinds(&working[&checkouts[1]])), "2 codex");
        assert_eq!(
            signature(&working),
            [
                (checkouts[0].clone(), vec![1]),
                (checkouts[1].clone(), vec![2, 3]),
            ]
        );
    }

    #[test]
    fn kinds_are_spelled_out_once_each() {
        let running = [
            agent(Kind::Codex, 1, "/x"),
            agent(Kind::Claude, 2, "/x"),
            agent(Kind::Claude, 3, "/x"),
        ];
        assert_eq!(spell(&kinds(&running)), "2 claude, codex");
        assert_eq!(spell(&[]), "");
    }
}
