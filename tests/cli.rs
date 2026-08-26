use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// A hermetic fixture: a bare "origin", a working clone seeded with a commit
/// on main, and a dedicated bonsai root — all inside one temp dir, with git
/// and bonsai fully isolated from the host environment.
struct TestRepo {
    _tmp: TempDir,
    dir: PathBuf,
    origin: PathBuf,
    clone: PathBuf,
    root: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let origin = dir.join("origin.git");
        let clone = dir.join("clone");
        let root = dir.join("bonsai-root");
        let repo = TestRepo {
            _tmp: tmp,
            dir,
            origin,
            clone,
            root,
        };
        repo.git(&repo.dir, &["init", "--bare", "-b", "main", "origin.git"]);
        repo.git(
            &repo.dir,
            &["clone", repo.origin.to_str().unwrap(), "clone"],
        );
        std::fs::write(repo.clone.join("README.md"), "seed\n").unwrap();
        repo.git(&repo.clone, &["add", "."]);
        repo.git(&repo.clone, &["commit", "-m", "seed"]);
        repo.git(&repo.clone, &["push", "-u", "origin", "main"]);
        repo
    }

    fn env_vars(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        vec![
            ("HOME", self.dir.clone().into()),
            ("XDG_CONFIG_HOME", self.dir.join(".config").into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("GIT_AUTHOR_NAME", "Test".into()),
            ("GIT_AUTHOR_EMAIL", "test@example.com".into()),
            ("GIT_COMMITTER_NAME", "Test".into()),
            ("GIT_COMMITTER_EMAIL", "test@example.com".into()),
        ]
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let mut cmd = StdCommand::new("git");
        cmd.envs(self.env_vars());
        cmd.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
        let output = cmd.current_dir(dir).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn bonsai(&self, dir: &Path) -> Command {
        let mut cmd = Command::cargo_bin("bonsai").unwrap();
        cmd.envs(self.env_vars())
            .env("BONSAI_ROOT", &self.root)
            .env_remove("_BONSAI_WRAPPED")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .current_dir(dir);
        cmd
    }

    /// Run `bonsai add <branch>` and return the created worktree path.
    fn add(&self, branch: &str) -> PathBuf {
        let output = self
            .bonsai(&self.clone)
            .arg("add")
            .arg(branch)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "bonsai add {branch} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        assert!(
            path.is_dir(),
            "worktree path not created: {}",
            path.display()
        );
        path
    }

    fn worktree_list(&self) -> String {
        self.git(&self.clone, &["worktree", "list", "--porcelain"])
    }
}

const SENTINEL: &str = "__bonsai_cd\u{1f}";

#[test]
fn add_creates_branch_worktree_and_prints_path() {
    let repo = TestRepo::new();
    let path = repo.add("feat-x");
    assert!(path.starts_with(&repo.root));
    assert!(path.join(".git").is_file());
    let branch = repo.git(&path, &["branch", "--show-current"]);
    assert_eq!(branch, "feat-x");
    // Created from origin/main: same tree as the seed commit.
    assert!(path.join("README.md").exists());
}

#[test]
fn add_nested_branch_maps_to_nested_dirs() {
    let repo = TestRepo::new();
    let path = repo.add("feature/login");
    assert!(path.ends_with("feature/login"), "path: {}", path.display());
    assert_eq!(
        repo.git(&path, &["branch", "--show-current"]),
        "feature/login"
    );
}

#[test]
fn add_is_idempotent_for_existing_bonsai_worktree() {
    let repo = TestRepo::new();
    let first = repo.add("feat-x");
    let second = repo.add("feat-x");
    assert_eq!(first, second);
}

#[test]
fn add_rejects_branch_checked_out_in_main_worktree() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.clone)
        .args(["add", "main"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("checked out"));
}

#[test]
fn add_rejects_invalid_branch_name() {
    let repo = TestRepo::new();
    for bad in ["-oops", "a..b", "a b"] {
        repo.bonsai(&repo.clone)
            .args(["add", "--", bad])
            .assert()
            .failure()
            .stderr(predicate::str::contains("invalid branch name"));
    }
}

