use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jsonc_parser::{
    cst::{CstInputValue, CstRootNode},
    parse_to_serde_value,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::parallel;
use crate::repo::{Repo, WorktreeKind};
use crate::worktree::{cleanup_empty_dirs, current_branch, find_worktree_dirs, repo_id_of};

/// Multi-root VS Code workspace file understood by VS Code, Cursor,
/// Windsurf, and other derivatives: one entry for the main checkout plus one
/// per worktree, labelled by branch. Kept in sync by add/remove/clean/prune
/// so `code "$(bonsai workspace)"` always shows the current worktrees.
pub fn file_path(repo: &Repo, config: &Config) -> PathBuf {
    let dir = repo.bonsai_dir(config);
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "bonsai".to_string());
    dir.join(format!("{name}.code-workspace"))
}

/// Refresh managed folders while preserving editor-owned JSONC. Empty files
/// are removed only when their contents still match the generated format.
pub fn sync(repo: &Repo, config: &Config) -> Result<PathBuf> {
    let dir = repo.bonsai_dir(config);
    let file = file_path(repo, config);
    let _lock = lock_workspace(&file, config)?;
    let project_worktrees = repo.project_worktrees(config)?;
    let main_name = project_worktrees
        .iter()
        .find(|entry| entry.kind == WorktreeKind::Main)
        .map(|entry| entry.label())
        .unwrap_or_else(|| "(detached) (root)".to_string());
    let mut worktrees = project_worktrees
        .into_iter()
        .filter(|entry| entry.kind != WorktreeKind::Main && !entry.worktree.is_bare)
        .collect::<Vec<_>>();

    if worktrees.is_empty() {
        update_file(&file, &[], config)?;
        return Ok(file);
    }

    worktrees.sort_by(|a, b| a.worktree.branch.cmp(&b.worktree.branch));
    let mut folders = vec![json!({
        "name": main_name,
        "path": repo.main_root,
    })];
    for entry in &worktrees {
        let wt = &entry.worktree;
        // Paths relative to the workspace file where possible.
        let path = wt
            .path
            .strip_prefix(&dir)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| wt.path.clone());
        folders.push(json!({
            "name": entry.label(),
            "path": path,
        }));
    }

    update_file(&file, &folders, config)?;
    Ok(file)
}

/// The global workspace file: every worktree of every repo under the bonsai
/// root, at `<root>/bonsai.code-workspace`.
pub fn global_file_path(config: &Config) -> PathBuf {
    config.root_dir().join("bonsai.code-workspace")
}

/// Rewrite the global workspace file from the on-disk layout (no repo
/// context needed), or delete it when no worktrees remain anywhere.
pub fn sync_global(config: &Config) -> Result<PathBuf> {
    let root = config.root_dir();
    let file = global_file_path(config);
    let _lock = lock_workspace(&file, config)?;
    let dirs = find_worktree_dirs(&root);

    if dirs.is_empty() {
        update_file(&file, &[], config)?;
        return Ok(file);
    }

    // (repo-id, branch label, relative path), sorted for a stable file.
    let mut entries: Vec<(String, String, PathBuf)> = parallel::map_ordered(&dirs, |path| {
        let branch = current_branch(path);
        let repo_id =
            repo_id_of(&root, path, branch.as_deref()).unwrap_or_else(|| "(unknown)".to_string());
        let rel = path
            .strip_prefix(&root)
            .map(|relative| relative.to_path_buf())
            .unwrap_or_else(|_| path.clone());
        (
            repo_id,
            branch.unwrap_or_else(|| "(detached)".to_string()),
            rel,
        )
    });
    entries.sort();

    // Label with the repo's short name, falling back to the full repo-id
    // when two repos share one.
    let short = |id: &str| id.rsplit('/').next().unwrap_or(id).to_string();
    let mut name_owners = std::collections::HashMap::new();
    for (id, _, _) in &entries {
        name_owners
            .entry(short(id))
            .or_insert_with(std::collections::HashSet::new)
            .insert(id.clone());
    }
    let folders: Vec<_> = entries
        .iter()
        .map(|(id, branch, rel)| {
            let repo_label = if name_owners[&short(id)].len() > 1 {
                id.clone()
            } else {
                short(id)
            };
            json!({"name": format!("{repo_label} \u{00b7} {branch}"), "path": rel})
        })
        .collect();

    update_file(&file, &folders, config)?;
    Ok(file)
}

