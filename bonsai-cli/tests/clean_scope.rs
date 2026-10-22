use std::path::{Path, PathBuf};
use std::process::Command as GitCommand;

use assert_cmd::Command;
use tempfile::TempDir;

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
    clone: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let clone = dir.join("clone");
        std::fs::create_dir(&clone).unwrap();
        std::fs::write(dir.join("gitconfig-empty"), "").unwrap();
        let repo = Self {
            _tmp: tmp,
            dir,
            clone,
        };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["commit", "--allow-empty", "-m", "seed"]);
        repo
    }

    fn env(&self, command: &mut GitCommand) {
        command
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig-empty"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("HOME", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("CODEX_HOME", self.dir.join("codex"))
            .env("CLAUDE_CONFIG_DIR", self.dir.join("claude"))
            .env("BONSAI_ROOT", self.dir.join("root"))
            .env_remove("_BONSAI_WRAPPED")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
    }

    fn git(&self, args: &[&str]) -> String {
        let mut command = GitCommand::new("git");
        self.env(&mut command);
        let output = command
            .current_dir(&self.clone)
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

    fn add(&self, branch: &str) -> PathBuf {
        let path = self.dir.join(branch);
        self.git(&["worktree", "add", "-b", branch, path.to_str().unwrap()]);
        path
    }

    fn clean(&self) -> Command {
        let mut command = GitCommand::new(assert_cmd::cargo::cargo_bin!("bonsai"));
        self.env(&mut command);
        command.current_dir(&self.clone).arg("clean");
        Command::from_std(command)
    }

    fn planned(&self, selector: &Path) -> serde_json::Value {
        let output = self
            .clean()
            .args(["--dry-run", "--json", "--no-fetch"])
            .arg(selector)
            .assert()
            .success()
            .get_output()
            .clone();
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn clean_selected_branch_limits_plan_and_deletion() {
    let repo = Repo::new();
    let selected = repo.add("selected");
    let untouched = repo.add("untouched");
    let report = repo.planned(Path::new("selected"));
    assert_eq!(report["planned"].as_array().unwrap().len(), 1);
    assert_eq!(report["planned"][0]["branch"], "selected");
    repo.clean()
        .args(["selected", "--yes", "--no-fetch"])
        .assert()
        .success();
    assert!(!selected.exists());
    assert!(untouched.exists());
    assert!(
        repo.git(&["branch", "--list", "untouched"])
            .contains("untouched")
    );
}

#[test]
fn clean_selected_external_path_limits_plan_and_deletion() {
    let repo = Repo::new();
    let selected = repo.add("selected");
    let untouched = repo.add("untouched");
    let report = repo.planned(&selected);
    assert_eq!(report["planned"].as_array().unwrap().len(), 1);
    assert_eq!(report["planned"][0]["path"], selected.to_str().unwrap());
    repo.clean()
        .arg(&selected)
        .args(["--yes", "--no-fetch"])
        .assert()
        .success();
    assert!(!selected.exists());
    assert!(untouched.exists());
}

#[test]
fn clean_invalid_selection_fails_before_fetch_or_partial_removal() {
    let repo = Repo::new();
    let selected = repo.add("selected");
    repo.git(&["remote", "add", "origin", "/nonexistent-clean-scope-origin"]);
    let output = repo
        .clean()
        .args(["selected", "unknown", "--yes"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown"), "{stderr}");
    assert!(!stderr.contains("fetch"), "{stderr}");
    assert!(selected.exists());
}

#[test]
fn clean_selected_dirty_or_missing_worktrees_keep_their_branches() {
    let repo = Repo::new();
    let dirty = repo.add("dirty");
    let missing = repo.add("missing");
    std::fs::write(dirty.join("unfinished"), "work").unwrap();
    std::fs::remove_dir_all(&missing).unwrap();
    let output = repo
        .clean()
        .args(["dirty", "../missing", "--yes", "--json", "--no-fetch"])
        .assert()
        .success()
        .get_output()
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["skipped_dirty"][0]["branch"], "dirty");
    assert_eq!(report["skipped_missing"][0]["branch"], "missing");
    assert!(report["removed"].as_array().unwrap().is_empty());
    assert!(dirty.exists());
    assert!(
        repo.git(&["branch", "--list", "dirty", "missing"])
            .contains("missing")
    );
}

#[test]
fn clean_selection_deduplicates_aliases_and_respects_locks() {
    let repo = Repo::new();
    let selected = repo.add("selected");
    let locked = repo.add("locked");
    repo.git(&["worktree", "lock", locked.to_str().unwrap()]);
    let output = repo
        .clean()
        .args(["selected", "locked"])
        .arg(&selected)
        .args(["--yes", "--json", "--no-fetch"])
        .assert()
        .success()
        .get_output()
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["planned"].as_array().unwrap().len(), 1);
    assert_eq!(report["removed"], serde_json::json!(["selected"]));
    assert!(!selected.exists());
    assert!(locked.exists());
}
