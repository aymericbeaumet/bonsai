use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;

const INPUT_LIMIT: u64 = 1024 * 1024;
const PROVIDERS: [&str; 3] = ["codex", "claude", "opencode"];

pub fn store_dir(config: &Config) -> PathBuf {
    std::env::var_os("_BONSAI_HQ_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.root_dir().join(".hq"))
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn key(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid JSON in {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    write_file(path, &serde_json::to_vec_pretty(value)?)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    // Follow an existing configuration symlink without replacing the symlink itself.
    let target = if path.is_symlink() {
        std::fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let parent = target.parent().context("file has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::metadata(&target)
            .map(|metadata| metadata.permissions())
            .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o600));
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(target)?;
    Ok(())
}

pub(crate) fn with_store_lock<T>(store: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::fs::create_dir_all(store)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(store, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock = crate::paths::open_lock_file(&store.join("state.lock"))?;
    lock.lock()?;
    f()
}

pub(crate) fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            }
            #[cfg(not(unix))]
            path.is_file()
        })
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn home_for(provider: &str) -> Result<PathBuf> {
    if let Some(path) = match provider {
        "codex" => std::env::var_os("CODEX_HOME"),
        "claude" => std::env::var_os("CLAUDE_CONFIG_DIR"),
        _ => None,
    } {
        return Ok(PathBuf::from(path));
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?);
    Ok(match provider {
        "codex" => home.join(".codex"),
        "claude" => home.join(".claude"),
        _ => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("opencode"),
    })
}

pub(crate) fn enabled(store: &Path, provider: &str) -> bool {
    read_json(&store.join("integrations.json"))
        .map(|value| value[provider]["enabled"] != false)
        .unwrap_or(false)
}

pub(crate) fn status(store: &Path, provider: &str) -> Value {
    let manifest = read_json(&store.join(format!("{provider}-installation.json")))
        .unwrap_or_else(|_| json!({}));
    let (state, message) = if !enabled(store, provider) {
        ("disabled", "Integration disabled".to_owned())
    } else if executable(provider).is_none() {
        ("unavailable", format!("{provider} is not installed"))
    } else if manifest["installed"] == true {
        ("awaiting-activation", match provider {
            "codex" => "Review Bonsai hooks in Codex /hooks; existing independent sessions remain navigable",
            "opencode" => "Bridge activates when OpenCode next starts",
            _ => "Waiting for a Claude statusline or lifecycle event",
        }.to_owned())
    } else {
        (
            "available",
            "Integration is available to install".to_owned(),
        )
    };
    json!({"provider": provider, "status": if state == "available" {"unavailable"} else {state},
        "message": message, "capabilities": []})
}

pub fn setup(store: &Path, provider: &str, action: &str) -> Result<()> {
    ensure!(PROVIDERS.contains(&provider), "unknown provider");
    ensure!(
        [
            "install",
            "repair",
            "enable",
            "disable",
            "uninstall",
            "connect"
        ]
        .contains(&action),
        "unknown integration action"
    );
    let home = home_for(provider)?;
    let executable = std::env::current_exe()?;
    with_store_lock(store, || {
        let mut preferences = read_json(&store.join("integrations.json"))?;
        if action == "disable" {
            preferences[provider] = json!({"enabled": false});
            return write_json(&store.join("integrations.json"), &preferences);
        }
        if action == "uninstall" {
            uninstall(store, provider)?;
            preferences[provider] = json!({"enabled": false});
        } else {
            install_at(store, provider, &home, &executable, action == "repair")?;
            preferences[provider] = json!({"enabled": true});
        }
        write_json(&store.join("integrations.json"), &preferences)
    })
}

fn install_at(
    store: &Path,
    provider: &str,
    home: &Path,
    executable: &Path,
    repair: bool,
) -> Result<()> {
    let manifest_path = store.join(format!("{provider}-installation.json"));
    let previous = read_json(&manifest_path)?;
    if previous["installed"] == true && !repair {
        return Ok(());
    }
    let command = |kind: &str| {
        format!(
            "_BONSAI_HQ_STORE={} {} __hq-event {} {}",
            shell_word(&store.to_string_lossy()),
            shell_word(&executable.to_string_lossy()),
            provider,
            kind
        )
    };
    if provider == "opencode" {
        let target = home.join("plugins/bonsai-hq.js");
        let content = include_str!("integrations/opencode.js")
            .replace(
                "__BONSAI_EXECUTABLE__",
                &serde_json::to_string(&executable.to_string_lossy())?,
            )
            .replace(
                "__BONSAI_STORE__",
                &serde_json::to_string(&store.to_string_lossy())?,
            );
        if target.exists() {
            let current = std::fs::read_to_string(&target)?;
            ensure!(
                previous["content"].as_str() == Some(&current) || current == content,
                "{} was changed outside Bonsai; preserving it",
                target.display()
            );
        }
        write_file(&target, content.as_bytes())?;
        return write_json(
            &manifest_path,
            &json!({"installed": true, "path": target, "content": content}),
        );
    }
    let target = home.join(if provider == "claude" {
        "settings.json"
    } else {
        "hooks.json"
    });
    let mut settings = read_json(&target)?;
    ensure!(
        settings.is_object(),
        "{} must contain a JSON object",
        target.display()
    );
    let mut installed = Vec::new();
    let events: &[&str] = if provider == "claude" {
        &[
            "SessionStart",
            "SessionEnd",
            "UserPromptSubmit",
            "SubagentStart",
            "SubagentStop",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
            "StopFailure",
            "Notification",
        ]
    } else {
        &[
            "SessionStart",
            "SessionEnd",
            "UserPromptSubmit",
            "SubagentStart",
            "SubagentStop",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
            "Interrupt",
        ]
    };
    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }
    ensure!(settings["hooks"].is_object(), "hooks must be an object");
    for hook in previous["hooks"].as_array().into_iter().flatten() {
        if let Some(event) = hook["event"].as_str()
            && let Some(entries) = settings["hooks"][event].as_array_mut()
        {
            entries.retain(|entry| entry != &hook["entry"]);
        }
    }
    for event in events {
        let entry =
            json!({"hooks": [{"type": "command", "command": command("event"), "timeout": 2}]});
        if settings["hooks"].get(*event).is_none() {
            settings["hooks"][event] = json!([]);
        }
        let entries = settings["hooks"][event]
            .as_array_mut()
            .context("hook event must contain an array")?;
        if !entries.contains(&entry) {
            entries.push(entry.clone());
        }
        installed.push(json!({"event": event, "entry": entry}));
    }
    let mut manifest = json!({"installed": true, "path": target, "hooks": installed});
    if provider == "claude" {
        let wrapper = json!({"type": "command", "command": command("statusline")});
        let original = if previous["statusLine"].is_object()
            && settings["statusLine"] == previous["statusLine"]
        {
            previous["originalStatusLine"].clone()
        } else {
            settings.get("statusLine").cloned().unwrap_or(Value::Null)
        };
        manifest["originalStatusLine"] = original;
        manifest["statusLine"] = wrapper.clone();
        settings["statusLine"] = wrapper;
    }
    // Store rollback information before changing user configuration.
    manifest["installed"] = json!(false);
    write_json(&manifest_path, &manifest)?;
    write_json(&target, &settings)?;
    manifest["installed"] = json!(true);
    write_json(&manifest_path, &manifest)
}