fn lock_workspace(file: &Path, config: &Config) -> Result<std::fs::File> {
    let root = config.root_dir();
    let locks = root.join(".locks");
    crate::paths::ensure_contained(&locks, &root)?;
    std::fs::create_dir_all(&locks)?;
    crate::paths::ensure_contained(&locks, &root)?;
    let lock_path = workspace_state_path(file, config, "lock");
    let lock = crate::paths::open_lock_file(&lock_path)?;
    lock.lock().context("could not lock workspace file")?;
    Ok(lock)
}

fn workspace_state_path(file: &Path, config: &Config, extension: &str) -> PathBuf {
    let identity = crate::paths::canonicalize_lenient(file);
    let digest = Sha256::digest(identity.as_os_str().as_encoded_bytes());
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    config
        .root_dir()
        .join(".locks")
        .join(format!("workspace-{digest}.{extension}"))
}

fn read_optional(file: &Path) -> Result<Option<String>> {
    reject_symlink(file)?;
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("could not read {}", file.display())),
    }
}

fn record_ownership(file: &Path, folders: &[Value], config: &Config) -> Result<()> {
    let state = workspace_state_path(file, config, "folders.json");
    crate::paths::ensure_contained(&state, &config.root_dir())?;
    let original = read_optional(&state)?;
    let next = format!("{}\n", serde_json::to_string(folders)?);
    if original.as_deref() != Some(next.as_str()) {
        replace_file(&state, &next, original.as_deref(), config)?;
    }
    Ok(())
}

fn reject_symlink(file: &Path) -> Result<()> {
    match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing symlink workspace path: {}", file.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn update_file(file: &Path, folders: &[Value], config: &Config) -> Result<()> {
    let dir = file.parent().context("workspace path has no parent")?;
    crate::paths::ensure_contained(file, &config.root_dir())?;
    let original = read_optional(file)?;
    if folders.is_empty() && original.is_none() {
        return Ok(());
    }
    let state = workspace_state_path(file, config, "folders.json");
    crate::paths::ensure_contained(&state, &config.root_dir())?;
    let ownership = read_optional(&state)?;
    let previous: Vec<Value> = ownership
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("invalid workspace ownership record; existing file preserved")?
        .unwrap_or_default();
    if previous.iter().any(|entry| {
        !entry.as_object().is_some_and(|entry| {
            entry.len() == 2
                && entry.get("path").is_some_and(Value::is_string)
                && entry.get("name").is_some_and(Value::is_string)
        })
    }) {
        bail!("invalid workspace ownership record; existing file preserved");
    }
    let next = match &original {
        Some(text) => merge_workspace(text, folders, &previous)?,
        None => format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({"folders": folders}))?
        ),
    };
    if folders.is_empty()
        && !previous.is_empty()
        && original
            .as_deref()
            .is_some_and(|text| is_generated_document(text, &previous))
    {
        crate::paths::ensure_contained(file, &config.root_dir())?;
        reject_symlink(file)?;
        if Some(std::fs::read_to_string(file)?) != original {
            bail!(
                "workspace changed during refresh; retry: {}",
                file.display()
            );
        }
        std::fs::remove_file(file)?;
        cleanup_empty_dirs(dir, &config.root_dir());
        record_ownership(file, &[], config)?;
        return Ok(());
    }
    if original.as_deref() != Some(next.as_str()) {
        replace_file(file, &next, original.as_deref(), config)?;
    }
    // Record only after the workspace replacement succeeds. A crash between
    // these writes can leave an unclaimed folder, never false ownership.
    record_ownership(file, folders, config)
}

