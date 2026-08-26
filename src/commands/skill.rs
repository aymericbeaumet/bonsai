use std::path::PathBuf;

use anyhow::{Context, Result};

/// The canonical skill, embedded so `cargo install` distributes it.
pub const SKILL_MD: &str = include_str!("../../skills/bonsai/SKILL.md");

pub fn show() {
    print!("{SKILL_MD}");
}

/// Install the skill into the skill directories of every AI harness detected
/// on this machine (`--all` skips detection). Directory conventions:
/// Claude Code ~/.claude/skills, Codex ~/.codex/skills, OpenCode
/// $XDG_CONFIG_HOME/opencode/skills, Cursor ~/.agents/skills (the shared
/// agent-compatible location it reads alongside the Claude/Codex ones), Pi
/// ~/.pi/agent/skills.
pub fn install(all: bool) -> Result<()> {
    let mut installed = 0;
    for (harness, detect_dir, skills_dir) in targets()? {
        if !all && !detect_dir.exists() {
            continue;
        }
        let dir = skills_dir.join("bonsai");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        std::fs::write(dir.join("SKILL.md"), SKILL_MD)
            .with_context(|| format!("cannot write {}", dir.display()))?;
        eprintln!("bonsai: installed skill for {harness}: {}", dir.display());
        installed += 1;
    }
    if installed == 0 {
        eprintln!(
            "bonsai: no AI harness detected; run 'bonsai skill install --all' to install everywhere"
        );
    }
    Ok(())
}

fn targets() -> Result<Vec<(&'static str, PathBuf, PathBuf)>> {
    let home = std::env::home_dir().context("cannot determine home directory")?;
    let xdg_config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home.join(".config"),
    };
    Ok(vec![
        (
            "Claude Code",
            home.join(".claude"),
            home.join(".claude/skills"),
        ),
        ("Codex", home.join(".codex"), home.join(".codex/skills")),
        (
            "OpenCode",
            xdg_config.join("opencode"),
            xdg_config.join("opencode/skills"),
        ),
        ("Cursor", home.join(".cursor"), home.join(".agents/skills")),
        ("Pi", home.join(".pi/agent"), home.join(".pi/agent/skills")),
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn skill_has_frontmatter_and_contract() {
        assert!(super::SKILL_MD.starts_with("---\nname: bonsai\n"));
        for needle in ["description:", "--dry-run --json", "--help"] {
            assert!(super::SKILL_MD.contains(needle), "missing: {needle}");
        }
    }

    #[test]
    fn codex_plugin_skill_matches_canonical_skill() {
        let plugin_skill = include_str!("../../plugins/bonsai/skills/bonsai/SKILL.md");
        assert_eq!(super::SKILL_MD, plugin_skill);
    }

    #[test]
    fn package_versions_match_crate_version() {
        let manifests = [
            include_str!("../../.claude-plugin/plugin.json"),
            include_str!("../../plugins/bonsai/.codex-plugin/plugin.json"),
            include_str!("../../editors/vscode/package.json"),
        ];
        for manifest in manifests {
            let json: serde_json::Value = serde_json::from_str(manifest).unwrap();
            assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
        }
    }
}
