use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::parallel;
use crate::picker;
use crate::repo::{Repo, WorktreeKind, dir_collides, validate_branch_name};
use crate::worktree::path_for_branch;

pub fn run(
    config: &Config,
    repo: &Repo,
    branch: Option<String>,
    base: Option<String>,
    fetch: bool,
    path_override: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    let fetch_enabled = fetch || config.add.fetch;
    let mut fetched = fetch_enabled && branch.is_none();
    if fetched {
        fetch_remote(repo, config)?;
    }

    let raw_branch = match branch {
        Some(branch) => branch,
        None => picker::text_with_suggestions(
            "Branch:",
            "pick an existing branch or type a new name to create it",
            branch_suggestions(repo, config)?,
        )?,
    };
    let _root_lock = crate::paths::lock_root(&config.root_dir(), false)?;
    let _mutation_lock = repo.lock_mutations()?;
    let mut branch = resolve_branch_input(repo, config, &raw_branch)?;
    // A newly published exact ref takes precedence over reusing a slugified
    // name, even when that normalized branch already has a worktree.
    if fetch_enabled && !fetched && branch != raw_branch {
        fetch_remote(repo, config)?;
        fetched = true;
        branch = resolve_branch_input(repo, config, &raw_branch)?;
    }
    validate_branch_name(&repo.git, &branch)?;

    let project_worktrees = repo.project_worktrees(config)?;
    let worktrees = project_worktrees
        .iter()
        .map(|entry| entry.worktree.clone())
        .collect::<Vec<_>>();
    let bonsai_dir = crate::paths::ensure_contained(&repo.bonsai_dir(config), &config.root_dir())?;

    // Idempotent: adding a branch that already has a Bonsai worktree just cds
    // there. A checkout anywhere else is read-only to Bonsai: `add` never
    // adopts, moves, or creates worktrees outside the configured root.
    if let Some(entry) = project_worktrees
        .iter()
        .find(|entry| entry.worktree.branch.as_deref() == Some(branch.as_str()))
    {
        let wt = &entry.worktree;
        if entry.kind == WorktreeKind::Managed {
            eprintln!(
                "bonsai: '{branch}' already has a worktree at {}",
                wt.path.display()
            );
            return Ok(Some(wt.path.clone()));
        }
        bail!(
            "'{branch}' is checked out at {}; switch branches there or pick another name",
            wt.path.display()
        );
    }

    if fetch_enabled && !fetched {
        fetch_remote(repo, config)?;
        branch = resolve_branch_input(repo, config, &raw_branch)?;
    }
    if branch != raw_branch {
        eprintln!("bonsai: using branch '{branch}' for '{raw_branch}'");
    }

    let path = match path_override {
        Some(p) => {
            let path = std::path::absolute(&p).context("invalid --path")?;
            if path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
            {
                bail!("--path must not contain '..'");
            }
            path
        }
        None => path_for_branch(&bonsai_dir, &branch),
    };
    let path = crate::paths::ensure_contained(&path, &bonsai_dir)
        .context("worktree path must stay inside this project's Bonsai directory")?;
    if let Some(other) = dir_collides(&path, &worktrees, &branch) {
        bail!(
            "path {} collides with the worktree for branch '{other}' (case-insensitive filesystem); use --path to pick another location",
            path.display()
        );
    }
    if path.exists() {
        // A leftover from a crash; only an empty dir is safe to reuse.
        if std::fs::remove_dir(&path).is_err() {
            bail!(
                "stale directory {} is in the way; run 'bonsai prune' first",
                path.display()
            );
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::paths::ensure_contained(&path, &config.root_dir())?;
    crate::paths::ensure_contained(&path, &bonsai_dir)?;

    let path_str = path.to_string_lossy().into_owned();
    if repo.git.ok(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ]) {
        repo.git.run(&["worktree", "add", &path_str, &branch])?;
        eprintln!("bonsai: added worktree for '{branch}' at {path_str}");
    } else if let Some(remote_ref) = remote_ref_for(repo, config, &branch)? {
        repo.git.run(&[
            "worktree",
            "add",
            "--track",
            "-b",
            &branch,
            &path_str,
            &remote_ref,
        ])?;
        eprintln!("bonsai: added worktree for '{branch}' (tracking {remote_ref}) at {path_str}");
    } else {
        let (base_display, base_ref) = resolve_base(repo, config, base)?;
        // --no-track: git would otherwise set the upstream to the base
        // (e.g. origin/main), which misleads `git push` and defeats clean's
        // gone-upstream detection once the branch gets its own upstream.
        repo.git.run(&[
            "worktree",
            "add",
            "--no-track",
            "-b",
            &branch,
            &path_str,
            &base_ref,
        ])?;
        eprintln!("bonsai: created branch '{branch}' from {base_display}, worktree at {path_str}");
    }

    copy_files(repo, config, &path);
    warn_package_manager_config(&path);
    let cancellation = crate::process::Cancellation::new()?;
    install_dependencies(config, &path, &cancellation)?;
    run_post_add(config, &branch, &path, &cancellation)?;
    crate::workspace::sync_quietly(repo, config);

    Ok(Some(path))
}

fn resolve_branch_input(repo: &Repo, config: &Config, input: &str) -> Result<String> {
    if validate_branch_name(&repo.git, input).is_ok()
        && (repo.git.ok(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{input}"),
        ]) || remote_ref_for(repo, config, input)?.is_some())
    {
        return Ok(input.to_string());
    }
    slugify_branch_input(input)
}

