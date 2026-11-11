use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    temp: tempfile::TempDir,
    anchor: PathBuf,
    external: PathBuf,
    anchor_missing: PathBuf,
    external_missing: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = bonsai::paths::canonicalize_ok(temp.path()).unwrap();
        let fixture = Self {
            temp,
            anchor: base.join("anchor"),
            external: base.join("external-project"),
            anchor_missing: base.join("anchor-missing"),
            external_missing: base.join("external-missing"),
        };
        for (repo, missing) in [
            (&fixture.anchor, &fixture.anchor_missing),
            (&fixture.external, &fixture.external_missing),
        ] {
            std::fs::create_dir(repo).unwrap();
            fixture.git(repo, &["init", "-b", "main"]);
            fixture.git(repo, &["commit", "--allow-empty", "-m", "seed"]);
            fixture.git(
                repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "feature",
                    missing.to_str().unwrap(),
                ],
            );
            std::fs::remove_dir_all(missing).unwrap();
        }
        fixture
    }

    fn command(&self, program: &Path, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("no-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .env("BONSAI_ROOT", self.temp.path().join("empty-root"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("_BONSAI_PROJECTS")
            .env_remove("_BONSAI_WRAPPED");
        command
    }

    fn git(&self, repo: &Path, args: &[&str]) -> String {
        let output = self
            .command(Path::new("git"), repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn has_worktree(&self, repo: &Path, worktree: &Path) -> bool {
        self.git(repo, &["worktree", "list", "--porcelain", "-z"])
            .split('\0')
            .filter_map(|field| field.strip_prefix("worktree "))
            .any(|path| Path::new(path) == worktree)
    }

    fn bonsai(&self, all: bool, roots: &str, execute: bool) -> Output {
        let mut command = self.command(Path::new(env!("CARGO_BIN_EXE_bonsai")), &self.anchor);
        command.env("_BONSAI_PROJECTS", roots).arg("prune");
        if all {
            command.arg("-a");
        }
        command.arg(if execute { "--yes" } else { "--dry-run" });
        command
            .arg("--json")
            .arg(&self.external_missing)
            .output()
            .unwrap()
    }
}

#[test]
fn hq_project_roots_find_external_projects_with_only_deleted_linked_checkouts() {
    let fixture = Fixture::new();
    let roots = serde_json::to_string(&[&fixture.external, &fixture.external]).unwrap();
    let preview = fixture.bonsai(true, &roots, false);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    let stale: Vec<PathBuf> = serde_json::from_value(report["stale"].clone()).unwrap();
    assert_eq!(stale, std::slice::from_ref(&fixture.external_missing));
    assert!(report["orphans"].as_array().unwrap().is_empty());
    assert!(fixture.has_worktree(&fixture.external, &fixture.external_missing));
    let executed = fixture.bonsai(true, &roots, true);
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert!(!fixture.has_worktree(&fixture.external, &fixture.external_missing));
    assert!(fixture.has_worktree(&fixture.anchor, &fixture.anchor_missing));
    assert!(
        fixture
            .git(&fixture.external, &["branch", "--list", "feature"])
            .contains("feature")
    );
}

#[test]
fn hq_project_roots_do_not_expand_current_project_scope() {
    let fixture = Fixture::new();
    let roots = serde_json::to_string(&[&fixture.external]).unwrap();
    assert!(!fixture.bonsai(false, &roots, true).status.success());
    assert!(fixture.has_worktree(&fixture.external, &fixture.external_missing));
}

#[test]
fn invalid_hq_project_roots_are_reported_without_expanding_scope() {
    let fixture = Fixture::new();
    let output = fixture.bonsai(true, "{invalid", false);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("_BONSAI_PROJECTS"));
    assert!(fixture.has_worktree(&fixture.external, &fixture.external_missing));
}