fn uninstall(store: &Path, provider: &str) -> Result<()> {
    let manifest_path = store.join(format!("{provider}-installation.json"));
    let mut manifest = read_json(&manifest_path)?;
    let Some(target) = manifest["path"].as_str().map(PathBuf::from) else {
        return Ok(());
    };
    if provider == "opencode" {
        if target.exists()
            && std::fs::read_to_string(&target).ok().as_deref() == manifest["content"].as_str()
        {
            std::fs::remove_file(&target)?;
        }
    } else {
        let mut settings = read_json(&target)?;
        for hook in manifest["hooks"].as_array().into_iter().flatten() {
            if let Some(event) = hook["event"].as_str()
                && let Some(entries) = settings["hooks"][event].as_array_mut()
            {
                entries.retain(|entry| entry != &hook["entry"]);
            }
        }
        if provider == "claude" && settings["statusLine"] == manifest["statusLine"] {
            if manifest["originalStatusLine"].is_null() {
                settings
                    .as_object_mut()
                    .context("settings must be an object")?
                    .remove("statusLine");
            } else {
                settings["statusLine"] = manifest["originalStatusLine"].clone();
            }
        }
        write_json(&target, &settings)?;
    }
    manifest["installed"] = json!(false);
    write_json(&manifest_path, &manifest)
}

