//! Coding agents running on this machine, and the worktree each works in.
//! Worktree commands consult this so nothing is deleted under a live agent,
//! and `ps` and the dashboard show agents next to their worktrees.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Claude,
    Codex,
    Grok,
    OpenCode,
    CursorAgent,
    Gemini,
    Copilot,
    Amp,
    Aider,
    Goose,
    Crush,
    Qwen,
    Pi,
}

/// Executable names, then npm package markers for agents launched through an
/// interpreter (`node .../@openai/codex/bin/codex.js`).
const AGENTS: &[(Kind, &str, &[&str])] = &[
    (Kind::Claude, "claude", &["@anthropic-ai/claude-code"]),
    (Kind::Codex, "codex", &["@openai/codex"]),
    (Kind::Grok, "grok", &["grok-dev"]),
    (Kind::OpenCode, "opencode", &["opencode-ai"]),
    (Kind::CursorAgent, "cursor-agent", &[]),
    (Kind::Gemini, "gemini", &["@google/gemini-cli"]),
    (Kind::Copilot, "copilot", &["@github/copilot"]),
    (Kind::Amp, "amp", &["@sourcegraph/amp"]),
    (Kind::Aider, "aider", &[]),
    (Kind::Goose, "goose", &[]),
    (Kind::Crush, "crush", &["@charmland/crush"]),
    (Kind::Qwen, "qwen", &["@qwen-code/qwen-code"]),
    (Kind::Pi, "pi", &["@mariozechner/pi-coding-agent"]),
];

const INTERPRETERS: &[&str] = &["node", "bun", "deno", "python", "python3"];

impl Kind {
    /// Every recognized agent, in display order; this is not a popularity ranking.
    pub fn all() -> impl Iterator<Item = Kind> {
        AGENTS.iter().map(|(kind, _, _)| *kind)
    }

    /// Short name, as typed on the command line.
    pub fn name(self) -> &'static str {
        AGENTS
            .iter()
            .find(|(kind, _, _)| *kind == self)
            .map(|(_, name, _)| *name)
            .unwrap_or("agent")
    }

    fn from_executable(stem: &str) -> Option<Kind> {
        AGENTS
            .iter()
            .find(|(_, name, _)| *name == stem)
            .map(|(kind, _, _)| *kind)
    }

    fn from_script(script: &str) -> Option<Kind> {
        let normalized = script.replace('\\', "/");
        AGENTS
            .iter()
            .find(|(_, _, packages)| {
                packages
                    .iter()
                    .any(|package| normalized.contains(&format!("/{package}/")))
            })
            .map(|(kind, _, _)| *kind)
            .or_else(|| Kind::from_executable(stem(&normalized)))
    }
}

/// `/x/claude.exe` → `claude`, `python3.12` → `python3`. Case is kept:
/// agent CLIs are lowercase, unlike desktop apps (`Claude`, `Codex`).
fn stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let name = [".exe", ".js", ".mjs", ".cjs"]
        .iter()
        .find(|extension| lower.ends_with(*extension))
        .map_or(name, |extension| &name[..name.len() - extension.len()]);
    match name.split_once('.') {
        Some((base, version)) if version.chars().all(|c| c.is_ascii_digit() || c == '.') => base,
        _ => name,
    }
}

/// Which agent a process is, from its name and argv. Interpreted agents
/// are recognized by their entry script; helpers spawned by an agent (MCP
/// servers, shells, `codex-code-mode-host`) are not agents themselves.
pub fn classify(name: &str, argv: &[String]) -> Option<Kind> {
    let process = stem(name);
    if let Some(kind) = Kind::from_executable(process) {
        return Some(kind);
    }
    // macOS framework builds of Python run as `Python`.
    if !INTERPRETERS.contains(&process.to_ascii_lowercase().as_str()) {
        return None;
    }
    let script = argv.iter().skip(1).find(|arg| !arg.starts_with('-'))?;
    Kind::from_script(script)
}

#[derive(Clone, Debug, Serialize)]
pub struct Agent {
    pub kind: Kind,
    pub pid: u32,
    pub cwd: PathBuf,
    #[serde(serialize_with = "unix_seconds")]
    pub started: SystemTime,
    /// This bonsai process descends from it: the agent invoked bonsai, so
    /// its own worktree is not "busy" for the operation it asked for.
    pub caller: bool,
}

fn unix_seconds<S: serde::Serializer>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serializer.serialize_u64(seconds)
}

/// Every agent process visible to this user, newest first. Launcher chains
/// (Codex's native binary under its node launcher) count once, as the
/// outermost process.
pub fn running() -> Vec<Agent> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    let kinds: HashMap<Pid, Kind> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let argv: Vec<String> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            Some((*pid, classify(&process.name().to_string_lossy(), &argv)?))
        })
        .collect();
    let mut callers = std::collections::HashSet::new();
    let mut current = system
        .process(Pid::from_u32(std::process::id()))
        .and_then(|p| p.parent());
    while let Some(pid) = current {
        if !callers.insert(pid) {
            break;
        }
        current = system.process(pid).and_then(|p| p.parent());
    }
    let mut agents: Vec<Agent> = kinds
        .iter()
        // A launcher's direct child of the same kind is the same agent; an
        // agent started from another agent's shell is a separate one.
        .filter(|(pid, kind)| {
            let parent = system.process(**pid).and_then(|p| p.parent());
            parent.and_then(|parent| kinds.get(&parent)) != Some(kind)
        })
        .filter_map(|(pid, kind)| {
            let process = system.process(*pid)?;
            Some(Agent {
                kind: *kind,
                pid: pid.as_u32(),
                cwd: crate::paths::canonicalize_or_self(process.cwd()?),
                started: UNIX_EPOCH + Duration::from_secs(process.start_time()),
                caller: callers.contains(pid),
            })
        })
        .collect();
    agents.sort_by(|a, b| b.started.cmp(&a.started).then(a.pid.cmp(&b.pid)));
    agents
}

