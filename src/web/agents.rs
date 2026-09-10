mod codex;
mod process;
use codex::{CodexClient, codex_account_quotas, codex_snapshot};
#[cfg(test)]
use codex::{normalize_codex, normalize_codex_quotas};
pub(crate) use process::command_output;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::integrations::{self, now, read_json, records, with_store_lock, write_json};
use crate::config::Config;

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const HISTORY_INTERVAL: Duration = Duration::from_secs(60);
const STALE_AFTER: u64 = 45;
const RECENT_FOR: u64 = 7 * 24 * 3600;

#[derive(Default, Clone)]
struct Cache {
    refreshing: BTreeSet<String>,
    refreshed: BTreeMap<String, Instant>,
    quota_refreshed: BTreeMap<String, Instant>,
    history_refreshing: bool,
    processes_refreshing: bool,
    processes_refreshed: Option<Instant>,
    history_refreshed: Option<Instant>,
    agents: Vec<Value>,
    history: Vec<Value>,
    integrations: Vec<Value>,
    quotas: Vec<Value>,
    processes: BTreeMap<u64, u64>,
}

pub struct AgentManager {
    config: Config,
    store: PathBuf,
    cache: Arc<Mutex<Cache>>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl AgentManager {
    pub fn new(config: &Config) -> Self {
        let store = integrations::store_dir(config);
        let agents = records(&store, "native")
            .into_iter()
            .filter_map(|value| value.as_array().cloned())
            .flatten()
            .collect();
        Self {
            config: config.clone(),
            store,
            cache: Arc::new(Mutex::new(Cache {
                agents,
                ..Cache::default()
            })),
            workers: Mutex::new(Vec::new()),
        }
    }

    pub fn annotate(&self, mut value: Value) -> Value {
        let project_paths: Vec<PathBuf> = value["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|project| project["path"].as_str().map(PathBuf::from))
            .collect();
        self.refresh(project_paths);
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let mut agents = BTreeMap::<String, Value>::new();
        for history in &cache.history {
            if let Some(agent) = normalize_history(history) {
                merge_agent(&mut agents, agent);
            }
        }
        for agent in records(&self.store, "agents") {
            merge_agent(&mut agents, agent);
        }
        for agent in &cache.agents {
            merge_agent(&mut agents, agent.clone());
        }
        let mut quotas = records(&self.store, "quotas");
        for quota in &cache.quotas {
            quotas.retain(|existing| existing["id"] != quota["id"]);
            quotas.push(quota.clone());
        }
        let mut statuses = if cache.integrations.is_empty() {
            ["claude", "codex", "opencode"]
                .iter()
                .map(|provider| integrations::status(&self.store, provider))
                .collect()
        } else {
            cache.integrations.clone()
        };
        let observed = now();
        let mut list: Vec<Value> = agents.into_values().collect();
        associate(&mut list, &value, &cache.processes, observed);
        let disabled: BTreeSet<_> = ["claude", "codex", "opencode"]
            .into_iter()
            .filter(|provider| !integrations::enabled(&self.store, provider))
            .collect();
        for agent in &mut list {
            if disabled.contains(agent["provider"].as_str().unwrap_or("")) {
                agent["live"] = json!(false);
                agent["stale"] = json!(true);
                agent["capabilities"] = json!(["resume"]);
                if !agent["target"].is_null() {
                    agent["capabilities"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("attach"));
                }
            }
        }
        for status in &mut statuses {
            let provider = status["provider"].as_str().unwrap_or("");
            if status["status"] != "error"
                && integrations::enabled(&self.store, provider)
                && list.iter().any(|agent| {
                    agent["provider"] == provider
                        && agent["live"] == true
                        && agent["stale"] == false
                })
            {
                status["status"] = json!("connected");
                status["message"] = json!("Receiving live session data");
            }
        }
        for provider in ["claude", "codex", "opencode"] {
            if !quotas.iter().any(|quota| quota["provider"] == provider) {
                quotas.push(
                    json!({"id": format!("{provider}:unavailable"), "provider": provider,
                    "label": "Subscription", "usedPercent": null, "resetsAt": null,
                    "observedAt": observed, "stale": false,
                    "unavailableReason": match provider {
                        "claude" => "Waiting for subscription usage from the Claude statusline",
                        "codex" => "Codex has not returned subscription usage",
                        _ => "OpenCode does not expose a shared subscription quota interface",
                    }}),
                );
            }
        }
        for quota in &mut quotas {
            if quota["usedPercent"].is_number() {
                quota["stale"] =
                    json!(observed.saturating_sub(quota["observedAt"].as_u64().unwrap_or(0)) > 300);
            }
        }
        let preferences = read_json(&self.store.join("attention.json")).unwrap_or(json!({}));
        let visits = read_json(&self.store.join("visits.json")).unwrap_or(json!({}));
        let attention = attention_items(&list, &preferences, observed);
        rank_worktrees(&mut value, &mut list, &attention, &visits, observed);
        list.sort_by(|left, right| {
            state_order(left)
                .cmp(&state_order(right))
                .then_with(|| right["updatedAt"].as_u64().cmp(&left["updatedAt"].as_u64()))
                .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
        });
        value["agents"] = json!(list);
        value["attention"] = json!(attention);
        value["quotas"] = json!(quotas);
        value["integrations"] = json!(statuses);
        value
    }

    fn refresh(&self, project_paths: Vec<PathBuf>) {
        if cfg!(test) {
            return;
        }
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if !cache.history_refreshing
            && cache
                .history_refreshed
                .is_none_or(|time| time.elapsed() >= HISTORY_INTERVAL)
        {
            cache.history_refreshing = true;
            let shared = Arc::clone(&self.cache);
            let config = self.config.clone();
            self.spawn_worker(move || {
                let history = crate::commands::resume::discover_for_hq(&config, &project_paths);
                let mut cache = shared.lock().unwrap_or_else(|error| error.into_inner());
                if let Ok(history) = history {
                    cache.history = history;
                }
                cache.history_refreshed = Some(Instant::now());
                cache.history_refreshing = false;
            });
        }
        if !cache.processes_refreshing
            && cache
                .processes_refreshed
                .is_none_or(|time| time.elapsed() >= REFRESH_INTERVAL)
        {
            cache.processes_refreshing = true;
            let shared = Arc::clone(&self.cache);
            self.spawn_worker(move || {
                let processes = process_parents();
                let mut cache = shared.lock().unwrap_or_else(|error| error.into_inner());
                if !processes.is_empty() {
                    cache.processes = processes;
                }
                cache.processes_refreshed = Some(Instant::now());
                cache.processes_refreshing = false;
            });
        }
        for provider in ["claude", "codex", "opencode"] {
            if cache.refreshing.contains(provider)
                || cache
                    .refreshed
                    .get(provider)
                    .is_some_and(|time| time.elapsed() < REFRESH_INTERVAL)
            {
                continue;
            }
            cache.refreshing.insert(provider.to_owned());
            let quota_due = cache
                .quota_refreshed
                .get(provider)
                .is_none_or(|time| time.elapsed() >= HISTORY_INTERVAL);
            let previous: Vec<Value> = cache
                .agents
                .iter()
                .filter(|agent| agent["provider"] == provider)
                .cloned()
                .collect();
            let shared = Arc::clone(&self.cache);
            let config = self.config.clone();
            let store = self.store.clone();
            self.spawn_worker(move || {
                let (agents, status, quotas) =
                    refresh_provider(&config, &store, provider, quota_due);
                let mut agents = match agents {
                    Some(mut agents) => {
                        retain_transition_times(&mut agents, &json!(previous), now());
                        if lifecycle_snapshot(&agents) != lifecycle_snapshot(&previous) {
                            let _ = with_store_lock(&store, || {
                                write_json(
                                    &store.join("native").join(format!("{provider}.json")),
                                    &json!(agents),
                                )
                            });
                        }
                        agents
                    }
                    None => previous,
                };
                let mut cache = shared.lock().unwrap_or_else(|error| error.into_inner());
                cache.agents.retain(|agent| agent["provider"] != provider);
                cache.agents.append(&mut agents);
                cache
                    .integrations
                    .retain(|entry| entry["provider"] != provider);
                cache.integrations.push(status);
                if !quotas.is_empty() {
                    cache.quotas.retain(|quota| quota["provider"] != provider);
                    cache.quotas.extend(quotas);
                }
                if quota_due {
                    cache
                        .quota_refreshed
                        .insert(provider.to_owned(), Instant::now());
                }
                cache.refreshed.insert(provider.to_owned(), Instant::now());
                cache.refreshing.remove(provider);
            });
        }
    }

    fn spawn_worker(&self, work: impl FnOnce() + Send + 'static) {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        workers.retain(|worker| !worker.is_finished());
        workers.push(std::thread::spawn(work));
    }

    pub fn visit(&self, path: &Path) -> Result<()> {
        let path = crate::paths::canonicalize_or_self(path);
        with_store_lock(&self.store, || {
            let target = self.store.join("visits.json");
            let mut visits = read_json(&target)?;
            visits[path.to_string_lossy().as_ref()] = json!(now());
            write_json(&target, &visits)
        })
    }

    pub fn acknowledge(&self, id: &str) -> Result<()> {
        ensure!(!id.is_empty() && id.len() < 1024, "invalid attention ID");
        let mut agents = BTreeMap::new();
        for agent in records(&self.store, "agents") {
            merge_agent(&mut agents, agent);
        }
        for agent in &self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .agents
        {
            merge_agent(&mut agents, agent.clone());
        }
        let items = attention_items(&agents.into_values().collect::<Vec<_>>(), &json!({}), now());
        ensure!(
            items
                .iter()
                .any(|item| item["id"] == id && item["kind"] == "completed"),
            "only a completed result can be marked reviewed; outstanding requests remain pending"
        );
        with_store_lock(&self.store, || {
            let target = self.store.join("attention.json");
            let mut acknowledgements = read_json(&target)?;
            let entries = acknowledgements
                .as_object_mut()
                .context("invalid acknowledgement store")?;
            entries.retain(|_, timestamp| {
                timestamp
                    .as_u64()
                    .is_some_and(|time| now().saturating_sub(time) < 30 * 86400)
            });
            entries.insert(id.to_owned(), json!(now()));
            write_json(&target, &acknowledgements)
        })
    }

    pub fn action(
        &self,
        id: &str,
        action: &str,
        text: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let action = if action == "reply" { "prompt" } else { action };
        let mut agents = records(&self.store, "agents");
        let cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        for agent in &cache.agents {
            if let Some(previous) = agents
                .iter_mut()
                .find(|previous| previous["id"] == agent["id"])
            {
                let mut map = BTreeMap::new();
                merge_agent(&mut map, previous.clone());
                merge_agent(&mut map, agent.clone());
                *previous = map.into_values().next().unwrap();
            } else {
                agents.push(agent.clone());
            }
        }
        for history in &cache.history {
            if let Some(agent) = normalize_history(history)
                && !agents.iter().any(|existing| existing["id"] == agent["id"])
            {
                agents.push(agent);
            }
        }
        drop(cache);
        let agent = agents
            .iter()
            .find(|agent| agent["id"] == id)
            .context("agent session is no longer available")?;
        let provider = agent["provider"]
            .as_str()
            .context("agent provider is unavailable")?;
        ensure!(
            integrations::enabled(&self.store, provider),
            "provider integration is disabled"
        );
        let session = agent["sessionId"]
            .as_str()
            .context("agent session ID is unavailable")?;
        if matches!(action, "attach" | "resume" | "open") {
            return Ok(
                json!({"kind":"navigate", "target":agent["target"], "provider":provider,
                "sessionId":session, "cwd":agent["cwd"], "backgroundId":agent["backgroundId"]}),
            );
        }
        ensure!(
            ["prompt", "interrupt", "approve", "reject"].contains(&action),
            "unsupported agent action"
        );
        ensure!(
            agent["live"] == true,
            "session is not running; resume it first"
        );
        ensure!(
            now().saturating_sub(agent["observedAt"].as_u64().unwrap_or(0)) < STALE_AFTER,
            "agent state is stale; reconnect before sending a command"
        );
        if action == "prompt" {
            ensure!(
                text.is_some_and(|text| !text.trim().is_empty()
                    && text.len() <= 32 * 1024
                    && !text.contains('\0')),
                "a prompt between 1 and 32768 bytes without NUL is required"
            );
        }
        if matches!(action, "approve" | "reject") {
            ensure!(
                request_id.is_some()
                    && agent["requestId"].as_str() == request_id
                    && agent["state"] == "waiting",
                "permission request is no longer pending"
            );
        }
        if provider == "opencode" {
            ensure!(
                agent["capabilities"]
                    .as_array()
                    .is_some_and(|capabilities| capabilities
                        .iter()
                        .any(|capability| capability == action
                            || (action == "prompt" && capability == "reply"))),
                "this session does not support the requested control"
            );
            ensure!(
                now().saturating_sub(agent["observedAt"].as_u64().unwrap_or(0)) < STALE_AFTER,
                "OpenCode bridge is disconnected"
            );
            let runtime = agent["runtimeId"]
                .as_str()
                .context("OpenCode session is not connected to the bridge")?;
            let command =
                json!({"action":action, "sessionId":session, "text":text, "requestId":request_id});
            let id = integrations::queue(&self.store, runtime, &command)?;
            let result = integrations::wait_result(&self.store, &id)?;
            if let Some(cwd) = agent["cwd"].as_str() {
                let _ = self.visit(Path::new(cwd));
            }
            return Ok(result);
        }
        if provider == "codex" && agent["nativeRuntime"] == true {
            let mut client = CodexClient::connect()?;
            if action == "prompt" {
                ensure!(
                    agent["state"] != "waiting",
                    "resolve the pending request in the native terminal first"
                );
                let method = if agent["state"] == "running" {
                    "turn/steer"
                } else {
                    "turn/start"
                };
                let mut params =
                    json!({"threadId": session, "input": [{"type":"text", "text":text.unwrap()}]});
                if method == "turn/steer" {
                    let turn = agent["turnId"]
                        .as_str()
                        .context("active turn ID is unavailable; open this session to reply")?;
                    params["expectedTurnId"] = json!(turn);
                }
                return client.request(method, params);
            }
            if action == "interrupt" {
                let turn = agent["turnId"]
                    .as_str()
                    .context("active turn ID is unavailable; open this session to interrupt")?;
                return client.request("turn/interrupt", json!({"threadId":session,"turnId":turn}));
            }
        }
        bail!("this session requires its native terminal for {action}; open it from HQ")
    }

    pub fn integration_action(&self, provider: &str, action: &str) -> Result<()> {
        integrations::setup(&self.store, provider, action)?;
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .refreshed
            .remove(provider);
        Ok(())
    }
}

impl Drop for AgentManager {
    fn drop(&mut self) {
        for worker in self
            .workers
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
        {
            let _ = worker.join();
        }
    }
}

fn refresh_provider(
    config: &Config,
    store: &Path,
    provider: &str,
    quota_due: bool,
) -> (Option<Vec<Value>>, Value, Vec<Value>) {
    let mut agents = None;
    let mut quotas = Vec::new();
    let mut status = integrations::status(store, provider);
    if !integrations::enabled(store, provider) || integrations::executable(provider).is_none() {
        return (agents, status, quotas);
    }
    if config.hq.auto_setup && status["status"] == "unavailable" {
        if let Err(error) = integrations::setup(store, provider, "install") {
            status["status"] = json!("error");
            status["message"] = json!(error.to_string());
        } else {
            status = integrations::status(store, provider);
        }
    }
    match provider {
        "claude" => match command_output(
            Command::new("claude").args(["agents", "--json", "--all"]),
            Duration::from_secs(4),
        )
        .and_then(|bytes| serde_json::from_slice::<Vec<Value>>(&bytes).map_err(Into::into))
        {
            Ok(sessions) => {
                agents = Some(
                    sessions
                        .iter()
                        .filter_map(|session| normalize_claude(session, now()))
                        .collect(),
                );
                if status["status"] != "error" {
                    status["status"] = json!("connected");
                    status["message"] = json!("Native Claude session discovery connected");
                }
            }
            Err(error) => {
                status["message"] = json!(format!("Native session discovery unavailable: {error}"))
            }
        },
        "codex" => match codex_snapshot(quota_due) {
            Ok((sessions, limits)) => {
                agents = Some(sessions);
                quotas = limits;
                if status["status"] != "error" {
                    status["status"] = json!("connected");
                    status["message"] = json!("Connected to the existing Codex app-server");
                }
            }
            Err(_) => {
                if status["status"] != "error" {
                    status["message"] =
                        json!("No shared Codex runtime; hooks observe independent sessions");
                }
                if quota_due && let Ok(limits) = codex_account_quotas() {
                    quotas = limits;
                }
            }
        },
        _ => {}
    }
    (agents, status, quotas)
}

fn lifecycle_snapshot(agents: &[Value]) -> Vec<Value> {
    agents
        .iter()
        .map(|agent| {
            let mut snapshot = agent.clone();
            if let Some(fields) = snapshot.as_object_mut() {
                fields.remove("observedAt");
            }
            snapshot
        })
        .collect()
}

fn merge_agent(agents: &mut BTreeMap<String, Value>, mut agent: Value) {
    let Some(id) = agent["id"].as_str().map(str::to_owned) else {
        return;
    };
    let Some(previous) = agents.get_mut(&id) else {
        agents.insert(id, agent);
        return;
    };
    if agent["source"] == "native"
        && agent["state"] == "idle"
        && previous["state"] == "completed"
        && previous["completionObserved"] == true
    {
        for field in [
            "state",
            "completionObserved",
            "stateSince",
            "stateRevision",
            "updatedAt",
        ] {
            agent[field] = previous[field].clone();
        }
    }
    let metadata_only =
        agent["source"] == "history" || (agent["state"] == "unknown" && agent["live"] == false);
    let observed = agent["observedAt"].as_u64().unwrap_or(0);
    let previous_observed = previous["observedAt"].as_u64().unwrap_or(0);
    let older = observed < previous_observed
        || (observed == previous_observed
            && agent["source"] == "native"
            && previous["source"] == "hook");
    if !metadata_only && !older && agent["state"].is_string() && agent["state"] != "waiting" {
        for field in ["waitingReason", "requestId", "requestKind"] {
            previous[field] = Value::Null;
        }
    }
    for (field, value) in agent.as_object().into_iter().flatten() {
        let identity_metadata = ["title", "model", "cwd", "parentId"].contains(&field.as_str());
        if !metadata_only
            && !older
            && value.is_null()
            && ["waitingReason", "requestId", "requestKind", "turnId"].contains(&field.as_str())
        {
            previous[field] = Value::Null;
            continue;
        }
        let generic_title = field == "title"
            && value
                .as_str()
                .zip(agent["provider"].as_str())
                .is_some_and(|(title, provider)| title.eq_ignore_ascii_case(provider));
        if value.is_null() || value == "" || generic_title {
            continue;
        }
        if field == "updatedAt" {
            if value.as_u64() > previous[field].as_u64() {
                previous[field] = value.clone();
            }
            continue;
        }
        if metadata_only && !identity_metadata {
            continue;
        }
        if older && (!identity_metadata || (!previous[field].is_null() && previous[field] != "")) {
            continue;
        }
        previous[field] = value.clone();
    }
}

fn normalize_history(value: &Value) -> Option<Value> {
    let provider = value["provider"].as_str()?;
    let session = value["sessionId"].as_str()?;
    let mut agent = json!({"id":format!("{provider}:{session}"),"provider":provider,"sessionId":session,
        "parentId":null,"cwd":value["cwd"],"worktreePath":null,"title":value["title"],"model":value["model"],
        "state":"unknown","waitingReason":null,"updatedAt":value["updatedAt"],"observedAt":0,
        "live":false,"stale":false,"target":null,"source":"history","capabilities":["resume"]});
    if let Some(parent) = value["parentId"].as_str() {
        agent["parentId"] = json!(format!("{provider}:{parent}"));
    }
    Some(agent)
}

pub(crate) fn normalize_claude(value: &Value, observed: u64) -> Option<Value> {
    let session = value["sessionId"]
        .as_str()
        .or_else(|| value["id"].as_str())?;
    let state = match value["status"].as_str() {
        Some("busy") => "running",
        Some("waiting") => "waiting",
        Some("idle") => "idle",
        _ => match value["state"].as_str() {
            Some("working") => "running",
            Some("blocked") => "waiting",
            Some("done") => "completed",
            Some("failed") => "failed",
            Some("stopped") => "stopped",
            _ => "unknown",
        },
    };
    let mut agent = json!({"id":format!("claude:{session}"),"provider":"claude","sessionId":session,"parentId":null,
        "cwd":value["cwd"],"worktreePath":null,"title":value["name"].as_str().unwrap_or("Claude"),"model":null,
        "state":state,"waitingReason":value["waitingFor"],"updatedAt":value["startedAt"].as_u64().map(|time|time/1000),
        "observedAt":observed,"live":value["pid"].as_u64().is_some(),"stale":false,"pid":value["pid"],
        "target":null,"capabilities":[],"source":"native","backgroundId":value["id"]});
    if value["state"] == "done" && state == "idle" {
        agent["state"] = json!("completed");
    }
    Some(agent)
}

fn retain_transition_times(agents: &mut [Value], previous: &Value, observed: u64) {
    for agent in agents {
        let old = previous
            .as_array()
            .into_iter()
            .flatten()
            .find(|old| old["id"] == agent["id"]);
        if let Some(old) = old {
            if agent["state"] == "idle"
                && (matches!(old["state"].as_str(), Some("running" | "waiting"))
                    || (old["state"] == "completed" && old["completionObserved"] == true))
            {
                agent["state"] = json!("completed");
            }
            let changed =
                old["state"] != agent["state"] || old["waitingReason"] != agent["waitingReason"];
            agent["stateRevision"] = json!(
                old["stateRevision"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_add(u64::from(changed))
            );
            agent["stateSince"] = if changed {
                json!(observed)
            } else {
                old["stateSince"].clone()
            };
            if changed {
                agent["updatedAt"] = json!(observed);
            } else {
                agent["updatedAt"] = old["updatedAt"].clone();
            }
            agent["completionObserved"] = if changed {
                json!(agent["state"] == "completed")
            } else {
                old["completionObserved"].clone()
            };
        } else {
            agent["stateSince"] = json!(agent["updatedAt"].as_u64().unwrap_or(observed));
            agent["completionObserved"] = json!(false);
            agent["stateRevision"] = json!(0);
        }
    }
}

fn process_parents() -> BTreeMap<u64, u64> {
    command_output(
        Command::new("ps").args(["-axo", "pid=,ppid="]),
        Duration::from_secs(2),
    )
    .ok()
    .map(|bytes| {
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect()
    })
    .unwrap_or_default()
}

fn descends_from(mut pid: u64, ancestor: u64, parents: &BTreeMap<u64, u64>) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        match parents.get(&pid) {
            Some(parent) if *parent != pid && *parent > 1 => pid = *parent,
            _ => return false,
        }
    }
    false
}

fn associate(agents: &mut [Value], inventory: &Value, parents: &BTreeMap<u64, u64>, observed: u64) {
    let panes: Vec<&Value> = inventory["tmux"]["panes"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let terminals: Vec<&Value> = inventory["terminals"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    for agent in agents.iter_mut() {
        if let Some(pid) = agent["pid"].as_u64()
            && let Some(pane) = panes.iter().find(|pane| {
                pane["pid"]
                    .as_u64()
                    .is_some_and(|ancestor| descends_from(pid, ancestor, parents))
            })
        {
            agent["target"] = json!({"tmuxSession":pane["session"],"tmuxPane":pane["id"]});
        }
        if let Some(pane_id) = agent["target"]["tmuxPane"].as_str() {
            if let Some(pane) = panes.iter().find(|pane| pane["id"] == pane_id) {
                agent["target"]["tmuxSession"] = pane["session"].clone();
            } else {
                agent["target"] = Value::Null;
            }
        }
        if let Some(terminal_id) = agent["target"]["terminalId"].as_str()
            && !terminals.iter().any(|terminal| {
                terminal["id"] == terminal_id
                    && terminal["exited"] != true
                    && terminal["kind"] != "tmux"
            })
        {
            agent["target"] = Value::Null;
        }
        let recent =
            observed.saturating_sub(agent["observedAt"].as_u64().unwrap_or(0)) < STALE_AFTER;
        let known_pid = agent["pid"].as_u64();
        let has_process = known_pid.is_some_and(|pid| parents.contains_key(&pid));
        let process_gone = !parents.is_empty() && known_pid.is_some() && !has_process;
        let heartbeat_source = agent["source"] == "bridge"
            || agent["source"] == "native"
            || agent["nativeRuntime"] == true;
        let completed = agent["state"] == "completed" && agent["completionObserved"] == true;
        let stale = !completed
            && agent["observedAt"].as_u64().unwrap_or(0) > 0
            && ((!recent && (heartbeat_source || !has_process)) || process_gone);
        let live = agent["live"] == true
            && !process_gone
            && (recent || (!heartbeat_source && has_process));
        if process_gone {
            agent["target"] = Value::Null;
        }
        let has_target = !agent["target"].is_null();
        agent["live"] = json!(live);
        agent["stale"] = json!(stale);
        let mut capabilities = BTreeSet::new();
        if has_target {
            capabilities.insert("attach");
        }
        if agent["sessionId"].is_string() {
            capabilities.insert("resume");
        }
        if live && recent && agent["runtimeId"].is_string() {
            for capability in agent["capabilities"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                capabilities.insert(if capability == "prompt" {
                    "reply"
                } else {
                    capability
                });
            }
        }
        if live && !stale && agent["nativeRuntime"] == true {
            if agent["state"] != "waiting"
                && (agent["state"] != "running" || agent["turnId"].is_string())
            {
                capabilities.insert("reply");
            }
            if agent["turnId"].is_string() {
                capabilities.insert("interrupt");
            }
        }
        agent["capabilities"] = json!(capabilities);
    }
    let targets: BTreeMap<String, (Value, Value)> = agents
        .iter()
        .filter_map(|agent| {
            Some((
                agent["id"].as_str()?.to_owned(),
                (agent["target"].clone(), agent["parentId"].clone()),
            ))
        })
        .collect();
    for agent in agents {
        if agent["target"].is_null() {
            let mut parent = agent["parentId"].as_str();
            let mut seen = BTreeSet::new();
            while let Some(id) = parent {
                if !seen.insert(id) {
                    break;
                }
                let Some((target, ancestor)) = targets.get(id) else {
                    break;
                };
                if !target.is_null() {
                    agent["target"] = target.clone();
                    if let Some(capabilities) = agent["capabilities"].as_array_mut() {
                        capabilities.push(json!("attach"));
                    }
                    break;
                }
                parent = ancestor.as_str();
            }
        }
    }
}

fn state_order(agent: &Value) -> u8 {
    match agent["state"].as_str() {
        Some("waiting" | "failed") => 0,
        Some("running") => 1,
        Some("completed") => 2,
        _ => 3,
    }
}

fn attention_items(agents: &[Value], acknowledgements: &Value, observed: u64) -> Vec<Value> {
    let mut items = Vec::new();
    for agent in agents {
        if agent["stale"] == true {
            continue;
        }
        let state = agent["state"].as_str().unwrap_or("unknown");
        let kind = match state {
            "waiting" if agent["live"] == true => {
                if agent["waitingReason"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains("permission")
                    || agent["requestKind"] == "approval"
                {
                    "approval"
                } else {
                    "question"
                }
            }
            "failed" if agent["live"] == true || agent["source"] == "hook" => "error",
            "completed" if agent["completionObserved"] == true => "completed",
            _ => continue,
        };
        let created = agent["stateSince"]
            .as_u64()
            .or_else(|| agent["updatedAt"].as_u64())
            .unwrap_or(observed);
        if kind == "completed" && observed.saturating_sub(created) > 86400 {
            continue;
        }
        let id = format!(
            "{}:{kind}:{created}:{}:{}",
            agent["id"].as_str().unwrap_or(""),
            agent["stateRevision"].as_u64().unwrap_or(0),
            agent["requestId"].as_str().unwrap_or("")
        );
        if kind == "completed" && acknowledgements.get(&id).is_some() {
            continue;
        }
        let summary = agent["waitingReason"].as_str().unwrap_or(match kind {
            "completed" => "Response ready",
            "error" => "Agent failed",
            _ => "Input needed",
        });
        let mut item = json!({"id":id,"agentId":agent["id"],"kind":kind,"summary":summary,"createdAt":created});
        if let Some(request) = agent["requestId"].as_str() {
            item["requestId"] = json!(request);
        }
        items.push(item);
    }
    items.sort_by(|left, right| {
        (left["kind"] == "completed")
            .cmp(&(right["kind"] == "completed"))
            .then_with(|| right["createdAt"].as_u64().cmp(&left["createdAt"].as_u64()))
            .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
    });
    items
}

fn rank_worktrees(
    value: &mut Value,
    agents: &mut [Value],
    attention: &[Value],
    visits: &Value,
    observed: u64,
) {
    let paths: Vec<(String, PathBuf)> = value["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|project| project["worktrees"].as_array().into_iter().flatten())
        .filter_map(|tree| tree["path"].as_str())
        .map(|path| {
            (
                path.to_owned(),
                crate::paths::canonicalize_or_self(Path::new(path)),
            )
        })
        .collect();
    for agent in agents.iter_mut() {
        let Some(cwd) = agent["cwd"].as_str().filter(|cwd| !cwd.is_empty()) else {
            agent["worktreePath"] = Value::Null;
            continue;
        };
        let cwd = crate::paths::canonicalize_or_self(Path::new(cwd));
        agent["worktreePath"] = paths
            .iter()
            .filter(|(_, path)| cwd.starts_with(path))
            .max_by_key(|(_, path)| path.components().count())
            .map(|(path, _)| json!(path))
            .unwrap_or(Value::Null);
    }
    for project in value["projects"].as_array_mut().into_iter().flatten() {
        for tree in project["worktrees"].as_array_mut().into_iter().flatten() {
            let path = tree["path"].as_str().unwrap_or("");
            let own: Vec<&Value> = agents
                .iter()
                .filter(|agent| agent["worktreePath"] == path)
                .collect();
            let ids: BTreeSet<&str> = own
                .iter()
                .filter_map(|agent| agent["id"].as_str())
                .collect();
            let canonical = crate::paths::canonicalize_or_self(Path::new(path));
            let mut activity = tree["lastActivity"].as_u64();
            for (visited, timestamp) in visits.as_object().into_iter().flatten() {
                if Path::new(visited).starts_with(&canonical) {
                    activity = activity.max(timestamp.as_u64());
                }
            }
            for pane in tree["activity"]["tmux"].as_array().into_iter().flatten() {
                activity = activity.max(pane["lastActivity"].as_u64());
            }
            for agent in &own {
                activity = activity.max(agent["updatedAt"].as_u64());
            }
            let needs_you = attention
                .iter()
                .any(|item| item["agentId"].as_str().is_some_and(|id| ids.contains(id)));
            let working = own.iter().any(|agent| {
                agent["state"] == "running" && agent["live"] == true && agent["stale"] == false
            });
            tree["priority"] = json!(if needs_you {
                "needs-you"
            } else if working {
                "working"
            } else if activity.is_some_and(|time| observed.saturating_sub(time) < RECENT_FOR) {
                "recent"
            } else {
                "older"
            });
            tree["lastActivity"] = json!(activity);
            tree["agentIds"] = json!(ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_claude_distinguishes_waiting_background_completion_and_process_liveness() {
        let waiting = normalize_claude(
            &json!({"sessionId":"s","pid":12,"cwd":"/a","startedAt":10000,
            "status":"waiting","waitingFor":"input needed"}),
            20,
        )
        .unwrap();
        assert_eq!(waiting["state"], "waiting");
        assert_eq!(waiting["updatedAt"], 10);
        assert_eq!(waiting["live"], true);
        let done = normalize_claude(
            &json!({"id":"short","sessionId":"full","state":"done","cwd":"/a"}),
            20,
        )
        .unwrap();
        assert_eq!(done["state"], "completed");
        assert_eq!(done["live"], false);
    }

    #[test]
    fn refreshes_do_not_make_old_idle_sessions_recent_and_new_waits_get_new_attention() {
        let mut agents = vec![
            normalize_claude(
                &json!({"sessionId":"s","pid":12,"status":"idle","startedAt":1000}),
                10,
            )
            .unwrap(),
        ];
        retain_transition_times(&mut agents, &json!([]), 10);
        let previous = json!(agents);
        agents[0]["observedAt"] = json!(100);
        retain_transition_times(&mut agents, &previous, 100);
        assert_eq!(agents[0]["updatedAt"], 1);
        agents[0]["state"] = json!("waiting");
        retain_transition_times(&mut agents, &previous, 101);
        assert_eq!(agents[0]["updatedAt"], 101);
        let items = attention_items(&agents, &json!({}), 101);
        let mut ack = json!({});
        ack[items[0]["id"].as_str().unwrap()] = json!(101);
        assert_eq!(attention_items(&agents, &ack, 102).len(), 1);
        agents[0]["stateSince"] = json!(103);
        assert_eq!(attention_items(&agents, &ack, 103).len(), 1);
    }

    #[test]
    fn multiple_agents_in_one_checkout_bind_by_process_not_cwd() {
        let mut agents = vec![
            normalize_claude(
                &json!({"sessionId":"a","pid":11,"cwd":"/same","status":"busy"}),
                100,
            )
            .unwrap(),
            normalize_claude(
                &json!({"sessionId":"b","pid":21,"cwd":"/same","status":"waiting"}),
                100,
            )
            .unwrap(),
        ];
        let inventory = json!({"tmux":{"panes":[{"id":"%1","pid":10,"session":"one","path":"/same"},
            {"id":"%2","pid":20,"session":"two","path":"/same"}]},"terminals":[]});
        associate(
            &mut agents,
            &inventory,
            &BTreeMap::from([(11, 10), (21, 20)]),
            100,
        );
        assert_eq!(agents[0]["target"]["tmuxPane"], "%1");
        assert_eq!(agents[1]["target"]["tmuxPane"], "%2");
    }

    #[test]
    fn nested_checkout_ownership_and_recency_do_not_hide_idle_worktrees() {
        let mut inventory = json!({"projects":[{"worktrees":[{"path":"/repo"},{"path":"/repo/child"},{"path":"/idle"}]}]});
        let mut agents = vec![normalize_claude(&json!({"sessionId":"a","pid":11,"cwd":"/repo/child/src","status":"waiting","startedAt":100000}),100).unwrap()];
        let attention = attention_items(&agents, &json!({}), 100);
        rank_worktrees(&mut inventory, &mut agents, &attention, &json!({}), 100);
        assert_eq!(agents[0]["worktreePath"], "/repo/child");
        assert_eq!(
            inventory["projects"][0]["worktrees"][1]["priority"],
            "needs-you"
        );
        assert_eq!(
            inventory["projects"][0]["worktrees"][2]["priority"],
            "older"
        );
        assert_eq!(
            inventory["projects"][0]["worktrees"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn codex_unloaded_threads_never_claim_runtime_control() {
        let thread = json!({"id":"s","status":{"type":"notLoaded"},"updatedAt":10,"cwd":"/work"});
        let agent = normalize_codex(&thread, 100).unwrap();
        assert_eq!(agent["state"], "unknown");
        assert_eq!(agent["live"], false);
        assert_eq!(agent["nativeRuntime"], false);
    }

    #[test]
    fn historical_completion_never_becomes_new_attention_but_observed_completion_does() {
        let mut agents = vec![
            normalize_claude(
                &json!({"sessionId":"s","state":"done","startedAt":1000}),
                100,
            )
            .unwrap(),
        ];
        retain_transition_times(&mut agents, &json!([]), 100);
        assert!(attention_items(&agents, &json!({}), 100).is_empty());
        let mut running = agents.clone();
        running[0]["state"] = json!("running");
        retain_transition_times(&mut agents, &json!(running), 101);
        let items = attention_items(&agents, &json!({}), 101);
        assert_eq!(items.len(), 1);
        let mut acknowledgements = json!({});
        acknowledgements[items[0]["id"].as_str().unwrap()] = json!(101);
        assert!(attention_items(&agents, &acknowledgements, 102).is_empty());
    }

    #[test]
    fn native_idle_after_a_turn_keeps_one_reviewable_completion() {
        let running = json!({"id":"codex:s","provider":"codex","source":"native",
            "state":"running","updatedAt":90,"observedAt":100,"stateSince":90,"stateRevision":3});
        let mut idle = vec![json!({"id":"codex:s","provider":"codex","source":"native",
            "state":"idle","updatedAt":101,"observedAt":101})];
        retain_transition_times(&mut idle, &json!([running]), 101);
        let first = attention_items(&idle, &json!({}), 101);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["kind"], "completed");
        let previous = json!(idle);
        idle[0]["state"] = json!("idle");
        idle[0]["observedAt"] = json!(102);
        retain_transition_times(&mut idle, &previous, 102);
        assert_eq!(attention_items(&idle, &json!({}), 102), first);
        let mut merged = BTreeMap::new();
        merge_agent(&mut merged, idle[0].clone());
        merge_agent(
            &mut merged,
            json!({"id":"codex:s","source":"native",
            "state":"idle","observedAt":103,"updatedAt":103}),
        );
        assert_eq!(
            attention_items(&merged.into_values().collect::<Vec<_>>(), &json!({}), 103),
            first
        );
    }

    #[test]
    fn completed_result_remains_reviewable_after_process_exit_and_hq_restart() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            root: temp.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let manager = AgentManager::new(&config);
        let completed = json!({"id":"claude:s","provider":"claude","source":"native",
            "sessionId":"s","state":"completed","completionObserved":true,"stateRevision":3,
            "live":false,"pid":42,"observedAt":now()-100,"updatedAt":now()-100,
            "stateSince":now()-100,"target":null});
        write_json(
            &manager.store.join("native/claude.json"),
            &json!([completed]),
        )
        .unwrap();
        drop(manager);
        let manager = AgentManager::new(&config);
        manager.cache.lock().unwrap().processes = BTreeMap::from([(1, 0)]);
        let snapshot = manager.annotate(json!({"projects":[],"terminals":[],"tmux":{"panes":[]}}));
        assert_eq!(snapshot["attention"].as_array().unwrap().len(), 1);
        let id = snapshot["attention"][0]["id"].as_str().unwrap();
        manager.acknowledge(id).unwrap();
        drop(manager);
        let manager = AgentManager::new(&config);
        assert!(
            manager.annotate(json!({"projects":[]}))["attention"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_old_pane_does_not_keep_an_exited_agent_running() {
        let mut agents = vec![
            json!({"id":"codex:s","provider":"codex","sessionId":"s","source":"hook",
            "state":"running","live":true,"observedAt":10,"pid":11,"target":{"tmuxPane":"%1"}}),
        ];
        let inventory = json!({"tmux":{"panes":[{"id":"%1","pid":10,"session":"work"}]}});
        associate(&mut agents, &inventory, &BTreeMap::from([(10, 1)]), 100);
        assert_eq!(agents[0]["live"], false);
        assert_eq!(agents[0]["stale"], true);
        assert!(agents[0]["target"].is_null());
    }

    #[test]
    fn quiet_hooks_keep_state_with_the_same_live_provider_but_dead_bridges_are_stale() {
        let base = json!({"id":"codex:s","provider":"codex","sessionId":"s","source":"hook",
            "state":"waiting","live":true,"observedAt":10,"pid":11,"target":{"tmuxPane":"%1"}});
        let inventory = json!({"tmux":{"panes":[{"id":"%1","pid":10,"session":"work"}]}});
        let parents = BTreeMap::from([(10, 1), (11, 10)]);
        let mut agents = vec![base];
        associate(&mut agents, &inventory, &parents, 100);
        assert_eq!(agents[0]["live"], true);
        assert_eq!(agents[0]["stale"], false);
        agents[0]["source"] = json!("bridge");
        associate(&mut agents, &inventory, &parents, 100);
        assert_eq!(agents[0]["stale"], true);
    }

    #[test]
    fn unloaded_native_metadata_cannot_erase_live_hook_evidence() {
        let mut agents = BTreeMap::new();
        merge_agent(
            &mut agents,
            json!({"id":"codex:s","provider":"codex","sessionId":"s","source":"hook",
            "state":"waiting","live":true,"observedAt":20,"updatedAt":20,"title":"my task"}),
        );
        merge_agent(
            &mut agents,
            normalize_codex(
                &json!({"id":"s","status":{"type":"notLoaded"},"updatedAt":25,"cwd":"/work"}),
                30,
            )
            .unwrap(),
        );
        assert_eq!(agents["codex:s"]["state"], "waiting");
        assert_eq!(agents["codex:s"]["live"], true);
        assert_eq!(agents["codex:s"]["observedAt"], 20);
        assert_eq!(agents["codex:s"]["updatedAt"], 25);
    }

    #[test]
    fn newer_lifecycle_observations_clear_old_requests_but_keep_identity_metadata() {
        let mut agents = BTreeMap::new();
        merge_agent(
            &mut agents,
            json!({"id":"codex:s","source":"hook","state":"waiting",
            "observedAt":10,"waitingReason":"Permission needed","requestId":"old","title":"Fix parser",
            "target":{"tmuxPane":"%1"}}),
        );
        merge_agent(
            &mut agents,
            json!({"id":"codex:s","source":"native","state":"idle",
            "observedAt":20,"waitingReason":null,"requestId":null,"title":null,"target":null}),
        );
        assert!(agents["codex:s"]["waitingReason"].is_null());
        assert!(agents["codex:s"]["requestId"].is_null());
        assert_eq!(agents["codex:s"]["title"], "Fix parser");
        assert_eq!(agents["codex:s"]["target"]["tmuxPane"], "%1");
    }

    #[test]
    fn observing_a_tmux_client_does_not_bind_its_outer_hq_terminal() {
        let mut agents = vec![json!({"id":"codex:s","provider":"codex","sessionId":"s",
            "source":"hook","state":"waiting","live":true,"observedAt":100,
            "target":{"terminalId":"client"}})];
        let inventory = json!({"terminals":[{"id":"client","kind":"tmux","exited":false}]});
        associate(&mut agents, &inventory, &BTreeMap::new(), 100);
        assert!(agents[0]["target"].is_null());
    }

    #[test]
    fn filesystem_pane_and_visit_recency_survive_annotation_without_refresh_churn() {
        let observed = RECENT_FOR + 1000;
        let mut inventory = json!({"projects":[{"worktrees":[
            {"path":"/fs","lastActivity":observed-5},
            {"path":"/pane","lastActivity":1,"activity":{"tmux":[{"lastActivity":observed-10}]}},
            {"path":"/visited","lastActivity":1},
            {"path":"/old","lastActivity":1}
        ]}]});
        let visits = json!({"/visited/subdir":observed-20});
        rank_worktrees(&mut inventory, &mut [], &[], &visits, observed);
        let trees = inventory["projects"][0]["worktrees"].as_array().unwrap();
        assert!(trees[..3].iter().all(|tree| tree["priority"] == "recent"));
        assert_eq!(trees[3]["priority"], "older");
        let previous = inventory.clone();
        rank_worktrees(&mut inventory, &mut [], &[], &visits, observed + 1);
        assert_eq!(inventory, previous);
    }

    #[test]
    fn quotas_keep_independent_windows_and_unknown_is_not_zero() {
        let limits = json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":0,"resetsAt":300,"windowDurationMins":300},
            "secondary":{"usedPercent":91,"resetsAt":900,"windowDurationMins":10080}}}});
        let quotas = normalize_codex_quotas(&limits, &json!({}), 100);
        assert_eq!(quotas.len(), 2);
        assert_eq!(quotas[0]["usedPercent"], 0.0);
        assert_eq!(quotas[1]["resetsAt"], 900);
        assert!(normalize_codex_quotas(&json!({}), &json!({}), 100).is_empty());
    }
}