pub fn run_bridge(config: &Config, provider: &str, kind: &str) -> Result<()> {
    ensure!(PROVIDERS.contains(&provider), "unknown provider");
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    let store = store_dir(config);
    process_bridge(&store, provider, kind, &bytes, &mut std::io::stdout())
}

fn process_bridge(
    store: &Path,
    provider: &str,
    kind: &str,
    bytes: &[u8],
    output: &mut impl Write,
) -> Result<()> {
    let recording = || -> Result<Value> {
        ensure!(
            bytes.len() as u64 <= INPUT_LIMIT,
            "HQ event exceeds one MiB"
        );
        let input: Value = serde_json::from_slice(bytes).context("invalid HQ event JSON")?;
        if enabled(store, provider) {
            with_store_lock(store, || ingest(store, provider, kind, &input, now()))
        } else {
            Ok(json!({}))
        }
    };
    let recorded = recording();
    if kind == "statusline" && provider == "claude" {
        // Telemetry failure must never suppress the user's existing statusline.
        let original = read_json(&store.join("claude-installation.json"))?;
        if let Some(command) = original["originalStatusLine"]["command"].as_str() {
            let mut child = Command::new("sh")
                .args(["-c", command])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let result = std::thread::scope(|scope| {
                if let Some(mut stdin) = child.stdin.take() {
                    scope.spawn(move || {
                        let _ = stdin.write_all(bytes);
                    });
                }
                child.wait_with_output()
            })?;
            output.write_all(&result.stdout)?;
            ensure!(
                result.status.success(),
                "original statusline command failed"
            );
        }
    } else if provider == "opencode" {
        writeln!(output, "{}", serde_json::to_string(&recorded?)?)?;
    } else {
        // A valid neutral hook result never changes an agent's decision or context.
        writeln!(output, "{{}}")?;
    }
    Ok(())
}

fn ingest(store: &Path, provider: &str, kind: &str, input: &Value, observed: u64) -> Result<Value> {
    if provider == "opencode" && kind == "snapshot" {
        let runtime = input["runtimeId"].as_str().context("missing runtime ID")?;
        let sessions = input["sessions"].as_array().context("missing sessions")?;
        for session in sessions.iter().take(1000) {
            if let Some(mut agent) = normalize_event(provider, kind, session, observed) {
                agent["runtimeId"] = json!(runtime);
                if agent["live"] == true {
                    agent["pid"] = input["pid"].clone();
                }
                // A server can host many conversations; its terminal is not proof
                // that a particular conversation is selected in that terminal.
                agent["target"] = Value::Null;
                persist_agent(store, agent, kind)?;
            }
        }
        for result in input["results"].as_array().into_iter().flatten().take(100) {
            if let Some(id) = result["id"].as_str() {
                write_json(
                    &store.join("results").join(format!("{}.json", key(id))),
                    result,
                )?;
            }
        }
        let directory = store.join("commands").join(key(runtime));
        let mut commands = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten().take(100) {
                let command = read_json(&entry.path())?;
                if command["expiresAt"].as_u64().unwrap_or(0) >= observed {
                    commands.push(command);
                }
                std::fs::remove_file(entry.path())?;
            }
        }
        return Ok(json!({"commands": commands}));
    }
    if let Some(mut agent) = normalize_event(provider, kind, input, observed) {
        agent["pid"] = json!(provider_pid(provider));
        if let Some(pane) = std::env::var_os("TMUX_PANE") {
            agent["target"] = json!({"tmuxPane": pane.to_string_lossy()});
        } else if let Some(terminal) = std::env::var_os("_BONSAI_HQ_TERMINAL_ID") {
            agent["target"] = json!({"terminalId": terminal.to_string_lossy()});
        }
        persist_agent(store, agent, kind)?;
    }
    if provider == "claude" && kind == "statusline" {
        let session = input["session_id"]
            .as_str()
            .context("missing Claude session ID")?;
        for (window, label) in [
            ("five_hour", "5 hours"),
            ("seven_day", "7 days"),
            ("spend_limit", "Spend limit"),
        ] {
            if let Some(used) = input["rate_limits"][window]["used_percentage"].as_f64() {
                let quota = json!({"id": format!("claude:{session}:{window}"), "provider": "claude", "label": label,
                    "sourceSessionId":session,
                    "usedPercent": used, "resetsAt": input["rate_limits"][window]["resets_at"],
                    "observedAt": observed, "stale": false});
                write_json(
                    &store
                        .join("quotas")
                        .join(format!("claude-{}-{window}.json", key(session))),
                    &quota,
                )?;
            }
        }
    }
    Ok(json!({}))
}