fn replace_file(file: &Path, next: &str, original: Option<&str>, config: &Config) -> Result<()> {
    let dir = file.parent().context("workspace path has no parent")?;
    crate::paths::ensure_contained(file, &config.root_dir())?;
    std::fs::create_dir_all(dir)?;
    crate::paths::ensure_contained(file, &config.root_dir())?;
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    if let Ok(metadata) = std::fs::metadata(file) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.write_all(next.as_bytes())?;
    temporary.as_file().sync_all()?;
    crate::paths::ensure_contained(file, &config.root_dir())?;
    reject_symlink(file)?;
    // Editors do not honor our lock. Avoid replacing a file they changed while
    // the inventory was being merged.
    let current = read_optional(file)?;
    if current.as_deref() != original {
        bail!(
            "workspace changed during refresh; retry: {}",
            file.display()
        );
    }
    temporary
        .persist(file)
        .with_context(|| format!("could not replace {}", file.display()))?;
    Ok(())
}

fn owned_folder(folder: &Value, previous: &[Value]) -> bool {
    previous.iter().any(|entry| {
        entry.get("path") == folder.get("path") && entry.get("name") == folder.get("name")
    })
}

fn is_generated_document(text: &str, previous: &[Value]) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(folders) = value.get("folders").and_then(Value::as_array) else {
        return false;
    };
    object.len() == 1
        && folders.iter().all(|folder| {
            folder.as_object().is_some_and(|entry| {
                entry.len() == 2 && entry.get("name").is_some_and(Value::is_string)
            }) && owned_folder(folder, previous)
        })
        && serde_json::to_string_pretty(&value)
            .is_ok_and(|formatted| text == format!("{formatted}\n"))
}

fn merge_workspace(text: &str, desired: &[Value], previous: &[Value]) -> Result<String> {
    let options = jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    };
    let root = CstRootNode::parse(text, &options)
        .context("invalid workspace JSONC; existing file preserved")?;
    let object = root
        .object_value()
        .context("workspace must be a JSONC object")?;
    let array = object
        .array_value_or_create("folders")
        .context("workspace folders must be an array")?;
    let mut remaining = desired.iter().collect::<Vec<_>>();
    for node in array.elements() {
        let folder: Value = parse_to_serde_value(&node.to_string(), &Default::default())?;
        if let Some(index) = remaining
            .iter()
            .position(|candidate| candidate.get("path") == folder.get("path"))
        {
            let desired = remaining.remove(index);
            if owned_folder(&folder, previous) && folder.get("name") != desired.get("name") {
                let entry = node
                    .as_object()
                    .context("workspace folder must be an object")?;
                let name = CstInputValue::String(
                    desired["name"]
                        .as_str()
                        .context("folder name must be a string")?
                        .into(),
                );
                if let Some(property) = entry.get("name") {
                    property.set_value(name);
                } else {
                    entry.append("name", name);
                }
            }
        } else if owned_folder(&folder, previous)
            && folder
                .as_object()
                .is_some_and(|entry| entry.len() == 2 && entry.contains_key("name"))
        {
            node.remove();
        }
    }
    for folder in remaining {
        array.append(CstInputValue::Object(vec![
            (
                "name".into(),
                CstInputValue::String(
                    folder["name"]
                        .as_str()
                        .context("folder name must be a string")?
                        .into(),
                ),
            ),
            (
                "path".into(),
                CstInputValue::String(
                    folder["path"]
                        .as_str()
                        .context("folder path must be a string")?
                        .into(),
                ),
            ),
        ]));
    }
    Ok(root.to_string())
}

/// Best-effort sync after a mutating command; failure to write an editor
/// convenience file must never fail the command itself.
pub fn sync_quietly(repo: &Repo, config: &Config) {
    if !config.workspace {
        return;
    }
    let targets = ["project", "global"];
    let results = parallel::map_ordered(&targets, |target| match *target {
        "project" => sync(repo, config),
        _ => sync_global(config),
    });
    for (target, result) in targets.iter().zip(results) {
        if let Err(error) = result {
            eprintln!("bonsai: [workspace:{target}] could not update: {error:#}");
        }
    }
}