#[test]
fn add_tracks_remote_only_branch() {
    let repo = TestRepo::new();
    // Publish a branch that exists only on the remote.
    repo.git(&repo.clone, &["push", "origin", "main:remote-feat"]);
    repo.git(&repo.clone, &["fetch", "origin"]);
    let path = repo.add("remote-feat");
    let upstream = repo.git(
        &path,
        &["rev-parse", "--abbrev-ref", "remote-feat@{upstream}"],
    );
    assert_eq!(upstream, "origin/remote-feat");
}

#[test]
fn add_without_argument_fails_when_not_a_tty() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.clone)
        .arg("add")
        .assert()
        .failure()
        .stderr(predicate::str::contains("terminal"));
}

#[test]
fn wrapped_mode_emits_cd_sentinel_as_last_line() {
    let repo = TestRepo::new();
    let output = repo
        .bonsai(&repo.clone)
        .env("_BONSAI_WRAPPED", "1")
        .args(["add", "feat-w"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().last().unwrap();
    assert!(last.starts_with(SENTINEL), "last line: {last:?}");
    assert!(Path::new(&last[SENTINEL.len()..]).is_dir());
}

#[test]
fn list_shows_main_and_bonsai_worktrees() {
    let repo = TestRepo::new();
    repo.add("feat-l");
    repo.bonsai(&repo.clone)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("main").and(predicate::str::contains("feat-l")));
}

#[test]
fn commands_behave_identically_from_inside_a_worktree() {
    let repo = TestRepo::new();
    let path = repo.add("feat-inside");
    let from_clone = repo
        .bonsai(&repo.clone)
        .arg("list")
        .output()
        .unwrap()
        .stdout;
    let from_worktree = repo.bonsai(&path).arg("list").output().unwrap().stdout;
    assert_eq!(from_clone, from_worktree);
    // Adding from inside a worktree anchors on the main repo too.
    let output = repo
        .bonsai(&path)
        .args(["add", "feat-nested"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn cd_resolves_exact_branch_to_path() {
    let repo = TestRepo::new();
    let path = repo.add("feat-cd");
    repo.bonsai(&repo.clone)
        .args(["cd", "feat-cd"])
        .assert()
        .success()
        .stdout(format!("{}\n", path.display()));
}

#[test]
fn cd_works_globally_outside_any_repo() {
    let repo = TestRepo::new();
    let path = repo.add("feat-global");
    repo.bonsai(&repo.dir)
        .args(["cd", "feat-global"])
        .assert()
        .success()
        .stdout(format!("{}\n", path.display()));
}

#[test]
fn remove_deletes_worktree_but_keeps_branch_by_default() {
    let repo = TestRepo::new();
    let path = repo.add("feat-rm");
    repo.bonsai(&repo.clone)
        .args(["remove", "feat-rm"])
        .assert()
        .success();
    assert!(!path.exists());
    assert!(!repo.worktree_list().contains("feat-rm"));
    // Branch survives; -d is explicit.
    repo.git(&repo.clone, &["show-ref", "--verify", "refs/heads/feat-rm"]);
    // Empty parent dirs under the root were cleaned up.
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn remove_with_delete_branch_flag_deletes_merged_branch() {
    let repo = TestRepo::new();
    repo.add("feat-rmd");
    repo.bonsai(&repo.clone)
        .args(["remove", "feat-rmd", "-d"])
        .assert()
        .success();
    let refs = repo.git(&repo.clone, &["for-each-ref", "refs/heads"]);
    assert!(!refs.contains("feat-rmd"));
}

#[test]
fn remove_refuses_dirty_worktree_without_force() {
    let repo = TestRepo::new();
    let path = repo.add("feat-dirty");
    std::fs::write(path.join("wip.txt"), "wip\n").unwrap();
    repo.bonsai(&repo.clone)
        .args(["remove", "feat-dirty"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));
    assert!(path.exists());
    repo.bonsai(&repo.clone)
        .args(["remove", "feat-dirty", "--force"])
        .assert()
        .success();
    assert!(!path.exists());
}

#[test]
fn remove_from_inside_the_worktree_sends_shell_home() {
    let repo = TestRepo::new();
    let path = repo.add("feat-here");
    let output = repo
        .bonsai(&path)
        .env("_BONSAI_WRAPPED", "1")
        .args(["remove", "feat-here"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().last().unwrap();
    assert_eq!(last, format!("{SENTINEL}{}", repo.clone.display()));
    assert!(!path.exists());
}

#[test]
fn clean_removes_squash_merged_branch_with_gone_upstream() {
    let repo = TestRepo::new();
    let path = repo.add("feat-done");
    std::fs::write(path.join("feature.txt"), "done\n").unwrap();
    repo.git(&path, &["add", "."]);
    repo.git(&path, &["commit", "-m", "feature"]);
    repo.git(&path, &["push", "-u", "origin", "feat-done"]);
    // Squash-merge on main and delete the remote branch — the GitHub PR flow.
    repo.git(&repo.clone, &["merge", "--squash", "feat-done"]);
    repo.git(&repo.clone, &["commit", "-m", "feat-done (squashed)"]);
    repo.git(&repo.clone, &["push", "origin", "main"]);
    repo.git(&repo.clone, &["push", "origin", ":feat-done"]);

    repo.bonsai(&repo.clone)
        .args(["clean", "--yes"])
        .assert()
        .success();
    assert!(!path.exists());
    let refs = repo.git(&repo.clone, &["for-each-ref", "refs/heads"]);
    assert!(!refs.contains("feat-done"));
}

#[test]
fn clean_dry_run_touches_nothing() {
    let repo = TestRepo::new();
    // A no-commit branch counts as merged.
    let path = repo.add("feat-fresh");
    repo.bonsai(&repo.clone)
        .args(["clean", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("feat-fresh").and(predicate::str::contains("dry run")));
    assert!(path.exists());
}

#[test]
fn clean_skips_dirty_worktrees_even_with_yes() {
    let repo = TestRepo::new();
    let path = repo.add("feat-wip");
    std::fs::write(path.join("wip.txt"), "wip\n").unwrap();
    repo.bonsai(&repo.clone)
        .args(["clean", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipping 'feat-wip'"));
    assert!(path.exists());
    repo.git(
        &repo.clone,
        &["show-ref", "--verify", "refs/heads/feat-wip"],
    );
}

#[test]
fn clean_respects_protected_globs() {
    let repo = TestRepo::new();
    let path = repo.add("release/1.0");
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        "[clean]\nprotected = [\"release/*\"]\n",
    )
    .unwrap();
    repo.bonsai(&repo.clone)
        .args(["clean", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to clean"));
    assert!(path.exists());
}

#[test]
fn prune_cleans_up_after_manual_deletion() {
    let repo = TestRepo::new();
    let path = repo.add("feat-gone");
    std::fs::remove_dir_all(&path).unwrap();
    repo.bonsai(&repo.clone)
        .args(["prune", "--yes"])
        .assert()
        .success();
    assert!(!repo.worktree_list().contains("feat-gone"));
}

#[test]
fn prune_deletes_orphaned_directories() {
    let repo = TestRepo::new();
    let path = repo.add("feat-anchor");
    let bonsai_dir = path.parent().unwrap();
    let orphan = bonsai_dir.join("orphan");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join(".git"), "gitdir: /nonexistent\n").unwrap();
    repo.bonsai(&repo.clone)
        .args(["prune", "--yes"])
        .assert()
        .success();
    assert!(!orphan.exists());
    assert!(path.exists(), "registered worktrees must survive prune");
}

#[test]
fn prune_all_sweeps_worktrees_of_deleted_repos() {
    let repo = TestRepo::new();
    let path = repo.add("feat-lost");
    // Deleting the whole clone leaves the worktree pointing at nothing.
    std::fs::remove_dir_all(&repo.clone).unwrap();
    repo.bonsai(&repo.dir)
        .args(["prune", "--all", "--yes"])
        .assert()
        .success();
    assert!(!path.exists());
}

#[test]
fn repo_config_overrides_and_cli_flag_wins() {
    let repo = TestRepo::new();
    let alt_root = repo.dir.join("alt-root");
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        format!("root = \"{}\"\n", alt_root.display()),
    )
    .unwrap();
    // Drop the BONSAI_ROOT env var: env sits above repo config in the
    // hierarchy and would rightfully win otherwise.
    let output = repo
        .bonsai(&repo.clone)
        .env_remove("BONSAI_ROOT")
        .args(["add", "feat-cfg"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    assert!(path.starts_with(&alt_root), "path: {}", path.display());

    let flag_root = repo.dir.join("flag-root");
    let output = repo
        .bonsai(&repo.clone)
        .args(["add", "feat-flag", "--root", flag_root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    assert!(path.starts_with(&flag_root), "path: {}", path.display());
}

#[test]
fn add_copies_configured_files_and_runs_post_add_hook() {
    let repo = TestRepo::new();
    std::fs::write(repo.clone.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        "[add]\ncopy = [\".env\"]\npost_add = \"touch hook-ran\"\n",
    )
    .unwrap();
    let path = repo.add("feat-hook");
    assert_eq!(
        std::fs::read_to_string(path.join(".env")).unwrap(),
        "SECRET=1\n"
    );
    assert!(path.join("hook-ran").exists());
}

#[test]
fn init_scripts_are_valid_shell() {
    let repo = TestRepo::new();
    for (shell, check_args) in [
        ("zsh", vec!["-n"]),
        ("bash", vec!["-n"]),
        ("fish", vec!["--no-execute"]),
    ] {
        let Ok(shell_path) = which(shell) else {
            eprintln!("skipping {shell}: not installed");
            continue;
        };
        let output = repo
            .bonsai(&repo.dir)
            .args(["init", shell])
            .output()
            .unwrap();
        assert!(output.status.success());
        let script = repo.dir.join(format!("init.{shell}"));
        std::fs::write(&script, &output.stdout).unwrap();
        let check = StdCommand::new(shell_path)
            .args(&check_args)
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "{shell} rejected init script: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }
}

#[test]
fn new_branch_has_no_upstream() {
    // git would auto-track the base (origin/main) on `-b`; that misleads
    // `git push` and breaks clean's gone-upstream detection later.
    let repo = TestRepo::new();
    let path = repo.add("feat-notrack");
    let out = StdCommand::new("git")
        .args(["rev-parse", "--abbrev-ref", "feat-notrack@{upstream}"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "new branch must not have an upstream"
    );
}

#[test]
fn base_head_resolves_against_current_worktree() {
    // Stacked branches: from inside worktree A, `--base HEAD` means A's
    // HEAD, not the main checkout's.
    let repo = TestRepo::new();
    let a = repo.add("feat-a");
    std::fs::write(a.join("a.txt"), "a\n").unwrap();
    repo.git(&a, &["add", "."]);
    repo.git(&a, &["commit", "-m", "a"]);
    let a_head = repo.git(&a, &["rev-parse", "HEAD"]);

    let output = repo
        .bonsai(&a)
        .args(["add", "feat-a2", "--base", "HEAD"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let a2 = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    assert_eq!(repo.git(&a2, &["rev-parse", "HEAD"]), a_head);
    assert!(a2.join("a.txt").exists());
}

#[test]
fn copy_prefers_current_worktree_over_main() {
    let repo = TestRepo::new();
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        "[add]\ncopy = [\".env\"]\n",
    )
    .unwrap();
    std::fs::write(repo.clone.join(".env"), "FROM=main\n").unwrap();
    let src = repo.add("feat-src");
    // The copied .env is then modified in the worktree we stand in.
    std::fs::write(src.join(".env"), "FROM=worktree\n").unwrap();

    let output = repo
        .bonsai(&src)
        .args(["add", "feat-dst"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let dst = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    assert_eq!(
        std::fs::read_to_string(dst.join(".env")).unwrap(),
        "FROM=worktree\n"
    );
}

#[test]
fn bonsai_toml_is_read_from_current_worktree() {
    let repo = TestRepo::new();
    let wt = repo.add("feat-cfgwt");
    let wt_root = repo.dir.join("wt-config-root");
    // Untracked config in this worktree only.
    std::fs::write(
        wt.join(".bonsai.toml"),
        format!("root = \"{}\"\n", wt_root.display()),
    )
    .unwrap();
    let output = repo
        .bonsai(&wt)
        .env_remove("BONSAI_ROOT")
        .args(["add", "feat-from-wt"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    assert!(path.starts_with(&wt_root), "path: {}", path.display());
}

#[test]
fn git_config_bonsai_layer_between_toml_and_env() {
    let repo = TestRepo::new();
    let toml_root = repo.dir.join("toml-root");
    let gitcfg_root = repo.dir.join("gitcfg-root");
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        format!("root = \"{}\"\n", toml_root.display()),
    )
    .unwrap();
    repo.git(
        &repo.clone,
        &["config", "bonsai.root", gitcfg_root.to_str().unwrap()],
    );

    // git config beats .bonsai.toml...
    let output = repo
        .bonsai(&repo.clone)
        .env_remove("BONSAI_ROOT")
        .args(["add", "feat-gitcfg"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    assert!(path.starts_with(&gitcfg_root), "path: {}", path.display());

    // ...but BONSAI_* env beats git config (BONSAI_ROOT is set by bonsai()).
    let path = repo.add("feat-envwins");
    assert!(path.starts_with(&repo.root), "path: {}", path.display());
}

#[test]
fn git_config_multi_valued_copy() {
    let repo = TestRepo::new();
    std::fs::write(repo.clone.join(".env"), "A=1\n").unwrap();
    std::fs::write(repo.clone.join(".envrc"), "export A=1\n").unwrap();
    repo.git(&repo.clone, &["config", "--add", "bonsai.add.copy", ".env"]);
    repo.git(
        &repo.clone,
        &["config", "--add", "bonsai.add.copy", ".envrc"],
    );
    let path = repo.add("feat-multicopy");
    assert!(path.join(".env").exists());
    assert!(path.join(".envrc").exists());
}

#[test]
fn checkout_default_remote_is_respected() {
    let repo = TestRepo::new();
    // A second remote holding a branch that only exists there.
    repo.git(&repo.dir, &["init", "--bare", "-b", "main", "upstream.git"]);
    let upstream = repo.dir.join("upstream.git");
    repo.git(
        &repo.clone,
        &["remote", "add", "upstream", upstream.to_str().unwrap()],
    );
    repo.git(&repo.clone, &["push", "upstream", "main:up-only"]);
    repo.git(&repo.clone, &["fetch", "upstream"]);
    repo.git(
        &repo.clone,
        &["config", "checkout.defaultRemote", "upstream"],
    );

    let path = repo.add("up-only");
    let tracking = repo.git(&path, &["rev-parse", "--abbrev-ref", "up-only@{upstream}"]);
    assert_eq!(tracking, "upstream/up-only");
}

#[test]
fn clean_removes_never_pushed_squash_merged_branch() {
    // The local-only PR flow: branch never pushed, squash-merged into main.
    // Works only because new branches carry no auto-upstream (--no-track).
    let repo = TestRepo::new();
    let path = repo.add("feat-local");
    std::fs::write(path.join("local.txt"), "x\n").unwrap();
    repo.git(&path, &["add", "."]);
    repo.git(&path, &["commit", "-m", "local feature"]);
    repo.git(&repo.clone, &["merge", "--squash", "feat-local"]);
    repo.git(&repo.clone, &["commit", "-m", "feat-local (squashed)"]);
    repo.git(&repo.clone, &["push", "origin", "main"]);

    repo.bonsai(&repo.clone)
        .args(["clean", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("squash-merged"));
    assert!(!path.exists());
    let refs = repo.git(&repo.clone, &["for-each-ref", "refs/heads"]);
    assert!(!refs.contains("feat-local"));
}

#[test]
fn symlinked_root_is_handled() {
    // git registers worktrees under the resolved path (macOS: /tmp ->
    // /private/tmp), so a symlinked BONSAI_ROOT must not break recognition.
    let repo = TestRepo::new();
    let real = repo.dir.join("real-root");
    let link = repo.dir.join("link-root");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let output = repo
        .bonsai(&repo.clone)
        .env("BONSAI_ROOT", &link)
        .args(["add", "feat-sym"])
        .output()
        .unwrap();
    assert!(output.status.success());
    // list must not mislabel the bonsai worktree as the main one.
    let list = repo
        .bonsai(&repo.clone)
        .env("BONSAI_ROOT", &link)
        .arg("list")
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&list.stdout).to_string();
    let feat_line = list.lines().find(|l| l.starts_with("feat-sym")).unwrap();
    assert!(!feat_line.contains("main"), "line: {feat_line}");
    // remove must recognize it as a bonsai worktree.
    repo.bonsai(&repo.clone)
        .env("BONSAI_ROOT", &link)
        .args(["remove", "feat-sym"])
        .assert()
        .success();
}

#[test]
fn completions_are_generated() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.dir)
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_bonsai"));
}

fn which(bin: &str) -> Result<PathBuf, ()> {
    let output = StdCommand::new("which").arg(bin).output().map_err(|_| ())?;
    if output.status.success() {
        Ok(PathBuf::from(
            String::from_utf8_lossy(&output.stdout).trim(),
        ))
    } else {
        Err(())
    }
}