pub(crate) fn normalize_event(
    provider: &str,
    kind: &str,
    input: &Value,
    observed: u64,
) -> Option<Value> {
    let root = input["session_id"]
        .as_str()
        .or_else(|| input["sessionId"].as_str())?;
    let event = input["hook_event_name"].as_str().unwrap_or(kind);
    let child = matches!(event, "SubagentStart" | "SubagentStop")
        .then(|| input["agent_id"].as_str())
        .flatten();
    let session = child.unwrap_or(root);
    let state = match event {
        "SessionStart" => "idle",
        "UserPromptSubmit" | "SubagentStart" | "PostToolUse" => "running",
        "PermissionRequest" => "waiting",
        "Notification" if input["notification_type"] == "permission_prompt" => "waiting",
        "Notification" if input["notification_type"] == "idle_prompt" => "idle",
        "Stop" | "SubagentStop" => "completed",
        "StopFailure" => "failed",
        "SessionEnd" | "Interrupt" => "stopped",
        "snapshot" => input["state"].as_str().unwrap_or("unknown"),
        "statusline" => "unknown",
        _ => return None,
    };
    let cwd = input["cwd"]
        .as_str()
        .or_else(|| input["workspace"]["current_dir"].as_str())
        .unwrap_or("");
    let model = input["model"]
        .as_str()
        .or_else(|| input["model"]["id"].as_str());
    let title = input["title"]
        .as_str()
        .or_else(|| input["session_title"].as_str())
        .or_else(|| input["agent_type"].as_str())
        .unwrap_or(provider);
    let reason = if state == "waiting" {
        Some(
            input["waitingReason"]
                .as_str()
                .unwrap_or("Permission required"),
        )
    } else if state == "failed" {
        Some(input["error"].as_str().unwrap_or("Agent failed"))
    } else {
        None
    };
    Some(
        json!({"id": format!("{provider}:{session}"), "provider": provider, "sessionId": session,
        "parentId": child.map(|_| format!("{provider}:{root}")).or_else(|| input["parentId"].as_str().map(|id| format!("{provider}:{id}"))),
        "cwd": cwd, "worktreePath": null, "title": title.chars().take(200).collect::<String>(), "model": model,
        "state": state, "waitingReason": reason, "updatedAt": if kind == "statusline" {Value::Null} else if kind == "snapshot" {input["updatedAt"].clone()} else {json!(observed)},
        "observedAt": observed, "live": if kind == "snapshot" {input["live"].as_bool().unwrap_or(false)} else {!matches!(state, "stopped")}, "stale": false,
        "target": null, "capabilities": input.get("capabilities").cloned().unwrap_or(json!([])), "turnId": input["turn_id"], "requestId": input["requestId"],
        "requestKind": input["requestKind"], "pid": input["pid"],
        "completionObserved": matches!(event, "Stop" | "SubagentStop"),
        "source": if kind == "snapshot" {"bridge"} else if kind == "statusline" {"statusline"} else {"hook"}}),
    )
}

fn provider_pid(provider: &str) -> Option<u64> {
    let inherited = std::env::var("_BONSAI_HQ_PROVIDER_PID")
        .ok()
        .and_then(|pid| pid.parse::<u64>().ok());
    #[cfg(unix)]
    {
        let bytes = super::agents::command_output(
            Command::new("ps").args(["-axo", "pid=,ppid=,comm="]),
            std::time::Duration::from_secs(1),
        )
        .ok()?;
        let rows: std::collections::BTreeMap<u64, (u64, String)> = String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?.parse().ok()?;
                let parent = fields.next()?.parse().ok()?;
                Some((pid, (parent, fields.collect::<Vec<_>>().join(" "))))
            })
            .collect();
        provider_ancestor(provider, u64::from(std::process::id()), inherited, &rows)
    }
    #[cfg(not(unix))]
    {
        let _ = (provider, inherited);
        None
    }
}