/// For each agent, the index of the worktree it works in: the deepest
/// worktree containing its directory (external worktrees may nest).
pub fn assign(agents: &[Agent], worktrees: &[PathBuf]) -> Vec<Option<usize>> {
    let worktrees: Vec<PathBuf> = worktrees
        .iter()
        .map(|path| crate::paths::canonicalize_or_self(path))
        .collect();
    agents
        .iter()
        .map(|agent| {
            worktrees
                .iter()
                .enumerate()
                .filter(|(_, worktree)| agent.cwd.starts_with(worktree))
                .max_by_key(|(_, worktree)| worktree.components().count())
                .map(|(index, _)| index)
        })
        .collect()
}

/// Other agents working inside `worktree`: what a command about to delete
/// files there must not pull the rug from under.
pub fn busy<'a>(agents: &'a [Agent], worktree: &Path) -> Vec<&'a Agent> {
    let worktree = crate::paths::canonicalize_or_self(worktree);
    agents
        .iter()
        .filter(|agent| !agent.caller && agent.cwd.starts_with(&worktree))
        .collect()
}

/// `claude (pid 42), codex (pid 7)`.
pub fn describe(agents: &[&Agent]) -> String {
    agents
        .iter()
        .map(|agent| format!("{} (pid {})", agent.kind.name(), agent.pid))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn recognizes_native_and_interpreted_agents() {
        let claude = "/x/node_modules/@anthropic-ai/claude-code/bin/claude.exe";
        assert_eq!(classify("claude.exe", &argv(&[claude])), Some(Kind::Claude));
        assert_eq!(
            classify(
                "node",
                &argv(&[
                    "node",
                    "/x/node_modules/@openai/codex/bin/codex.js",
                    "--yolo"
                ])
            ),
            Some(Kind::Codex)
        );
        assert_eq!(
            classify(
                "node",
                &argv(&[
                    "node",
                    "--no-warnings",
                    "/x/@google/gemini-cli/dist/index.js"
                ])
            ),
            Some(Kind::Gemini)
        );
        assert_eq!(
            classify("python3.12", &argv(&["python3.12", "-m", "aider"])),
            Some(Kind::Aider)
        );
        assert_eq!(
            classify("opencode", &argv(&["opencode"])),
            Some(Kind::OpenCode)
        );
        assert_eq!(
            classify("cursor-agent", &argv(&["cursor-agent"])),
            Some(Kind::CursorAgent)
        );
    }

    #[test]
    fn ignores_desktop_apps_helpers_shells_and_mcp_servers() {
        let desktop = "/Applications/Claude.app/Contents/MacOS/Claude";
        assert_eq!(classify("Claude", &argv(&[desktop])), None);
        assert_eq!(
            classify("codex-code-mode-host", &argv(&["codex-code-mode-host"])),
            None
        );
        assert_eq!(
            classify(
                "zsh",
                &argv(&["/bin/zsh", "-c", "source ~/.claude/shell-snapshots/x.sh"])
            ),
            None
        );
        assert_eq!(
            classify(
                "node",
                &argv(&[
                    "node",
                    "/x/@modelcontextprotocol/server-github/dist/index.js"
                ])
            ),
            None
        );
        assert_eq!(classify("node", &argv(&["node"])), None);
    }

    #[test]
    fn recognizes_every_supported_agent_and_its_interpreted_packages() {
        for &(kind, executable, packages) in AGENTS {
            assert_eq!(classify(executable, &argv(&[executable])), Some(kind));
            for package in packages {
                let script = format!("/x/node_modules/{package}/dist/index.js");
                assert_eq!(classify("bun", &argv(&["bun", &script])), Some(kind));
            }
        }
        assert_eq!(classify("Grok", &argv(&["Grok"])), None);
        assert_eq!(classify("grok-helper", &argv(&["grok-helper"])), None);
        assert_eq!(
            classify("node", &argv(&["node", "/x/grok-dev-helper/index.js"])),
            None
        );
    }

    #[test]
    fn agents_belong_to_the_deepest_containing_worktree() {
        let agent = |cwd: &str| Agent {
            kind: Kind::Claude,
            pid: 1,
            cwd: PathBuf::from(cwd),
            started: UNIX_EPOCH,
            caller: false,
        };
        let agents = [
            agent("/repo/src"),
            agent("/repo/.worktrees/feat/pkg"),
            agent("/elsewhere"),
            agent("/repo-other"),
        ];
        let worktrees = [
            PathBuf::from("/repo"),
            PathBuf::from("/repo/.worktrees/feat"),
        ];
        assert_eq!(assign(&agents, &worktrees), [Some(0), Some(1), None, None]);
        assert_eq!(busy(&agents, Path::new("/repo/.worktrees/feat")).len(), 1);
        let caller = Agent {
            caller: true,
            ..agent("/repo/.worktrees/feat")
        };
        assert!(busy(&[caller], Path::new("/repo/.worktrees/feat")).is_empty());
    }
}