fn fetch_remote(repo: &Repo, config: &Config) -> Result<()> {
    if let Some(remote) = repo.remote_name(config) {
        repo.git.interactive(&["fetch", "--prune", &remote])?;
    }
    Ok(())
}

/// Turn a task-like branch input into a stable Git/path slug while keeping
/// forward slashes as nested branch and directory delimiters.
fn slugify_branch_input(input: &str) -> Result<String> {
    input
        .split('/')
        .map(|segment| {
            let slug = slug::slugify(segment);
            if slug.is_empty() {
                bail!("branch segment '{segment}' is empty after slugifying '{input}'");
            }
            Ok(slug)
        })
        .collect::<Result<Vec<_>>>()
        .map(|segments| segments.join("/"))
}

/// Branches worth suggesting: locals without a worktree, plus remote branches
/// without a local counterpart.
fn branch_suggestions(repo: &Repo, config: &Config) -> Result<Vec<String>> {
    let checked_out: HashSet<String> = repo
        .worktrees()?
        .into_iter()
        .filter_map(|wt| wt.branch)
        .collect();
    let locals: Vec<String> = repo
        .git
        .out(&["for-each-ref", "--format=%(refname:short)", "refs/heads"])?
        .lines()
        .map(str::to_string)
        .collect();
    let local_set = locals.iter().cloned().collect::<HashSet<_>>();
    let mut suggestions: Vec<String> = locals
        .iter()
        .filter(|branch| !checked_out.contains(branch.as_str()))
        .cloned()
        .collect();
    let mut suggested = suggestions.iter().cloned().collect::<HashSet<_>>();
    if let Some(remote) = repo.remote_name(config) {
        let prefix = format!("{remote}/");
        for r in repo
            .git
            .out(&[
                "for-each-ref",
                "--format=%(refname:short)",
                &format!("refs/remotes/{remote}"),
            ])?
            .lines()
        {
            if let Some(branch) = r.strip_prefix(&prefix)
                && branch != "HEAD"
                && !local_set.contains(branch)
                && suggested.insert(branch.to_string())
            {
                suggestions.push(branch.to_string());
            }
        }
    }
    suggestions.sort();
    Ok(suggestions)
}

