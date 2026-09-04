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
        let dir = canon(tmp.path());
        // Isolate git from the host: an empty file beats /dev/null (which
        // does not exist on Windows).
        std::fs::write(dir.join("gitconfig-empty"), "").unwrap();
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
            ("USERPROFILE", self.dir.clone().into()),
            ("XDG_CONFIG_HOME", self.dir.join(".config").into()),
            ("XDG_DATA_HOME", self.dir.join(".local/share").into()),
            ("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig-empty").into()),
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

    /// Register a linked worktree outside the configured Bonsai root, as a
    /// third-party worktree manager would.
    fn add_external(&self, branch: &str) -> PathBuf {
        let path = self
            .dir
            .join("third-party-worktrees")
            .join(branch.replace('/', "--"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        self.git(
            &self.clone,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        canon(&path)
    }

    fn worktree_list(&self) -> String {
        self.git(&self.clone, &["worktree", "list", "--porcelain"])
    }

    /// Commit files in the clone and push them to origin/main so they show
    /// up in worktrees created from it.
    fn commit_files(&self, files: &[(&str, &str)]) {
        for (name, content) in files {
            std::fs::write(self.clone.join(name), content).unwrap();
        }
        self.git(&self.clone, &["add", "."]);
        self.git(&self.clone, &["commit", "-m", "fixtures"]);
        self.git(&self.clone, &["push", "origin", "main"]);
    }

    /// Directory holding fake package-manager executables.
    fn fake_bin_dir(&self) -> PathBuf {
        let dir = self.dir.join("fakebin");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake package manager recording its argv into `<name>-args.txt` in
    /// its cwd (i.e. the new worktree) before exiting with `code`.
    fn fake_pm_with_exit(&self, name: &str, code: i32) {
        let dir = self.fake_bin_dir();
        if cfg!(windows) {
            std::fs::write(
                dir.join(format!("{name}.cmd")),
                format!("@echo %*> {name}-args.txt\r\n@exit /b {code}\r\n"),
            )
            .unwrap();
        } else {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\nprintf '%s' \"$*\" > {name}-args.txt\nexit {code}\n"),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    fn fake_pm(&self, name: &str) {
        self.fake_pm_with_exit(name, 0);
    }

    /// A fake interactive harness recording its argv and working directory.
    fn fake_harness(&self, name: &str) {
        let dir = self.fake_bin_dir();
        let args = self.dir.join(format!("{name}-args.txt"));
        let cwd = self.dir.join(format!("{name}-cwd.txt"));
        let wrapped = self.dir.join(format!("{name}-wrapped.txt"));
        if cfg!(windows) {
            std::fs::write(
                dir.join(format!("{name}.cmd")),
                format!(
                    "@echo %*> \"{}\"\r\n@cd > \"{}\"\r\n@echo %_BONSAI_WRAPPED%> \"{}\"\r\n",
                    args.display(),
                    cwd.display(),
                    wrapped.display()
                ),
            )
            .unwrap();
        } else {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s' \"$*\" > '{}'\npwd > '{}'\nprintf '%s' \"${{_BONSAI_WRAPPED-unset}}\" > '{}'\n",
                    args.display(),
                    cwd.display(),
                    wrapped.display()
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    /// The host PATH with the fake-binary dir prepended (fakes win).
    fn path_with_fakebin(&self) -> std::ffi::OsString {
        let mut paths = vec![self.fake_bin_dir()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).unwrap()
    }

    /// A PATH reduced to the fake-binary dir plus every host dir containing
    /// git, making "package manager not installed" reproducible even on
    /// machines that have the real tools.
    fn restricted_path(&self) -> std::ffi::OsString {
        let git_name = if cfg!(windows) { "git.exe" } else { "git" };
        let mut paths = vec![self.fake_bin_dir()];
        paths.extend(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|dir| dir.join(git_name).is_file()),
        );
        std::env::join_paths(paths).unwrap()
    }

    /// `bonsai add` with a custom PATH; returns the worktree path + stderr.
    fn add_with_path(&self, branch: &str, path_env: &std::ffi::OsStr) -> (PathBuf, String) {
        let output = self
            .bonsai(&self.clone)
            .env("PATH", path_env)
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
        (path, String::from_utf8_lossy(&output.stderr).into_owned())
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
fn add_slugifies_branch_and_preserves_nested_dirs() {
    let repo = TestRepo::new();
    let path = repo.add("AB/Fix Login #42");
    assert!(
        path.ends_with("ab/fix-login-42"),
        "path: {}",
        path.display()
    );
    assert_eq!(
        repo.git(&path, &["branch", "--show-current"]),
        "ab/fix-login-42"
    );
}

#[test]
fn add_is_idempotent_for_existing_bonsai_worktree() {
    let repo = TestRepo::new();
    let first = repo.add("feat-x");
    std::fs::rename(&repo.origin, repo.dir.join("origin-offline.git")).unwrap();
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
fn add_rejects_paths_outside_the_bonsai_project_directory() {
    let repo = TestRepo::new();
    let outside = repo.dir.join("outside-bonsai");

    repo.bonsai(&repo.clone)
        .args(["add", "feat-outside", "--path", outside.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must stay inside"));

    assert!(!outside.exists());
    assert!(!repo.worktree_list().contains("feat-outside"));
}

#[test]
fn add_rejects_invalid_branch_name() {
    let repo = TestRepo::new();
    for bad in ["...", "/foo", "foo//bar"] {
        repo.bonsai(&repo.clone)
            .args(["add", "--", bad])
            .assert()
            .failure()
            .stderr(predicate::str::contains("empty after slugifying"));
    }
}

#[test]
fn add_fetches_latest_default_branch_by_default() {
    let repo = TestRepo::new();
    let publisher = repo.dir.join("publisher");
    repo.git(
        &repo.dir,
        &[
            "clone",
            repo.origin.to_str().unwrap(),
            publisher.to_str().unwrap(),
        ],
    );
    std::fs::write(publisher.join("latest.txt"), "latest\n").unwrap();
    repo.git(&publisher, &["add", "."]);
    repo.git(&publisher, &["commit", "-m", "latest"]);
    repo.git(&publisher, &["push", "origin", "main"]);

    let path = repo.add("feat-latest");
    assert!(path.join("latest.txt").is_file());
}

#[test]
fn add_fetch_can_be_disabled_in_config() {
    let repo = TestRepo::new();
    let publisher = repo.dir.join("publisher");
    repo.git(
        &repo.dir,
        &[
            "clone",
            repo.origin.to_str().unwrap(),
            publisher.to_str().unwrap(),
        ],
    );
    std::fs::write(publisher.join("latest.txt"), "latest\n").unwrap();
    repo.git(&publisher, &["add", "."]);
    repo.git(&publisher, &["commit", "-m", "latest"]);
    repo.git(&publisher, &["push", "origin", "main"]);
    std::fs::write(repo.clone.join(".bonsai.toml"), "[add]\nfetch = false\n").unwrap();

    let path = repo.add("feat-stale");
    assert!(!path.join("latest.txt").exists());
}

#[test]
fn add_tracks_remote_only_branch() {
    let repo = TestRepo::new();
    // Publish a branch that exists only on the remote.
    repo.git(&repo.clone, &["push", "origin", "main:remote-feat"]);
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
        .stdout(
            predicate::str::contains("main")
                .and(predicate::str::contains("main (root)"))
                .and(predicate::str::contains("feat-l")),
        );
}

#[test]
fn list_aligns_branch_and_directory_columns() {
    let repo = TestRepo::new();
    let short = repo.add("x");
    let long = repo.add("feature/a-much-longer-branch");
    let output = repo.bonsai(&repo.clone).arg("ls").output().unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();

    let rows = [
        ("main (root)", repo.clone.as_path()),
        ("x", short.as_path()),
        ("feature/a-much-longer-branch", long.as_path()),
    ];
    let directory_columns = rows.map(|(branch, path)| {
        let line = output
            .lines()
            .find(|line| line.starts_with(branch))
            .unwrap();
        line.find(path.to_str().unwrap()).unwrap()
    });
    assert!(
        directory_columns
            .iter()
            .all(|column| *column == directory_columns[0]),
        "directory columns are not aligned:\n{output}"
    );
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
    let output = repo
        .bonsai(&repo.clone)
        .args(["cd", "feat-cd"])
        .output()
        .unwrap();
    assert!(output.status.success());
    // git may report forward-slash paths on Windows; compare components.
    assert_eq!(
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
        path
    );
}

#[test]
fn external_worktrees_are_visible_but_not_bonsai_owned() {
    let repo = TestRepo::new();
    let external = repo.add_external("feat-external");

    let output = repo
        .bonsai(&external)
        .args(["cd", "feat-external"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
        external
    );

    let output = repo
        .bonsai(&external)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let entries: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = entries.as_array().unwrap();
    assert_eq!(
        entries.iter().filter(|entry| entry["main"] == true).count(),
        1
    );
    let entry = entries
        .iter()
        .find(|entry| entry["branch"] == "feat-external")
        .unwrap();
    assert_eq!(entry["main"], false);
    assert_eq!(entry["external"], true);

    // Creation/adoption remains exclusive to the configured Bonsai root.
    repo.bonsai(&external)
        .args(["add", "feat-external"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("checked out at"));

    // Destructive lifecycle commands continue to target managed worktrees.
    repo.bonsai(&external)
        .args(["remove", "feat-external"])
        .assert()
        .failure();
    assert!(external.is_dir());
}

#[test]
fn workspace_includes_external_worktrees_without_managed_ones() {
    let repo = TestRepo::new();
    let external = repo.add_external("feat-external-workspace");

    let output = repo.bonsai(&repo.clone).arg("workspace").output().unwrap();
    assert!(output.status.success());
    let file = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let workspace: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let folders = workspace["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 2);
    let external_folder = folders
        .iter()
        .find(|folder| folder["name"] == "feat-external-workspace (external)")
        .unwrap();
    assert_eq!(
        PathBuf::from(external_folder["path"].as_str().unwrap()),
        external
    );
}

#[test]
fn cd_works_globally_outside_any_repo() {
    let repo = TestRepo::new();
    let path = repo.add("feat-global");
    let output = repo
        .bonsai(&repo.dir)
        .args(["cd", "feat-global"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
        path
    );
}

#[test]
fn resume_exact_claude_session_runs_in_its_worktree() {
    let repo = TestRepo::new();
    let worktree = repo.add("feat-resume");
    let key: String = worktree
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let sessions = repo.dir.join(".claude/projects").join(key);
    std::fs::create_dir_all(&sessions).unwrap();
    let id = "5f674c63-05f9-48af-9330-4cdd94b31d15";
    let event = serde_json::json!({
        "type": "user",
        "sessionId": id,
        "cwd": worktree,
        "gitBranch": "feat-resume",
        "message": { "role": "user", "content": "Resume the parser work" }
    });
    std::fs::write(sessions.join(format!("{id}.jsonl")), format!("{event}\n")).unwrap();
    repo.fake_harness("claude");

    repo.bonsai(&worktree)
        .env("PATH", repo.path_with_fakebin())
        .args(["resume", id])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(repo.dir.join("claude-args.txt"))
            .unwrap()
            .trim(),
        format!("--resume {id}")
    );
    assert_eq!(
        PathBuf::from(
            std::fs::read_to_string(repo.dir.join("claude-cwd.txt"))
                .unwrap()
                .trim()
        ),
        worktree
    );
}

#[test]
fn resume_finds_sessions_in_external_worktrees() {
    let repo = TestRepo::new();
    let worktree = repo.add_external("feat-external-resume");
    let key: String = worktree
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let sessions = repo.dir.join(".claude/projects").join(key);
    std::fs::create_dir_all(&sessions).unwrap();
    let id = "8edac90c-8cc0-40f3-9229-571dd359bc9b";
    let event = serde_json::json!({
        "type": "user",
        "sessionId": id,
        "cwd": worktree,
        "gitBranch": "feat-external-resume",
        "message": { "role": "user", "content": "Continue external work" }
    });
    std::fs::write(sessions.join(format!("{id}.jsonl")), format!("{event}\n")).unwrap();
    repo.fake_harness("claude");

    repo.bonsai(&worktree)
        .env("PATH", repo.path_with_fakebin())
        .args(["resume", id])
        .assert()
        .success();

    assert_eq!(
        PathBuf::from(
            std::fs::read_to_string(repo.dir.join("claude-cwd.txt"))
                .unwrap()
                .trim()
        ),
        worktree
    );
}

#[test]
fn resume_ignores_sessions_from_other_projects() {
    let repo = TestRepo::new();
    let sessions = repo.dir.join(".claude/projects/other");
    std::fs::create_dir_all(&sessions).unwrap();
    let event = serde_json::json!({
        "type": "user",
        "sessionId": "93a5a661-943d-481c-ac37-90a4ce41773c",
        "cwd": repo.dir.join("unrelated"),
        "message": { "role": "user", "content": "Unrelated work" }
    });
    std::fs::write(sessions.join("unrelated.jsonl"), format!("{event}\n")).unwrap();

    repo.bonsai(&repo.clone)
        .arg("resume")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no sessions found"));
}

#[test]
fn resume_exact_codex_session_runs_in_its_worktree() {
    let repo = TestRepo::new();
    let worktree = repo.add("feat-codex-resume");
    let codex = repo.dir.join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    let database = rusqlite::Connection::open(codex.join("state_5.sqlite")).unwrap();
    let schema = "CREATE TABLE threads (
            id TEXT PRIMARY KEY,
            cwd TEXT NOT NULL,
            name TEXT,
            title TEXT NOT NULL,
            preview TEXT NOT NULL,
            updated_at_ms INTEGER,
            git_branch TEXT,
            source TEXT NOT NULL,
            archived INTEGER NOT NULL
        );";
    database.execute_batch(schema).unwrap();
    // A newer but empty compatible state DB must not hide older sessions.
    rusqlite::Connection::open(codex.join("state_6.sqlite"))
        .unwrap()
        .execute_batch(schema)
        .unwrap();
    let id = "019a06bd-2888-7781-a54d-ad997a836dbe";
    database
        .execute(
            "INSERT INTO threads VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, 'cli', 0)",
            rusqlite::params![
                id,
                worktree.to_string_lossy(),
                "Codex parser work",
                "Latest parser prompt",
                1_788_510_000_000_i64,
                "feat-codex-resume"
            ],
        )
        .unwrap();
    drop(database);
    repo.fake_harness("codex");

    repo.bonsai(&repo.clone)
        .env("PATH", repo.path_with_fakebin())
        .args(["resume", id])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(repo.dir.join("codex-args.txt"))
            .unwrap()
            .trim(),
        format!("resume {id}")
    );
    assert_eq!(
        PathBuf::from(
            std::fs::read_to_string(repo.dir.join("codex-cwd.txt"))
                .unwrap()
                .trim()
        ),
        worktree
    );
}

#[test]
fn resume_exact_opencode_session_runs_in_its_worktree() {
    let repo = TestRepo::new();
    let worktree = repo.add("feat-opencode-resume");
    let opencode = repo.dir.join(".local/share/opencode");
    std::fs::create_dir_all(&opencode).unwrap();
    let database = rusqlite::Connection::open(opencode.join("opencode.db")).unwrap();
    database
        .execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL);
             CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                parent_id TEXT,
                directory TEXT NOT NULL,
                title TEXT NOT NULL,
                time_updated INTEGER NOT NULL,
                time_archived INTEGER
             );",
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO project VALUES ('project', ?1)",
            rusqlite::params![repo.clone.to_string_lossy()],
        )
        .unwrap();
    let id = "ses_53a1e3d19ffeQcBwf5QsSBgLpo";
    database
        .execute(
            "INSERT INTO session VALUES (?1, 'project', NULL, ?2, ?3, ?4, NULL)",
            rusqlite::params![
                id,
                worktree.to_string_lossy(),
                "OpenCode renderer work",
                1_788_520_000_000_i64
            ],
        )
        .unwrap();
    drop(database);
    repo.fake_harness("opencode");

    repo.bonsai(&repo.clone)
        .env("PATH", repo.path_with_fakebin())
        .args(["resume", id])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(repo.dir.join("opencode-args.txt"))
            .unwrap()
            .trim(),
        format!("--session {id}")
    );
    assert_eq!(
        PathBuf::from(
            std::fs::read_to_string(repo.dir.join("opencode-cwd.txt"))
                .unwrap()
                .trim()
        ),
        worktree
    );
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
    let target = last.strip_prefix(SENTINEL).expect("cd sentinel expected");
    assert_eq!(PathBuf::from(target), repo.clone);
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
fn clean_force_is_an_alias_of_yes() {
    let repo = TestRepo::new();
    // A no-commit branch counts as merged, so -f must remove it unprompted.
    let path = repo.add("feat-forced");
    repo.bonsai(&repo.clone)
        .args(["clean", "-f"])
        .assert()
        .success();
    assert!(!path.exists());

    let dirty = repo.add("feat-wip");
    std::fs::write(dirty.join("wip.txt"), "wip\n").unwrap();
    repo.bonsai(&repo.clone)
        .args(["clean", "--force"])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipping 'feat-wip'"));
    assert!(dirty.exists(), "--force must not touch dirty worktrees");
}

#[test]
fn prune_force_is_an_alias_of_yes() {
    let repo = TestRepo::new();
    let path = repo.add("feat-gone");
    std::fs::remove_dir_all(&path).unwrap();
    repo.bonsai(&repo.clone)
        .args(["prune", "-f"])
        .assert()
        .success();
    assert!(!repo.worktree_list().contains("feat-gone"));
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
        format!("root = '{}'\n", alt_root.display()),
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
    // Hooks run via `sh -c` on unix and `cmd /C` on windows.
    let hook = if cfg!(windows) {
        "type nul > hook-ran"
    } else {
        "touch hook-ran"
    };
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        format!("[add]\ncopy = [\".env\"]\npost_add = \"{hook}\"\n"),
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
fn add_installs_with_pnpm_when_lockfile_present() {
    let repo = TestRepo::new();
    repo.commit_files(&[("package.json", "{}\n"), ("pnpm-lock.yaml", "")]);
    repo.fake_pm("pnpm");
    let (path, stderr) = repo.add_with_path("feat-pnpm", &repo.path_with_fakebin());
    let args = std::fs::read_to_string(path.join("pnpm-args.txt")).unwrap();
    assert_eq!(args.trim(), "install --frozen-lockfile --prefer-offline");
    assert!(stderr.contains("warning: pnpm is using a per-worktree virtual store"));
    assert!(stderr.contains("https://pnpm.io/git-worktrees"));
    assert!(!stderr.contains("\x1b["), "captured stderr must be plain");
}

#[test]
fn add_suppresses_pnpm_warning_when_global_store_is_configured() {
    let repo = TestRepo::new();
    repo.commit_files(&[
        ("package.json", "{}\n"),
        ("pnpm-lock.yaml", ""),
        ("pnpm-workspace.yaml", "virtualStoreType: global\n"),
    ]);
    repo.fake_pm("pnpm");
    let (path, stderr) = repo.add_with_path("feat-pnpm-global", &repo.path_with_fakebin());
    assert!(path.join("pnpm-args.txt").is_file());
    assert!(!stderr.contains("pnpm is using a per-worktree virtual store"));
}

#[test]
fn add_install_honors_package_manager_field() {
    let repo = TestRepo::new();
    repo.commit_files(&[
        ("package.json", "{\"packageManager\": \"yarn@4.0.0\"}\n"),
        ("package-lock.json", "{}\n"),
    ]);
    repo.fake_pm("yarn");
    repo.fake_pm("npm");
    let (path, _) = repo.add_with_path("feat-yarn", &repo.path_with_fakebin());
    let args = std::fs::read_to_string(path.join("yarn-args.txt")).unwrap();
    assert_eq!(args.trim(), "install --immutable");
    assert!(!path.join("npm-args.txt").exists());
}

#[test]
fn add_installs_multiple_ecosystems() {
    let repo = TestRepo::new();
    repo.commit_files(&[("Cargo.lock", ""), ("package-lock.json", "{}\n")]);
    repo.fake_pm("cargo");
    repo.fake_pm("npm");
    let (path, _) = repo.add_with_path("feat-multi", &repo.path_with_fakebin());
    let cargo_args = std::fs::read_to_string(path.join("cargo-args.txt")).unwrap();
    assert_eq!(cargo_args.trim(), "fetch --locked");
    let npm_args = std::fs::read_to_string(path.join("npm-args.txt")).unwrap();
    assert_eq!(npm_args.trim(), "ci --prefer-offline --no-audit --no-fund");
}

#[test]
fn add_install_disabled_via_config() {
    let repo = TestRepo::new();
    repo.commit_files(&[("pnpm-lock.yaml", "")]);
    repo.fake_pm("pnpm");
    std::fs::write(repo.clone.join(".bonsai.toml"), "[add]\ninstall = false\n").unwrap();
    let (path, stderr) = repo.add_with_path("feat-noinstall", &repo.path_with_fakebin());
    assert!(!path.join("pnpm-args.txt").exists());
    assert!(stderr.contains("warning: pnpm"));
}

#[test]
fn add_install_skips_silently_when_pm_missing() {
    let repo = TestRepo::new();
    repo.commit_files(&[("pnpm-lock.yaml", "")]);
    // Empty fakebin + only the host dirs containing git: pnpm is absent.
    let (path, stderr) = repo.add_with_path("feat-nopm", &repo.restricted_path());
    assert!(!path.join("pnpm-args.txt").exists());
    assert!(
        !stderr.contains("installing dependencies"),
        "expected silent skip, got: {stderr}"
    );
}

#[test]
fn add_install_failure_is_non_fatal() {
    let repo = TestRepo::new();
    repo.commit_files(&[("pnpm-lock.yaml", "")]);
    repo.fake_pm_with_exit("pnpm", 1);
    let (path, stderr) = repo.add_with_path("feat-installfail", &repo.path_with_fakebin());
    assert!(path.is_dir());
    assert!(
        stderr.contains("failed"),
        "missing failure notice: {stderr}"
    );
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
        let generated = String::from_utf8_lossy(&output.stdout);
        assert!(generated.contains("resume"));
        assert!(generated.contains("command bonsai"));
        assert!(
            generated.matches("_BONSAI_WRAPPER_VERSION=").count() >= 2,
            "{shell} wrapper must stamp captured and direct-resume invocations"
        );
        assert!(
            generated.matches("_BONSAI_WRAPPER_ACTIVE=1").count() >= 2,
            "{shell} wrapper must identify captured and direct-resume invocations"
        );
        assert!(
            generated.contains(&format!("_BONSAI_WRAPPER_SHELL='{shell}'")),
            "{shell} wrapper must identify its shell"
        );
        assert!(
            generated.contains(&format!("'{}'", env!("CARGO_PKG_VERSION"))),
            "{shell} wrapper must carry the generating binary version"
        );
        let script = repo.dir.join(format!("init.{shell}"));
        std::fs::write(&script, &output.stdout).unwrap();
        let Ok(check) = StdCommand::new(shell_path)
            .args(&check_args)
            .arg(&script)
            .output()
        else {
            eprintln!("skipping {shell}: cannot execute");
            continue;
        };
        assert!(
            check.status.success(),
            "{shell} rejected init script: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }
}

#[test]
fn stale_shell_integration_warns_with_shell_specific_refresh_command() {
    let repo = TestRepo::new();
    for (shell, refresh) in [
        ("zsh", "eval \"$(bonsai init zsh)\""),
        ("bash", "eval \"$(bonsai init bash)\""),
        ("fish", "bonsai init fish | source"),
    ] {
        repo.bonsai(&repo.clone)
            .env("_BONSAI_WRAPPED", "1")
            .env("_BONSAI_WRAPPER_VERSION", "0.0.0-stale")
            .env("_BONSAI_WRAPPER_SHELL", shell)
            .arg("list")
            .assert()
            .success()
            .stderr(
                predicate::str::contains("shell integration is out of sync")
                    .and(predicate::str::contains(refresh))
                    .and(predicate::str::contains("restart your shell")),
            );
    }

    repo.bonsai(&repo.clone)
        .env_remove("_BONSAI_WRAPPED")
        .env("_BONSAI_WRAPPER_ACTIVE", "1")
        .env("_BONSAI_WRAPPER_VERSION", "0.0.0-stale")
        .env("_BONSAI_WRAPPER_SHELL", "zsh")
        .arg("list")
        .assert()
        .success()
        .stderr(
            predicate::str::contains("shell integration is out of sync")
                .and(predicate::str::contains("eval \"$(bonsai init zsh)\"")),
        );
}

#[test]
fn pre_stamp_shell_integration_is_detected_from_the_shell_environment() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.clone)
        .env("_BONSAI_WRAPPED", "1")
        .env_remove("_BONSAI_WRAPPER_VERSION")
        .env_remove("_BONSAI_WRAPPER_SHELL")
        .env("SHELL", "/bin/zsh")
        .arg("list")
        .assert()
        .success()
        .stderr(
            predicate::str::contains("shell integration is out of sync")
                .and(predicate::str::contains("eval \"$(bonsai init zsh)\"")),
        );
}

#[test]
fn current_shell_integration_and_direct_binary_use_stay_quiet() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.clone)
        .env("_BONSAI_WRAPPED", "1")
        .env("_BONSAI_WRAPPER_VERSION", env!("CARGO_PKG_VERSION"))
        .env("_BONSAI_WRAPPER_SHELL", "zsh")
        .arg("list")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    repo.bonsai(&repo.clone)
        .env("_BONSAI_WRAPPER_VERSION", "0.0.0-stale")
        .env("_BONSAI_WRAPPER_SHELL", "zsh")
        .arg("list")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

#[cfg(unix)]
#[test]
fn shell_wrappers_leave_resume_stdio_uncaptured_with_global_options() {
    let repo = TestRepo::new();
    repo.fake_harness("bonsai");
    for shell in ["zsh", "bash", "fish"] {
        let Ok(shell_path) = which(shell) else {
            eprintln!("skipping {shell}: not installed");
            continue;
        };
        let generated = repo
            .bonsai(&repo.dir)
            .args(["init", shell])
            .output()
            .unwrap();
        assert!(generated.status.success());
        let script = repo.dir.join(format!("resume-wrapper.{shell}"));
        std::fs::write(&script, generated.stdout).unwrap();
        let mut command = StdCommand::new(shell_path);
        command
            .envs(repo.env_vars())
            .env("PATH", repo.path_with_fakebin())
            .current_dir(&repo.dir);
        if shell == "fish" {
            command.args([
                "-c",
                "source \"$argv[1]\"; bonsai --root \"$argv[2]\" resume session-id",
                script.to_str().unwrap(),
                repo.root.to_str().unwrap(),
            ]);
        } else {
            command.args([
                "-c",
                "source \"$1\"; bonsai --root \"$2\" resume session-id",
                "_",
                script.to_str().unwrap(),
                repo.root.to_str().unwrap(),
            ]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{shell} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(repo.dir.join("bonsai-wrapped.txt"))
                .unwrap()
                .trim(),
            "unset",
            "{shell} wrapper captured resume stdout"
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
        format!("root = '{}'\n", wt_root.display()),
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
        format!("root = '{}'\n", toml_root.display()),
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
#[cfg(unix)]
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
fn default_copy_carries_env_and_harness_config() {
    // No copy config at all: env files and per-user harness config still
    // travel into new worktrees, whatever tool created them.
    let repo = TestRepo::new();
    std::fs::write(repo.clone.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(repo.clone.join("CLAUDE.local.md"), "notes\n").unwrap();
    std::fs::create_dir_all(repo.clone.join(".claude")).unwrap();
    std::fs::write(repo.clone.join(".claude/settings.local.json"), "{}\n").unwrap();
    let path = repo.add("feat-defaultcopy");
    assert!(path.join(".env").exists());
    assert!(path.join("CLAUDE.local.md").exists());
    assert!(path.join(".claude/settings.local.json").exists());
}

#[test]
fn explicit_copy_config_replaces_defaults() {
    let repo = TestRepo::new();
    std::fs::write(repo.clone.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(repo.clone.join("notes.txt"), "n\n").unwrap();
    std::fs::write(
        repo.clone.join(".bonsai.toml"),
        "[add]\ncopy = [\"notes.txt\"]\n",
    )
    .unwrap();
    let path = repo.add("feat-explicitcopy");
    assert!(path.join("notes.txt").exists());
    assert!(!path.join(".env").exists());
}

#[test]
fn list_json_is_machine_readable() {
    let repo = TestRepo::new();
    let path = repo.add("feat-json");
    let output = repo
        .bonsai(&repo.clone)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let entries: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = entries.as_array().unwrap();
    let main = entries.iter().find(|e| e["main"] == true).unwrap();
    assert_eq!(main["branch"], "main");
    let feat = entries.iter().find(|e| e["branch"] == "feat-json").unwrap();
    assert_eq!(feat["main"], false);
    assert_eq!(PathBuf::from(feat["path"].as_str().unwrap()), path);
    let repo_id = feat["repo"].as_str().unwrap();
    assert!(repo_id.starts_with("local/clone-"), "repo: {repo_id}");

    // --all carries the same repo id (derived from the path layout), so UIs
    // can group worktrees by repository.
    let output = repo
        .bonsai(&repo.dir)
        .args(["list", "--all", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let entries: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let feat = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["branch"] == "feat-json")
        .unwrap();
    assert_eq!(feat["repo"].as_str().unwrap(), repo_id);
}

#[test]
fn clean_json_reports_plan_and_removals() {
    let repo = TestRepo::new();
    let path = repo.add("feat-cleanjson");

    let output = repo
        .bonsai(&repo.clone)
        .args(["clean", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["planned"][0]["branch"], "feat-cleanjson");
    assert_eq!(report["removed"].as_array().unwrap().len(), 0);
    assert!(path.exists());

    let output = repo
        .bonsai(&repo.clone)
        .args(["clean", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["removed"][0], "feat-cleanjson");
    assert!(!path.exists());
}

#[test]
fn skill_prints_and_installs_into_detected_harnesses() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.dir)
        .arg("skill")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("---\nname: bonsai\n"));

    // HOME is the temp dir; only Claude Code is "installed".
    std::fs::create_dir_all(repo.dir.join(".claude")).unwrap();
    repo.bonsai(&repo.dir)
        .args(["skill", "install"])
        .assert()
        .success();
    assert!(repo.dir.join(".claude/skills/bonsai/SKILL.md").exists());
    assert!(!repo.dir.join(".codex").exists());

    // --all installs everywhere, detected or not.
    repo.bonsai(&repo.dir)
        .args(["skill", "install", "--all"])
        .assert()
        .success();
    assert!(repo.dir.join(".codex/skills/bonsai/SKILL.md").exists());
    assert!(
        repo.dir
            .join(".config/opencode/skills/bonsai/SKILL.md")
            .exists()
    );
    assert!(repo.dir.join(".agents/skills/bonsai/SKILL.md").exists());
}

#[test]
fn workspace_file_tracks_worktrees() {
    let repo = TestRepo::new();
    let a = repo.add("feat-ws-a");
    repo.add("feature/ws-b");

    let output = repo.bonsai(&repo.clone).arg("workspace").output().unwrap();
    assert!(output.status.success());
    let file = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    assert!(file.extension().is_some_and(|e| e == "code-workspace"));
    let ws: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let folders = ws["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 3); // main + 2 worktrees
    let names: Vec<&str> = folders
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "main (root)", "names: {names:?}");
    assert!(names.contains(&"feat-ws-a"));
    assert!(names.contains(&"feature/ws-b"));

    // remove keeps the file in sync, and deletes it with the last worktree.
    repo.bonsai(&repo.clone)
        .args(["remove", "feature/ws-b"])
        .assert()
        .success();
    let ws: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(ws["folders"].as_array().unwrap().len(), 2);
    repo.bonsai(&repo.clone)
        .args(["remove", "feat-ws-a"])
        .assert()
        .success();
    assert!(!file.exists());
    assert!(a.parent().is_none_or(|p| !p.exists()));
}

#[test]
fn global_workspace_file_spans_repos_and_updates() {
    let repo = TestRepo::new();
    repo.add("feat-g1");
    repo.add("feature/g2");

    // `workspace --all` works even outside any repo.
    let output = repo
        .bonsai(&repo.dir)
        .args(["workspace", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let file = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    assert_eq!(file, repo.root.join("bonsai.code-workspace"));
    let ws: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let folders = ws["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 2);
    let names: Vec<&str> = folders
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("\u{b7} feat-g1")),
        "names: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with("\u{b7} feature/g2")),
        "names: {names:?}"
    );
    // Relative folder paths resolve against the file's directory.
    for folder in folders {
        let rel = folder["path"].as_str().unwrap();
        assert!(repo.root.join(rel).is_dir(), "missing: {rel}");
    }

    // Kept in sync by mutations; deleted with the last worktree.
    repo.bonsai(&repo.clone)
        .args(["remove", "feature/g2"])
        .assert()
        .success();
    let ws: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(ws["folders"].as_array().unwrap().len(), 1);
    repo.bonsai(&repo.clone)
        .args(["remove", "feat-g1"])
        .assert()
        .success();
    assert!(!file.exists());
}

#[test]
fn workspace_file_can_be_disabled() {
    let repo = TestRepo::new();
    let path = repo
        .bonsai(&repo.clone)
        .env("BONSAI_WORKSPACE", "false")
        .args(["add", "feat-nows"])
        .output()
        .unwrap();
    assert!(path.status.success());
    let path = PathBuf::from(String::from_utf8_lossy(&path.stdout).trim());
    let has_workspace_file = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|e| e.path().extension().is_some_and(|x| x == "code-workspace"));
    assert!(!has_workspace_file);
}

#[test]
fn agents_prints_usage_contract() {
    let repo = TestRepo::new();
    repo.bonsai(&repo.dir)
        .arg("agents")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("bonsai add")
                .and(predicate::str::contains("bonsai clean --yes")),
        );
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

/// Canonicalize without Windows verbatim prefixes (`\\?\C:\...`), which git
/// neither prints nor accepts.
fn canon(p: &Path) -> PathBuf {
    let c = p.canonicalize().unwrap();
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC")
        {
            return PathBuf::from(rest);
        }
    }
    c
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