/// Global variant for commands without a repo context (prune --all).
pub fn sync_global_quietly(config: &Config) {
    if !config.workspace {
        return;
    }
    if let Err(e) = sync_global(config) {
        eprintln!("bonsai: could not update global workspace file: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_jsonc_editor_settings_comments_and_custom_folders() {
        let original = "{\n  // editor preferences\n  \"folders\": [\n    {\"path\": \"ab/old\", \"name\": \"old\"},\n    {\"path\": \"/external\", \"name\": \"notes\"},\n  ],\n  \"settings\": {\"editor.fontSize\": 15,},\n  \"tasks\": {\"version\": \"2.0.0\"},\n}\n";
        let merged = merge_workspace(
            original,
            &[json!({"path": "ab/new", "name": "new"})],
            &[json!({"name":"old","path":"ab/old"})],
        )
        .unwrap();
        assert!(merged.contains("// editor preferences"));
        assert!(merged.contains("\"settings\": {\"editor.fontSize\": 15,}"));
        assert!(merged.contains("\"tasks\": {\"version\": \"2.0.0\"}"));
        assert!(merged.contains("/external"));
        assert!(!merged.contains("ab/old"));
        assert!(merged.contains("ab/new"));
    }

    #[test]
    fn merge_is_byte_identical_when_inventory_is_unchanged() {
        let original = "{\"folders\": [/* keep */ {\"path\":\"ab/current\",\"name\":\"current\",\"custom\":true}], \"settings\":{}}";
        let folders = [json!({"path": "ab/current", "name": "current"})];
        assert_eq!(
            merge_workspace(original, &folders, &folders).unwrap(),
            original
        );
    }

    #[test]
    fn merge_retains_comments_next_to_removed_folders() {
        let original = "{\"folders\": [\n// team notes\n{\"path\":\"ab/old\",\"name\":\"old\"}\n]}";
        let merged =
            merge_workspace(original, &[], &[json!({"name":"old","path":"ab/old"})]).unwrap();
        assert!(merged.contains("// team notes"), "{merged}");
    }

    #[test]
    fn refresh_preserves_custom_folders_inside_the_managed_root() {
        let (_temporary, config, file) = fixture();
        let managed = json!({"name":"branch", "path":"ab/branch"});
        update_file(&file, &[managed], &config).unwrap();
        let original = "{\"folders\":[{\"name\":\"branch\",\"path\":\"ab/branch\"},{\"name\":\"source\",\"path\":\"ab/branch/src\"},{\"name\":\"notes\",\"path\":\"notes\"}]}";
        std::fs::write(&file, original).unwrap();
        update_file(&file, &[], &config).unwrap();
        let value: Value =
            parse_to_serde_value(&std::fs::read_to_string(file).unwrap(), &Default::default())
                .unwrap();
        assert_eq!(
            value["folders"],
            json!([{"name":"source","path":"ab/branch/src"},{"name":"notes","path":"notes"}])
        );
    }

    #[test]
    fn migration_preserves_unknown_folders_and_custom_labels() {
        let (_temporary, config, file) = fixture();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let original = "{\"folders\":[{\"name\":\"my task\",\"path\":\"ab/branch\"},{\"name\":\"source\",\"path\":\"ab/branch/src\"},{\"name\":\"old\",\"path\":\"ab/old\"}]}";
        std::fs::write(&file, original).unwrap();
        update_file(
            &file,
            &[json!({"name":"branch", "path":"ab/branch"})],
            &config,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        update_file(&file, &[], &config).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    }

    #[test]
    fn user_renamed_generated_folder_survives_refresh_and_removal() {
        let (_temporary, config, file) = fixture();
        let desired = [json!({"name":"branch", "path":"ab/branch"})];
        update_file(&file, &desired, &config).unwrap();
        let original = "{\"folders\":[{\"name\":\"my task\",\"path\":\"ab/branch\"}]}";
        std::fs::write(&file, original).unwrap();
        update_file(&file, &desired, &config).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        update_file(&file, &[], &config).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    }

    #[test]
    fn generated_labels_update_only_when_the_old_label_is_still_owned() {
        let original = "{\"folders\":[{\"name\":\"old\",\"path\":\"ab/branch\"}]}";
        let merged = merge_workspace(
            original,
            &[json!({"name":"new", "path":"ab/branch"})],
            &[json!({"name":"old", "path":"ab/branch"})],
        )
        .unwrap();
        assert!(merged.contains("\"new\""));
        assert!(!merged.contains("\"old\""));
    }

    #[test]
    fn corrupt_ownership_record_preserves_the_workspace() {
        let (_temporary, config, file) = fixture();
        update_file(
            &file,
            &[json!({"name":"branch", "path":"ab/branch"})],
            &config,
        )
        .unwrap();
        let original = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            workspace_state_path(&file, &config, "folders.json"),
            "broken",
        )
        .unwrap();
        assert!(update_file(&file, &[], &config).is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), original);
    }

    #[test]
    fn malformed_workspace_is_rejected() {
        assert!(merge_workspace("{ invalid", &[], &[]).is_err());
        assert!(merge_workspace("{\"folders\": false}", &[], &[]).is_err());
    }

    fn fixture() -> (tempfile::TempDir, Config, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let config = Config {
            root: temporary.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let file = temporary
            .path()
            .join("project")
            .join("project.code-workspace");
        (temporary, config, file)
    }

    #[test]
    fn unchanged_refresh_preserves_mtime_and_empty_customized_workspace() {
        let (_temporary, config, file) = fixture();
        let dir = file.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let original = "{\"folders\":[{\"path\":\"ab/one\",\"name\":\"one\"}],\"settings\":{\"editor.fontSize\":15}}";
        std::fs::write(&file, original).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::open(&file)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        update_file(&file, &[json!({"path":"ab/one","name":"one"})], &config).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), old);
        update_file(&file, &[], &config).unwrap();
        let value: Value = parse_to_serde_value(
            &std::fs::read_to_string(&file).unwrap(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(value["folders"], json!([]));
        assert_eq!(value["settings"]["editor.fontSize"], 15);
    }

    #[test]
    fn generated_empty_workspace_is_removed_and_invalid_workspace_is_untouched() {
        let (_temporary, config, file) = fixture();
        let dir = file.parent().unwrap();
        update_file(&file, &[json!({"path":"ab/one","name":"one"})], &config).unwrap();
        update_file(&file, &[], &config).unwrap();
        assert!(!file.exists());
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&file, "{ broken").unwrap();
        assert!(update_file(&file, &[], &config).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ broken");
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_refreshes_replace_complete_documents_and_preserve_editor_content() {
        let (_temporary, config, file) = fixture();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{\"folders\":[],\"settings\":{\"keep\":true}}").unwrap();
        std::thread::scope(|scope| {
            for index in 0..8 {
                let config = &config;
                let file = &file;
                scope.spawn(move || {
                    let _lock = lock_workspace(file, config).unwrap();
                    update_file(
                        file,
                        &[json!({"path":format!("ab/{index}"),"name":index.to_string()})],
                        config,
                    )
                    .unwrap();
                });
            }
            for _ in 0..100 {
                let value: Value = parse_to_serde_value(
                    &std::fs::read_to_string(&file).unwrap(),
                    &Default::default(),
                )
                .unwrap();
                assert_eq!(value["settings"]["keep"], true);
            }
        });
        let value: Value = parse_to_serde_value(
            &std::fs::read_to_string(&file).unwrap(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(value["folders"].as_array().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    #[cfg(unix)]
    fn workspace_symlink_never_replaces_its_target() {
        let (temporary, config, file) = fixture();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let target = temporary.path().join("editor.json");
        std::fs::write(&target, "{\"folders\":[]}").unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        assert!(update_file(&file, &[json!({"path":"ab/one","name":"one"})], &config).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "{\"folders\":[]}");
    }
}