#[cfg(unix)]
fn provider_ancestor(
    provider: &str,
    mut pid: u64,
    inherited: Option<u64>,
    rows: &std::collections::BTreeMap<u64, (u64, String)>,
) -> Option<u64> {
    for _ in 0..32 {
        let (parent, executable) = rows.get(&pid)?;
        let name = Path::new(executable).file_name()?.to_string_lossy();
        if name == provider
            || name.starts_with(&format!("{provider}-"))
            || (inherited == Some(pid) && name == "node")
        {
            return Some(pid);
        }
        if *parent <= 1 || *parent == pid {
            return None;
        }
        pid = *parent;
    }
    None
}

fn persist_agent(store: &Path, mut agent: Value, kind: &str) -> Result<()> {
    let id = agent["id"].as_str().context("missing agent ID")?;
    let path = store.join("agents").join(format!("{}.json", key(id)));
    let previous = read_json(&path)?;
    if kind == "snapshot"
        && previous["live"] == true
        && agent["live"] != true
        && previous["runtimeId"] != agent["runtimeId"]
    {
        return Ok(());
    }
    if agent["state"] == "stopped"
        && (previous["state"] == "failed"
            || (previous["state"] == "completed" && previous["completionObserved"] == true))
    {
        for field in [
            "state",
            "completionObserved",
            "updatedAt",
            "waitingReason",
            "requestId",
            "requestKind",
        ] {
            agent[field] = previous[field].clone();
        }
    }
    if kind == "statusline" {
        let metadata = agent;
        agent = if previous["id"].is_string() {
            previous.clone()
        } else {
            metadata.clone()
        };
        for field in ["model", "observedAt", "target", "cwd"] {
            if !metadata[field].is_null() && metadata[field] != "" {
                agent[field] = metadata[field].clone();
            }
        }
        if !previous["id"].is_string() {
            agent["updatedAt"] = Value::Null;
        }
    }
    for field in ["model", "target", "title", "turnId", "pid", "parentId"] {
        if (agent[field].is_null() || (field == "title" && agent[field] == agent["provider"]))
            && let Some(value) = previous.get(field)
        {
            agent[field] = value.clone();
        }
    }
    let changed =
        previous["state"] != agent["state"] || previous["requestId"] != agent["requestId"];
    agent["stateRevision"] =
        json!(previous["stateRevision"].as_u64().unwrap_or(0) + u64::from(changed));
    if agent["state"] == "completed" {
        agent["completionObserved"] = json!(
            agent["completionObserved"] == true
                || (!changed && previous["completionObserved"] == true)
                || (kind == "snapshot"
                    && previous["live"] == true
                    && previous["state"] != "completed")
        );
    }
    agent["stateSince"] = if changed {
        agent["observedAt"].clone()
    } else {
        previous["stateSince"].clone()
    };
    if kind == "snapshot" && !changed && agent["updatedAt"] == agent["observedAt"] {
        agent["updatedAt"] = previous["updatedAt"].clone();
    }
    write_json(&path, &agent)
}

pub(crate) fn records(store: &Path, directory: &str) -> Vec<Value> {
    let mut entries: Vec<_> = std::fs::read_dir(store.join(directory))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    entries.sort_by_cached_key(|entry| {
        std::cmp::Reverse(
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok(),
        )
    });
    entries
        .into_iter()
        .take(2000)
        .filter_map(|entry| read_json(&entry.path()).ok())
        .collect()
}

pub(crate) fn queue(store: &Path, runtime: &str, command: &Value) -> Result<String> {
    let id = format!("{}-{:016x}", now(), rand::random::<u64>());
    let mut command = command.clone();
    command["id"] = json!(id);
    command["expiresAt"] = json!(now() + 5);
    with_store_lock(store, || {
        write_json(
            &store
                .join("commands")
                .join(key(runtime))
                .join(format!("{}.json", key(&id))),
            &command,
        )
    })?;
    Ok(id)
}