/// Explicit remote-branch resolution, no git DWIM: configured remote first,
/// then a unique match across all remotes.
fn remote_ref_for(repo: &Repo, config: &Config, branch: &str) -> Result<Option<String>> {
    if let Some(remote) = repo.remote_name(config)
        && repo.git.ok(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{branch}"),
        ])
    {
        return Ok(Some(format!("{remote}/{branch}")));
    }
    let matches: Vec<String> = repo
        .git
        .out(&[
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/remotes/*/{branch}"),
        ])?
        .lines()
        .map(str::to_string)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap())),
        _ => bail!(
            "branch '{branch}' exists on several remotes ({}); pass --base to disambiguate",
            matches.join(", ")
        ),
    }
}

/// Returns (display name, ref to pass to git).
fn resolve_base(repo: &Repo, config: &Config, base: Option<String>) -> Result<(String, String)> {
    if let Some(base) = base {
        // Resolve in the *current directory's* context: `--base HEAD` from
        // inside a worktree must mean that worktree's HEAD (stacked
        // branches), not the main checkout's.
        let sha = crate::git::Git::new()
            .out(&[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{base}^{{commit}}"),
            ])
            .map_err(|e| anyhow::anyhow!("cannot resolve base ref '{base}': {e}"))?;
        return Ok((base, sha));
    }
    let default = repo.default_branch(config)?;
    if let Some(remote) = repo.remote_name(config)
        && repo.git.ok(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{default}"),
        ])
    {
        let base = format!("{remote}/{default}");
        return Ok((base.clone(), base));
    }
    if repo.git.ok(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{default}"),
    ]) {
        return Ok((default.clone(), default));
    }
    bail!("base ref '{default}' not found; pass --base");
}

/// Copy configured globs (e.g. .env files) into the new worktree, looking in
/// the worktree we are standing in first (freshest local files), then the
/// main worktree. Best-effort. Copied `.envrc` files get `direnv allow`ed —
/// they come from the user's own checkout.
fn copy_files(repo: &Repo, config: &Config, dest: &std::path::Path) {
    if config.add.copy.is_empty() {
        return;
    }
    let mut sources: Vec<PathBuf> = Vec::new();
    if let Some(top) = &repo.current_root {
        sources.push(top.clone());
    }
    if !sources.contains(&repo.main_root) {
        sources.push(repo.main_root.clone());
    }
    let mut copied_envrc: Vec<PathBuf> = Vec::new();
    for pattern in &config.add.copy {
        for source in &sources {
            let full = format!("{}/{pattern}", source.display());
            let Ok(paths) = glob::glob(&full) else {
                eprintln!("bonsai: invalid copy glob '{pattern}', skipping");
                break;
            };
            for path in paths.flatten().filter(|p| p.is_file()) {
                let Ok(rel) = path.strip_prefix(source) else {
                    continue;
                };
                let target = dest.join(rel);
                if let Err(error) = crate::paths::ensure_contained(&target, dest) {
                    eprintln!("bonsai: refusing copy to {}: {error:#}", target.display());
                    continue;
                }
                // Also what makes the current worktree win over the main one.
                if target.exists() {
                    continue;
                }
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::copy(&path, &target) {
                    Ok(_) => {
                        eprintln!("bonsai: copied {}", rel.display());
                        if rel.file_name().is_some_and(|n| n == ".envrc") {
                            copied_envrc.push(target);
                        }
                    }
                    Err(e) => eprintln!("bonsai: failed to copy {}: {e}", rel.display()),
                }
            }
        }
    }
    direnv_allow(&copied_envrc);
}

/// Pre-approve `.envrc` files that bonsai itself copied from the user's own
/// worktree, so the first cd doesn't stop on "direnv: error .envrc is
/// blocked". Tracked `.envrc` files coming from the repo checkout are left
/// for direnv to gate as usual.
fn direnv_allow(envrcs: &[PathBuf]) {
    for envrc in envrcs {
        match std::process::Command::new("direnv")
            .arg("allow")
            .arg(envrc)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {
                eprintln!("bonsai: direnv allowed {}", envrc.display());
            }
            Ok(_) => eprintln!("bonsai: direnv allow failed for {}", envrc.display()),
            Err(_) => return, // direnv not installed
        }
    }
}

fn warn_package_manager_config(path: &std::path::Path) {
    use std::io::IsTerminal;

    let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let label = warning_label(color);
    for warning in crate::pm::worktree_warnings(path) {
        eprintln!("bonsai: {label} {}: {}", warning.message, warning.docs);
    }
}

fn warning_label(color: bool) -> &'static str {
    if color {
        "\x1b[30;43m WARNING \x1b[0m"
    } else {
        "warning:"
    }
}

/// Install dependencies with every package manager detected in the new
/// worktree. Frozen-lockfile flags keep the checkout pristine and each
/// tool's shared store keeps disk usage low. Independent ecosystems run in
/// parallel and finish before `post_add`. Failures preserve the worktree and
/// explain how to finish setup. Live child output stays on stderr.
fn install_dependencies(
    config: &Config,
    path: &std::path::Path,
    cancellation: &crate::process::Cancellation,
) -> Result<()> {
    if !config.add.install {
        return Ok(());
    }
    let jobs = crate::pm::detect(path)
        .into_iter()
        .map(|pm| (pm, crate::pm::find_program_at(pm.program(), path)))
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return Ok(());
    }
    if parallel::worker_count(jobs.len()) > 1 {
        eprintln!(
            "bonsai: installing dependencies in parallel ({} jobs):",
            jobs.len()
        );
    } else {
        eprintln!("bonsai: installing dependencies (1 job):");
    }
    for (pm, _) in &jobs {
        eprintln!(
            "  [{}] {} {}",
            pm.program(),
            pm.program(),
            pm.args().join(" ")
        );
    }

    let results =
        parallel::map_ordered(&jobs, |(pm, program)| -> Result<std::process::ExitStatus> {
            let program = program
                .as_ref()
                .with_context(|| format!("{} is not on PATH", pm.program()))?;
            crate::process::run(
                std::process::Command::new(program)
                    .args(pm.args())
                    .current_dir(path),
                pm.program(),
                cancellation,
            )
        });
    let mut incomplete = false;
    for ((pm, _), result) in jobs.iter().zip(results) {
        let label = pm.program();
        match result {
            Ok(status) => {
                if status.success() {
                    eprintln!("  [{label}] done");
                } else {
                    eprintln!("  [{label}] failed (exit {:?})", status.code());
                    incomplete = true;
                    print_retry(pm, path);
                }
            }
            Err(error) => {
                eprintln!("  [{label}] setup incomplete: {error:#}");
                incomplete = true;
                print_retry(pm, path);
            }
        }
    }
    if incomplete {
        eprintln!(
            "bonsai: worktree created at {}; dependency setup is incomplete",
            path.display()
        );
    }
    cancellation.check()
}

fn print_retry(pm: &crate::pm::PackageManager, path: &std::path::Path) {
    eprintln!(
        "  [{}] after activating the project's tools in {}, run: {} {}",
        pm.program(),
        path.display(),
        pm.program(),
        pm.args().join(" ")
    );
}

/// Run the post_add hook inside the new worktree. Failure is reported but
/// does not undo the add. Hook stdout goes to our stderr so wrapped stdout
/// capture stays clean.
fn run_post_add(
    config: &Config,
    branch: &str,
    path: &std::path::Path,
    cancellation: &crate::process::Cancellation,
) -> Result<()> {
    let Some(hook) = &config.add.post_add else {
        return Ok(());
    };
    eprintln!("bonsai: running post_add hook");
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let result = crate::process::run(
        std::process::Command::new(shell)
            .arg(flag)
            .arg(hook)
            .current_dir(path)
            .env("BONSAI_BRANCH", branch)
            .env("BONSAI_WORKTREE", path),
        "post_add",
        cancellation,
    );
    match result {
        Ok(status) => {
            if !status.success() {
                eprintln!("bonsai: post_add hook failed (exit {:?})", status.code());
                eprintln!(
                    "bonsai: setup incomplete at {}; rerun the configured post_add hook there",
                    path.display()
                );
            }
        }
        Err(e) => eprintln!(
            "bonsai: setup incomplete at {}; post_add failed: {e:#}",
            path.display()
        ),
    }
    cancellation.check()
}

#[cfg(test)]
mod tests {
    use super::{slugify_branch_input, warning_label};

    #[test]
    fn slugifies_branch_segments_without_removing_slashes() {
        let cases = [
            ("Fix Parser", "fix-parser"),
            ("AB/Fix Parser #42", "ab/fix-parser-42"),
            ("feature/login", "feature/login"),
            ("Crème brûlée/Über Fix", "creme-brulee/uber-fix"),
            ("foo/💥", "foo/boom"),
        ];
        for (input, expected) in cases {
            assert_eq!(slugify_branch_input(input).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_empty_slug_segments() {
        for input in ["", "/foo", "foo/", "foo//bar", "foo/---"] {
            assert!(slugify_branch_input(input).is_err(), "input: {input:?}");
        }
    }

    #[test]
    fn warning_label_has_a_yellow_background_when_colored() {
        assert_eq!(warning_label(true), "\x1b[30;43m WARNING \x1b[0m");
        assert_eq!(warning_label(false), "warning:");
    }
}
