use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    temp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let root = temp.path().join("root");
        std::fs::create_dir(&repo).unwrap();
        let fixture = Self { temp, repo, root };
        fixture.git(&fixture.repo, &["init", "-b", "main"]);
        fixture.git(&fixture.repo, &["commit", "--allow-empty", "-m", "seed"]);
        fixture
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
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
            .env("BONSAI_ROOT", &self.root)
            .env("BONSAI_ADD__INSTALL", "false")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("_BONSAI_WRAPPED");
        command
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.command("git", cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn bonsai(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_bonsai"), cwd)
            .args(args)
            .output()
            .unwrap()
    }

    fn add(&self, branch: &str) -> PathBuf {
        let output = self.bonsai(&self.repo, &["add", branch]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
    }
}

#[test]
fn clean_keeps_unmerged_branch_when_upstream_disappeared() {
    let fixture = Fixture::new();
    let worktree = fixture.add("ab/unmerged");
    fixture.git(&worktree, &["commit", "--allow-empty", "-m", "unmerged"]);
    std::fs::write(worktree.join("valuable"), "unmerged work").unwrap();
    fixture.git(&worktree, &["add", "."]);
    fixture.git(&worktree, &["commit", "-m", "valuable"]);
    fixture.git(&fixture.repo, &["config", "branch.ab/unmerged.remote", "."]);
    fixture.git(
        &fixture.repo,
        &["config", "branch.ab/unmerged.merge", "refs/heads/deleted"],
    );
    let output = fixture.bonsai(&fixture.repo, &["clean", "--yes", "--no-fetch", "--json"]);
    assert!(output.status.success());
    assert!(worktree.join("valuable").exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["planned"], serde_json::json!([]));
}

#[test]
fn clean_keeps_commits_added_after_a_squash_merge_and_upstream_deletion() {
    let fixture = Fixture::new();
    let worktree = fixture.add("ab/post-merge");
    std::fs::write(worktree.join("feature"), "integrated\n").unwrap();
    fixture.git(&worktree, &["add", "."]);
    fixture.git(&worktree, &["commit", "-m", "feature"]);
    fixture.git(&fixture.repo, &["merge", "--squash", "ab/post-merge"]);
    fixture.git(&fixture.repo, &["commit", "-m", "squashed feature"]);
    std::fs::write(worktree.join("valuable"), "later local work\n").unwrap();
    fixture.git(&worktree, &["add", "."]);
    fixture.git(&worktree, &["commit", "-m", "later work"]);
    fixture.git(
        &fixture.repo,
        &["config", "branch.ab/post-merge.remote", "."],
    );
    fixture.git(
        &fixture.repo,
        &["config", "branch.ab/post-merge.merge", "refs/heads/deleted"],
    );
    let output = fixture.bonsai(&fixture.repo, &["clean", "--yes", "--no-fetch"]);
    assert!(output.status.success());
    assert!(worktree.join("valuable").exists());
}

#[test]
fn clean_requires_integration_into_the_selected_remote() {
    let fixture = Fixture::new();
    let worktree = fixture.add("ab/other-remote");
    std::fs::write(worktree.join("feature"), "feature\n").unwrap();
    fixture.git(&worktree, &["add", "."]);
    fixture.git(&worktree, &["commit", "-m", "feature"]);
    fixture.git(
        &fixture.repo,
        &["remote", "add", "origin", fixture.repo.to_str().unwrap()],
    );
    fixture.git(
        &fixture.repo,
        &["remote", "add", "other", fixture.repo.to_str().unwrap()],
    );
    fixture.git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    fixture.git(
        &fixture.repo,
        &["update-ref", "refs/remotes/other/main", "ab/other-remote"],
    );
    let output = fixture.bonsai(
        &fixture.repo,
        &["--remote", "origin", "clean", "--yes", "--no-fetch"],
    );
    assert!(output.status.success());
    assert!(worktree.exists());
    let output = fixture.bonsai(
        &fixture.repo,
        &["--remote", "other", "clean", "--yes", "--no-fetch"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!worktree.exists());
}

#[test]
fn remove_preflights_all_targets_before_deleting_anything() {
    let fixture = Fixture::new();
    let first = fixture.add("ab/first");
    let second = fixture.add("ab/second");
    fixture.git(&second, &["commit", "--allow-empty", "-m", "unmerged"]);
    let output = fixture.bonsai(&first, &["remove", "ab/first", "ab/second", "-d"]);
    assert!(!output.status.success());
    assert!(
        first.is_dir(),
        "an early target must survive a predictable later failure"
    );
    assert!(second.is_dir(), "an unmerged branch must keep its checkout");
}

#[test]
fn prune_retains_unrecognized_workspace_files() {
    let fixture = Fixture::new();
    let custom = fixture.root.join("project/custom.code-workspace");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    std::fs::write(&custom, r#"{"settings":{"editor.fontSize":18}}"#).unwrap();
    let output = fixture.bonsai(&fixture.repo, &["prune", "--all", "--yes"]);
    assert!(output.status.success());
    assert!(custom.exists());
}

#[cfg(unix)]
#[test]
fn prune_never_follows_external_symlinks() {
    let fixture = Fixture::new();
    let external = fixture.temp.path().join("external");
    std::fs::create_dir_all(external.join("orphan")).unwrap();
    std::fs::write(
        external.join("orphan/.git"),
        "gitdir: /definitely-missing-bonsai-test\n",
    )
    .unwrap();
    std::fs::write(external.join("custom.code-workspace"), "{}").unwrap();
    std::fs::create_dir_all(&fixture.root).unwrap();
    std::os::unix::fs::symlink(&external, fixture.root.join("link")).unwrap();
    std::os::unix::fs::symlink(&fixture.root, fixture.root.join("cycle")).unwrap();
    std::os::unix::fs::symlink(external.join("absent"), fixture.root.join("dangling")).unwrap();
    let output = fixture.bonsai(&fixture.repo, &["prune", "--all", "--yes"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(external.join("orphan/.git").exists());
    assert!(external.join("custom.code-workspace").exists());
}

#[test]
fn prune_preserves_malformed_git_metadata() {
    let fixture = Fixture::new();
    let orphan = fixture.root.join("unknown");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join(".git"), "not a gitdir pointer").unwrap();
    let output = fixture.bonsai(&fixture.repo, &["prune", "--all", "--yes"]);
    assert!(output.status.success());
    assert!(orphan.exists());
}

#[test]
fn repository_lock_prevents_overlapping_mutations() {
    let fixture = Fixture::new();
    let worktree = fixture.add("ab/locked-operation");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.repo.join(".git/bonsai.lock"))
        .unwrap();
    lock.lock().unwrap();
    let output = fixture.bonsai(&fixture.repo, &["remove", "ab/locked-operation"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another bonsai mutation"));
    assert!(worktree.is_dir());
    drop(lock);
    assert!(
        fixture
            .bonsai(&fixture.repo, &["remove", "ab/locked-operation"])
            .status
            .success()
    );
}

#[test]
fn global_prune_waits_for_other_root_mutations() {
    let fixture = Fixture::new();
    fixture.add("ab/root-operation");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root.join(".locks/mutations.lock"))
        .unwrap();
    lock.lock_shared().unwrap();
    let output = fixture.bonsai(&fixture.repo, &["prune", "--all", "--yes"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another bonsai operation"));
    drop(lock);
    assert!(
        fixture
            .bonsai(&fixture.repo, &["prune", "--all", "--yes"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn linked_lock_files_are_rejected_without_modifying_the_target() {
    let fixture = Fixture::new();
    let important = fixture.temp.path().join("important");
    std::fs::write(&important, "keep me").unwrap();
    let lock = fixture.repo.join(".git/bonsai.lock");
    std::os::unix::fs::symlink(&important, &lock).unwrap();
    let output = fixture.bonsai(&fixture.repo, &["add", "ab/symlink-lock"]);
    assert!(!output.status.success());
    std::fs::remove_file(&lock).unwrap();
    std::fs::hard_link(&important, &lock).unwrap();
    let output = fixture.bonsai(&fixture.repo, &["add", "ab/hardlink-lock"]);
    assert!(!output.status.success());
    assert_eq!(std::fs::read_to_string(important).unwrap(), "keep me");
}

#[cfg(unix)]
#[test]
fn changed_ref_survives_partial_removal_and_emits_cwd_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let worktree = fixture.add("ab/race");
    fixture.git(&fixture.repo, &["commit", "--allow-empty", "-m", "newer"]);
    let newer = fixture.git(&fixture.repo, &["rev-parse", "HEAD"]);
    let real_git = Command::new("/bin/sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let shims = fixture.temp.path().join("shims");
    std::fs::create_dir(&shims).unwrap();
    let shim = shims.join("git");
    std::fs::write(&shim, "#!/bin/sh\ncase \"$*\" in\n  *' update-ref -d refs/heads/ab/race '*) \"$REAL_GIT\" -C \"$TEST_REPO\" update-ref refs/heads/ab/race \"$NEW_TIP\" ;;\nesac\nexec \"$REAL_GIT\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![shims];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = fixture
        .command(env!("CARGO_BIN_EXE_bonsai"), &worktree)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("REAL_GIT", String::from_utf8_lossy(&real_git.stdout).trim())
        .env("TEST_REPO", &fixture.repo)
        .env("NEW_TIP", &newer)
        .env("_BONSAI_WRAPPED", "1")
        .args(["remove", "ab/race", "-d"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!worktree.exists());
    assert_eq!(
        fixture.git(&fixture.repo, &["rev-parse", "refs/heads/ab/race"]),
        newer
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(fixture.repo.to_str().unwrap()),
        "partial removal must tell the shell where to recover: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(unix)]
#[test]
fn acquiring_mutation_lock_discards_initial_worktree_inventory() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let real_git = Command::new("/bin/sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let shims = fixture.temp.path().join("shims");
    std::fs::create_dir(&shims).unwrap();
    let shim = shims.join("git");
    // Add a worktree after initial discovery, immediately before the caller
    // opens its repository lock. The locked operation must discover it anew.
    std::fs::write(&shim, "#!/bin/sh\ncase \"$*\" in\n  *' rev-parse --path-format=absolute --git-common-dir') PATH=\"$ORIGINAL_PATH\" \"$TEST_BINARY\" add ab/late >/dev/null || exit $? ;;\nesac\nexec \"$REAL_GIT\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![shims];
    paths.extend(std::env::split_paths(&original_path));
    let output = fixture
        .command(env!("CARGO_BIN_EXE_bonsai"), &fixture.repo)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("ORIGINAL_PATH", original_path)
        .env("TEST_BINARY", env!("CARGO_BIN_EXE_bonsai"))
        .env("REAL_GIT", String::from_utf8_lossy(&real_git.stdout).trim())
        .args(["remove", "ab/late"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("[ab/late] removed worktree"));
}