pub(crate) fn wait_result(store: &Path, id: &str) -> Result<Value> {
    let path = store.join("results").join(format!("{}.json", key(id)));
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < 8 {
        if path.exists() {
            let result = read_json(&path)?;
            std::fs::remove_file(path)?;
            ensure!(
                result["ok"] == true,
                "{}",
                result["error"]
                    .as_str()
                    .unwrap_or("provider rejected the action")
            );
            return Ok(result);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("provider has not acknowledged this action; refresh before trying again")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_preserves_existing_hooks_statusline_and_user_edits() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("claude");
        let store = temp.path().join("store");
        let target = home.join("settings.json");
        let original = json!({"theme": "dark", "statusLine": {"type":"command","command":"printf original"},
            "hooks":{"Stop":[{"hooks":[{"type":"command","command":"my-hook"}]}]}});
        write_json(&target, &original).unwrap();
        install_at(&store, "claude", &home, Path::new("/bin/bonsai"), false).unwrap();
        let installed = read_json(&target).unwrap();
        install_at(&store, "claude", &home, Path::new("/bin/bonsai"), false).unwrap();
        assert_eq!(read_json(&target).unwrap(), installed);
        assert_eq!(installed["hooks"]["Stop"].as_array().unwrap().len(), 2);
        let mut edited = installed;
        edited["theme"] = json!("light");
        edited["statusLine"] = json!({"type":"command","command":"user-replacement"});
        write_json(&target, &edited).unwrap();
        uninstall(&store, "claude").unwrap();
        let removed = read_json(&target).unwrap();
        assert_eq!(removed["theme"], "light");
        assert_eq!(removed["statusLine"]["command"], "user-replacement");
        assert_eq!(removed["hooks"]["Stop"], original["hooks"]["Stop"]);
    }

    #[test]
    fn hook_receipts_preserve_parent_identity_without_recording_conversation() {
        let input = json!({"session_id":"parent", "agent_id":"child", "hook_event_name":"SubagentStart",
            "cwd":"/work", "model":"model", "prompt":"secret", "transcript_path":"secret-file"});
        let value = normalize_event("claude", "event", &input, 100).unwrap();
        assert_eq!(value["id"], "claude:child");
        assert_eq!(value["parentId"], "claude:parent");
        assert_eq!(value["state"], "running");
        assert!(!value.to_string().contains("secret"));
    }

    #[test]
    fn statusline_never_clears_pending_attention_or_refreshes_activity_age() {
        let temp = tempfile::tempdir().unwrap();
        let permission =
            json!({"session_id":"session", "cwd":"/work", "hook_event_name":"PermissionRequest"});
        ingest(temp.path(), "claude", "event", &permission, 100).unwrap();
        let statusline = json!({"session_id":"session", "model":{"id":"new-model"},
            "rate_limits":{"five_hour":{"used_percentage":42,"resets_at":999}}});
        ingest(temp.path(), "claude", "statusline", &statusline, 200).unwrap();
        let agents = records(temp.path(), "agents");
        assert_eq!(agents[0]["state"], "waiting");
        assert_eq!(agents[0]["updatedAt"], 100);
        assert_eq!(agents[0]["model"], "new-model");
        assert_eq!(records(temp.path(), "quotas")[0]["usedPercent"], 42.0);
    }

    #[test]
    fn statusline_preserves_request_identity_and_namespaces_unproven_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let event = json!({"session_id":"one", "agent_id":"child", "cwd":"/work", "hook_event_name":"SubagentStart"});
        ingest(temp.path(), "claude", "event", &event, 99).unwrap();
        let event = json!({"session_id":"child", "cwd":"/work", "hook_event_name":"PermissionRequest", "requestId":"request-1","requestKind":"approval"});
        ingest(temp.path(), "claude", "event", &event, 100).unwrap();
        let path = temp
            .path()
            .join("agents")
            .join(format!("{}.json", key("claude:child")));
        let before = read_json(&path).unwrap();
        let statusline = json!({"session_id":"child", "rate_limits":{"five_hour":{"used_percentage":42,"resets_at":999}}});
        ingest(temp.path(), "claude", "statusline", &statusline, 200).unwrap();
        let after = read_json(&path).unwrap();
        for field in [
            "requestId",
            "requestKind",
            "cwd",
            "parentId",
            "stateSince",
            "state",
            "updatedAt",
        ] {
            assert_eq!(
                after[field], before[field],
                "metadata update changed {field}"
            );
        }
        let statusline = json!({"session_id":"other", "rate_limits":{"five_hour":{"used_percentage":4,"resets_at":999}}});
        ingest(temp.path(), "claude", "statusline", &statusline, 201).unwrap();
        let quotas = records(temp.path(), "quotas");
        assert_eq!(quotas.len(), 2);
        assert_ne!(quotas[0]["id"], quotas[1]["id"]);
    }

    #[test]
    fn statusline_is_forwarded_when_recording_fails_or_is_disabled() {
        let temp = tempfile::tempdir().unwrap();
        write_json(
            &temp.path().join("claude-installation.json"),
            &json!({"originalStatusLine":{"command":"cat"}}),
        )
        .unwrap();
        // A file where the agent directory should be forces event persistence to fail.
        std::fs::write(temp.path().join("agents"), "blocked").unwrap();
        let bytes = br#"{"session_id":"s","cwd":"/work"}"#;
        let mut output = Vec::new();
        process_bridge(temp.path(), "claude", "statusline", bytes, &mut output).unwrap();
        assert_eq!(output, bytes);
        write_json(
            &temp.path().join("integrations.json"),
            &json!({"claude":{"enabled":false}}),
        )
        .unwrap();
        output.clear();
        process_bridge(temp.path(), "claude", "statusline", bytes, &mut output).unwrap();
        assert_eq!(output, bytes);
    }

    #[test]
    fn interrupted_install_recovery_does_not_wrap_the_wrapper() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("claude");
        let store = temp.path().join("store");
        write_json(
            &home.join("settings.json"),
            &json!({"statusLine":{"type":"command","command":"original"}}),
        )
        .unwrap();
        install_at(&store, "claude", &home, Path::new("/bin/bonsai"), false).unwrap();
        let manifest_path = store.join("claude-installation.json");
        let mut manifest = read_json(&manifest_path).unwrap();
        manifest["installed"] = json!(false);
        write_json(&manifest_path, &manifest).unwrap();
        install_at(&store, "claude", &home, Path::new("/bin/new-bonsai"), true).unwrap();
        let repaired = read_json(&manifest_path).unwrap();
        assert_eq!(repaired["originalStatusLine"]["command"], "original");
        assert_eq!(
            read_json(&home.join("settings.json")).unwrap()["hooks"]["Stop"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn opencode_history_is_not_claimed_as_live_or_bound_to_the_runtime_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let snapshot = json!({"runtimeId":"runtime","pid":42,"target":{"tmuxPane":"%0"},"sessions":[
            {"sessionId":"old","cwd":"/work","state":"unknown","live":false},
            {"sessionId":"working","cwd":"/work","state":"running","live":true}]});
        ingest(temp.path(), "opencode", "snapshot", &snapshot, 100).unwrap();
        let sessions = records(temp.path(), "agents");
        let old = sessions
            .iter()
            .find(|agent| agent["sessionId"] == "old")
            .unwrap();
        assert_eq!(old["live"], false);
        assert!(sessions.iter().all(|agent| agent["target"].is_null()));
    }

    #[test]
    fn another_runtime_importing_history_does_not_erase_a_live_session() {
        let temp = tempfile::tempdir().unwrap();
        ingest(temp.path(), "opencode", "snapshot", &json!({"runtimeId":"active", "sessions":[{"sessionId":"s","state":"waiting","live":true,"requestKind":"approval","requestId":"r"}]}), 100).unwrap();
        ingest(temp.path(), "opencode", "snapshot", &json!({"runtimeId":"other", "sessions":[{"sessionId":"s","state":"unknown","live":false}]}), 101).unwrap();
        let agents = records(temp.path(), "agents");
        assert_eq!(agents[0]["runtimeId"], "active");
        assert_eq!(agents[0]["requestId"], "r");
        assert_eq!(agents[0]["state"], "waiting");
    }

    #[test]
    fn same_second_completion_episodes_have_distinct_revisions() {
        let temp = tempfile::tempdir().unwrap();
        let event = |name| json!({"session_id":"s", "hook_event_name":name});
        ingest(temp.path(), "claude", "event", &event("Stop"), 100).unwrap();
        let first = records(temp.path(), "agents")[0].clone();
        ingest(
            temp.path(),
            "claude",
            "event",
            &event("UserPromptSubmit"),
            100,
        )
        .unwrap();
        ingest(temp.path(), "claude", "event", &event("Stop"), 100).unwrap();
        let second = records(temp.path(), "agents")[0].clone();
        assert_eq!(first["stateSince"], second["stateSince"]);
        assert_ne!(first["stateRevision"], second["stateRevision"]);
        assert_eq!(second["completionObserved"], true);
        assert!(normalize_event("claude", "event", &json!({"session_id":"s","hook_event_name":"Notification","notification_type":"auth_success"}), 101).is_none());
    }

    #[test]
    fn session_exit_preserves_unreviewed_outcome_until_another_lifecycle_starts() {
        let temp = tempfile::tempdir().unwrap();
        for (session, outcome, state) in [
            ("done", "Stop", "completed"),
            ("error", "StopFailure", "failed"),
        ] {
            let event = |name| json!({"session_id":session, "hook_event_name":name, "error":"Agent failed"});
            ingest(
                temp.path(),
                "claude",
                "event",
                &event("UserPromptSubmit"),
                100,
            )
            .unwrap();
            ingest(temp.path(), "claude", "event", &event(outcome), 101).unwrap();
            let path = temp
                .path()
                .join("agents")
                .join(format!("{}.json", key(&format!("claude:{session}"))));
            let outcome = read_json(&path).unwrap();
            ingest(temp.path(), "claude", "event", &event("SessionEnd"), 102).unwrap();
            let exited = read_json(&path).unwrap();
            assert_eq!(exited["state"], state);
            assert_eq!(exited["live"], false);
            assert_eq!(exited["observedAt"], 102);
            for field in [
                "updatedAt",
                "stateSince",
                "stateRevision",
                "completionObserved",
                "waitingReason",
            ] {
                assert_eq!(
                    exited[field], outcome[field],
                    "session exit changed {field}"
                );
            }
            ingest(
                temp.path(),
                "claude",
                "event",
                &event("UserPromptSubmit"),
                103,
            )
            .unwrap();
            let restarted = read_json(&path).unwrap();
            assert_eq!(restarted["state"], "running");
            assert_eq!(restarted["live"], true);
            assert_eq!(restarted["completionObserved"], false);
            assert_ne!(restarted["stateRevision"], exited["stateRevision"]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn inherited_provider_pid_requires_actual_ancestry_and_provider_identity() {
        let rows = std::collections::BTreeMap::from([
            (50, (40, "bonsai".to_owned())),
            (40, (30, "sh".to_owned())),
            (30, (20, "/bin/node".to_owned())),
            (20, (1, "zsh".to_owned())),
            (99, (1, "/bin/claude".to_owned())),
        ]);
        assert_eq!(provider_ancestor("claude", 50, Some(99), &rows), None);
        assert_eq!(provider_ancestor("claude", 50, Some(40), &rows), None);
        assert_eq!(provider_ancestor("claude", 50, Some(30), &rows), Some(30));
        let mut native = rows;
        native.insert(30, (20, "/installed/claude".to_owned()));
        assert_eq!(provider_ancestor("claude", 50, None, &native), Some(30));
    }

    #[test]
    fn expired_control_commands_are_never_delivered() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("commands").join(key("runtime"));
        write_json(
            &directory.join("old.json"),
            &json!({"id":"old", "expiresAt":10}),
        )
        .unwrap();
        let result = ingest(
            temp.path(),
            "opencode",
            "snapshot",
            &json!({"runtimeId":"runtime","sessions":[]}),
            20,
        )
        .unwrap();
        assert_eq!(result["commands"], json!([]));
        assert!(!directory.join("old.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn settings_symlink_survives_install_and_uninstall() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("claude");
        std::fs::create_dir_all(&home).unwrap();
        let real = temp.path().join("dotfiles.json");
        write_json(&real, &json!({"custom":true})).unwrap();
        let link = home.join("settings.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let store = temp.path().join("store");
        install_at(&store, "claude", &home, Path::new("/bin/bonsai"), false).unwrap();
        uninstall(&store, "claude").unwrap();
        assert!(link.is_symlink());
        assert_eq!(read_json(&real).unwrap()["custom"], true);
    }
}
