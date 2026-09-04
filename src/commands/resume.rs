use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::config::Config;
use crate::picker;
use crate::repo::Repo;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Provider {
    Claude,
    Codex,
    OpenCode,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    fn resume_args(self, id: &str) -> Vec<String> {
        match self {
            Self::Claude => vec!["--resume".into(), id.into()],
            Self::Codex => vec!["resume".into(), id.into()],
            Self::OpenCode => vec!["--session".into(), id.into()],
        }
    }
}

#[derive(Clone, Debug)]
struct Session {
    provider: Provider,
    id: String,
    title: String,
    cwd: PathBuf,
    branch: Option<String>,
    updated: SystemTime,
}

struct ProjectScope {
    main_root: PathBuf,
    bonsai_dir: PathBuf,
    repo_id: String,
    /// Current worktrees, longest path first so nested custom worktrees get
    /// the most specific label.
    worktrees: Vec<(PathBuf, String)>,
}

impl ProjectScope {
    fn new(repo: &Repo, config: &Config) -> Result<Self> {
        let main_root =
            normalized_absolute(&repo.main_root).unwrap_or_else(|| repo.main_root.to_path_buf());
        let bonsai_dir = crate::paths::canonicalize_lenient(&repo.bonsai_dir(config));
        let mut worktrees = repo
            .project_worktrees(config)?
            .into_iter()
            .filter(|entry| !entry.worktree.is_bare)
            .filter_map(|entry| {
                let path = normalized_absolute(&entry.worktree.path)?;
                Some((path, entry.label()))
            })
            .collect::<Vec<_>>();
        worktrees.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        Ok(Self {
            main_root,
            bonsai_dir,
            repo_id: repo.id(config),
            worktrees,
        })
    }

    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.main_root)
            || path.starts_with(&self.bonsai_dir)
            || self
                .worktrees
                .iter()
                .any(|(worktree, _)| path.starts_with(worktree))
    }

    fn contains_project_root(&self, path: &Path) -> bool {
        path == self.main_root
            || path.starts_with(&self.bonsai_dir)
            || self.worktrees.iter().any(|(worktree, _)| path == worktree)
    }

    fn normalize_if_member(&self, path: impl AsRef<Path>) -> Option<PathBuf> {
        let path = normalized_absolute(path.as_ref())?;
        self.contains(&path).then_some(path)
    }

    fn location(&self, path: &Path) -> String {
        if let Some((root, label)) = self
            .worktrees
            .iter()
            .find(|(root, _)| path.starts_with(root))
        {
            let suffix = path.strip_prefix(root).unwrap_or(Path::new(""));
            return if suffix.as_os_str().is_empty() {
                label.clone()
            } else {
                format!("{label}/{}", suffix.display())
            };
        }
        if let Ok(relative) = path.strip_prefix(&self.bonsai_dir) {
            return format!("{} (removed)", relative.display());
        }
        path.display().to_string()
    }
}

pub fn run(config: &Config, query: Option<String>) -> Result<()> {
    let repo = Repo::require()?;
    let scope = ProjectScope::new(&repo, config)?;
    let mut sessions = HashMap::new();

    collect_provider("Claude Code", claude_sessions(&scope), &mut sessions);
    collect_provider("Codex", codex_sessions(&scope), &mut sessions);
    collect_provider("OpenCode", opencode_sessions(&scope), &mut sessions);

    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    if sessions.is_empty() {
        bail!("no sessions found for the current project");
    }
    sort_sessions(&mut sessions);

    let picked = if let Some(picked) = resolve_query(&sessions, query.as_deref()) {
        picked
    } else {
        let rows = session_rows(&sessions, &scope);
        picker::select_styled("Session:", picker::recent_options(&rows), query.as_deref())?
    };
    launch(&sessions[picked], &scope)
}

fn sort_sessions(sessions: &mut [Session]) {
    sessions.sort_by(|left, right| {
        right
            .updated
            .cmp(&left.updated)
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn collect_provider(
    name: &str,
    result: Result<Vec<Session>>,
    sessions: &mut HashMap<(Provider, String), Session>,
) {
    match result {
        Ok(found) => {
            for session in found {
                let key = (session.provider, session.id.clone());
                match sessions.entry(key) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(session);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let existing = entry.get_mut();
                        if session.updated > existing.updated {
                            existing.updated = session.updated;
                            existing.cwd = session.cwd;
                            if session.title != "Untitled session" {
                                existing.title = session.title;
                            }
                        }
                        if existing.branch.is_none() {
                            existing.branch = session.branch;
                        }
                    }
                }
            }
        }
        Err(error) => eprintln!("bonsai: warning: could not read {name} sessions: {error:#}"),
    }
}

fn resolve_query(sessions: &[Session], query: Option<&str>) -> Option<usize> {
    let query = query?;
    if let Some(exact) = sessions.iter().position(|session| session.id == query) {
        return Some(exact);
    }
    let query = query.to_lowercase();
    let matches = sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| session_search_text(session).to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    (matches.len() == 1).then(|| matches[0])
}

fn session_rows(sessions: &[Session], scope: &ProjectScope) -> Vec<picker::RecentRow> {
    sessions
        .iter()
        .map(|session| {
            let location = scope.location(&session.cwd);
            picker::RecentRow {
                columns: vec![
                    session.provider.label().to_string(),
                    sanitize(&session.title, 52),
                    sanitize(&location, 36),
                ],
                search: format!("{} {location}", session_search_text(session)),
                last_change: Some(session.updated),
            }
        })
        .collect()
}

fn session_search_text(session: &Session) -> String {
    format!(
        "{} {} {} {} {}",
        session.provider.label(),
        session.id,
        session.title,
        session.branch.as_deref().unwrap_or_default(),
        session.cwd.display()
    )
}

fn launch(session: &Session, scope: &ProjectScope) -> Result<()> {
    let Some(program) = crate::pm::find_program(session.provider.executable()) else {
        bail!(
            "{} session selected but '{}' was not found on PATH",
            session.provider.label(),
            session.provider.executable()
        );
    };
    // PATH entries may be relative to bonsai's cwd; resolve before changing
    // the child cwd to the historical session directory.
    let program = crate::paths::canonicalize_or_self(&program);
    let cwd = if session.cwd.is_dir() {
        &session.cwd
    } else {
        eprintln!(
            "bonsai: original session directory no longer exists ({}); resuming from {}",
            session.cwd.display(),
            scope.main_root.display()
        );
        &scope.main_root
    };
    let status = Command::new(program)
        .args(session.provider.resume_args(&session.id))
        .current_dir(cwd)
        .status()
        .with_context(|| format!("failed to launch {}", session.provider.executable()))?;
    if !status.success() {
        bail!(
            "{} exited with status {}",
            session.provider.executable(),
            status
        );
    }
    Ok(())
}

fn claude_sessions(scope: &ProjectScope) -> Result<Vec<Session>> {
    let root = env_path("CLAUDE_CONFIG_DIR").unwrap_or(home_dir()?.join(".claude"));
    let mut sessions = Vec::new();
    let history = root.join("history.jsonl");
    if history.is_file() {
        sessions.extend(claude_history(&history, scope)?);
    }

    let projects = root.join("projects");
    let Ok(project_dirs) = fs::read_dir(&projects) else {
        return Ok(sessions);
    };
    for project_dir in project_dirs.flatten() {
        let Ok(file_type) = project_dir.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(project_dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            if file.path().extension() != Some(OsStr::new("jsonl")) {
                continue;
            }
            if let Ok(Some(session)) = parse_claude_transcript(&file.path(), scope) {
                sessions.push(session);
            }
        }
    }
    Ok(sessions)
}

fn claude_history(path: &Path, scope: &ProjectScope) -> Result<Vec<Session>> {
    let file = File::open(path)?;
    let mut sessions: HashMap<String, Session> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = json_string(&value, "sessionId") else {
            continue;
        };
        let Some(cwd) =
            json_string(&value, "project").and_then(|path| scope.normalize_if_member(path))
        else {
            continue;
        };
        let updated = value
            .get("timestamp")
            .and_then(Value::as_u64)
            .map(time_from_unknown_epoch)
            .unwrap_or(UNIX_EPOCH);
        let title = json_string(&value, "display")
            .map(|title| sanitize(&title, 72))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Untitled session".to_string());
        let session = Session {
            provider: Provider::Claude,
            id: id.clone(),
            title,
            cwd,
            branch: None,
            updated,
        };
        match sessions.entry(id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(session);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if session.updated > entry.get().updated {
                    entry.get_mut().updated = session.updated;
                    entry.get_mut().cwd = session.cwd;
                }
            }
        }
    }
    Ok(sessions.into_values().collect())
}

fn parse_claude_transcript(path: &Path, scope: &ProjectScope) -> Result<Option<Session>> {
    let file = File::open(path)?;
    let mut id = path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut cwd = None;
    let mut branch = None;
    let mut title = None;
    for line in BufReader::new(file).lines().map_while(Result::ok).take(200) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(session_id) = json_string(&value, "sessionId") {
            id = session_id;
        }
        if cwd.is_none() {
            cwd = json_string(&value, "cwd").and_then(|path| scope.normalize_if_member(path));
        }
        if branch.is_none() {
            branch = json_string(&value, "gitBranch");
        }
        if title.is_none() {
            title = claude_title(&value);
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let updated = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH);
    Ok(Some(Session {
        provider: Provider::Claude,
        id,
        title: title.unwrap_or_else(|| "Untitled session".to_string()),
        cwd,
        branch,
        updated,
    }))
}

fn claude_title(value: &Value) -> Option<String> {
    let event_type = value.get("type")?.as_str()?;
    let title = match event_type {
        "agent-name" => json_string(value, "agentName"),
        "ai-title" => json_string(value, "aiTitle"),
        "last-prompt" => json_string(value, "lastPrompt"),
        "user"
            if value.get("isMeta").and_then(Value::as_bool) != Some(true)
                && value.get("isSidechain").and_then(Value::as_bool) != Some(true) =>
        {
            value.pointer("/message/content").and_then(json_text)
        }
        _ => None,
    }?;
    let title = sanitize(&title, 72);
    (!title.is_empty()).then_some(title)
}

fn codex_sessions(scope: &ProjectScope) -> Result<Vec<Session>> {
    let root = env_path("CODEX_HOME").unwrap_or(home_dir()?.join(".codex"));
    let database_root = codex_database_root(&root);
    let databases = codex_state_databases(&database_root);
    let mut sessions = Vec::new();
    let mut read_database = false;
    let mut last_error = None;
    for database in &databases {
        match codex_database_sessions(database, scope) {
            Ok(found) => {
                read_database = true;
                sessions.extend(found);
            }
            Err(error) => last_error = Some(error),
        }
    }
    sessions.extend(codex_legacy_sessions(&root, scope)?);
    match (read_database, sessions.is_empty(), last_error) {
        (false, true, Some(error)) => Err(error),
        _ => Ok(sessions),
    }
}

fn codex_database_root(codex_home: &Path) -> PathBuf {
    if let Some(path) = env_path("CODEX_SQLITE_HOME") {
        return path;
    }
    fs::read_to_string(codex_home.join("config.toml"))
        .ok()
        .and_then(|contents| contents.parse::<toml::Value>().ok())
        .and_then(|config| config.get("sqlite_home")?.as_str().map(str::to_string))
        .map(|path| crate::config::expand_tilde(&path))
        .unwrap_or_else(|| codex_home.to_path_buf())
}

fn codex_state_databases(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut databases = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((version, entry.path()))
        })
        .collect::<Vec<_>>();
    databases.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    databases.into_iter().map(|(_, path)| path).collect()
}

fn codex_database_sessions(path: &Path, scope: &ProjectScope) -> Result<Vec<Session>> {
    let connection = open_database(path)?;
    let columns = table_columns(&connection, "threads")?;
    if !columns.contains("id") || !columns.contains("cwd") {
        bail!("{} has no compatible threads table", path.display());
    }
    let title = coalesce_text(
        &columns,
        &["name", "title", "preview", "first_user_message"],
        "id",
    );
    let mut timestamps = Vec::new();
    if columns.contains("recency_at_ms") {
        timestamps.push("NULLIF(recency_at_ms, 0)");
    }
    if columns.contains("updated_at_ms") {
        timestamps.push("NULLIF(updated_at_ms, 0)");
    }
    if columns.contains("updated_at") {
        timestamps.push("updated_at * 1000");
    }
    timestamps.push("0");
    let updated = format!("COALESCE({})", timestamps.join(", "));
    let branch = if columns.contains("git_branch") {
        "git_branch"
    } else {
        "NULL"
    };
    let source = if columns.contains("source") {
        "source"
    } else {
        "''"
    };
    let origin = if columns.contains("git_origin_url") {
        "git_origin_url"
    } else {
        "NULL"
    };
    let mut filters = Vec::new();
    if columns.contains("archived") {
        filters.push("archived = 0");
    }
    if columns.contains("preview") {
        filters.push("preview <> ''");
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let sql = format!(
        "SELECT id, cwd, {title}, {updated}, {branch}, {source}, {origin} \
         FROM threads{where_clause}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        let (id, cwd, title, updated, branch, source, origin) = row?;
        if !codex_source_is_interactive(&source) {
            continue;
        }
        let Some(cwd) = normalized_absolute(Path::new(&cwd)) else {
            continue;
        };
        let origin_matches = origin
            .as_deref()
            .and_then(crate::repo::repo_id_from_url)
            .is_some_and(|repo_id| repo_id == scope.repo_id);
        let origin_mismatches = origin
            .as_deref()
            .and_then(crate::repo::repo_id_from_url)
            .is_some_and(|repo_id| repo_id != scope.repo_id);
        if origin_mismatches {
            continue;
        }
        if !scope.contains(&cwd) && !origin_matches {
            continue;
        }
        sessions.push(Session {
            provider: Provider::Codex,
            id,
            title: nonempty_title(&title),
            cwd,
            branch,
            updated: time_from_millis(updated),
        });
    }
    Ok(sessions)
}

fn codex_legacy_sessions(root: &Path, scope: &ProjectScope) -> Result<Vec<Session>> {
    let history_path = root.join("history.jsonl");
    if !history_path.is_file() {
        return Ok(Vec::new());
    }
    let mut history: HashMap<String, (String, SystemTime)> = HashMap::new();
    for line in BufReader::new(File::open(history_path)?)
        .lines()
        .map_while(Result::ok)
    {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = json_string(&value, "session_id") else {
            continue;
        };
        let title = json_string(&value, "text")
            .map(|value| sanitize(&value, 72))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Untitled session".to_string());
        let updated = value
            .get("ts")
            .and_then(Value::as_u64)
            .map(time_from_unknown_epoch)
            .unwrap_or(UNIX_EPOCH);
        history
            .entry(id)
            .and_modify(|entry| entry.1 = entry.1.max(updated))
            .or_insert((title, updated));
    }
    let ids = history.keys().cloned().collect::<HashSet<_>>();
    let mut files = Vec::new();
    walk_jsonl(&root.join("sessions"), &mut files);
    let mut sessions = Vec::new();
    for path in files {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let Some(id) = ids.iter().find(|id| name.contains(id.as_str())) else {
            continue;
        };
        let Some((cwd, branch)) = codex_rollout_metadata(&path, scope)? else {
            continue;
        };
        let (title, updated) = history
            .get(id)
            .cloned()
            .unwrap_or_else(|| ("Untitled session".to_string(), UNIX_EPOCH));
        sessions.push(Session {
            provider: Provider::Codex,
            id: id.clone(),
            title,
            cwd,
            branch,
            updated,
        });
    }
    Ok(sessions)
}

fn codex_rollout_metadata(
    path: &Path,
    scope: &ProjectScope,
) -> Result<Option<(PathBuf, Option<String>)>> {
    let Some(line) = BufReader::new(File::open(path)?)
        .lines()
        .map_while(Result::ok)
        .next()
    else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&line) else {
        return Ok(None);
    };
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Ok(None);
    }
    let Some(payload) = value.get("payload") else {
        return Ok(None);
    };
    let Some(cwd) = json_string(payload, "cwd").and_then(|cwd| scope.normalize_if_member(cwd))
    else {
        return Ok(None);
    };
    let branch = payload
        .pointer("/git/branch")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Some((cwd, branch)))
}

fn opencode_sessions(scope: &ProjectScope) -> Result<Vec<Session>> {
    let data = env_path("XDG_DATA_HOME").unwrap_or_else(default_data_home);
    let path = env_path("OPENCODE_DB")
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                data.join("opencode").join(path)
            }
        })
        .unwrap_or_else(|| data.join("opencode/opencode.db"));
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let connection = open_database(&path)?;
    let project_ids = opencode_project_ids(&connection, scope)?;
    let columns = table_columns(&connection, "session")?;
    if !columns.contains("id") || !columns.contains("directory") {
        bail!("{} has no compatible session table", path.display());
    }
    let title = coalesce_text(&columns, &["title", "slug"], "id");
    let updated = if columns.contains("time_updated") {
        "time_updated"
    } else if columns.contains("time_created") {
        "time_created"
    } else {
        "0"
    };
    let project = if columns.contains("project_id") {
        "project_id"
    } else {
        "''"
    };
    let mut filters = Vec::new();
    if columns.contains("parent_id") {
        filters.push("parent_id IS NULL");
    }
    if columns.contains("time_archived") {
        filters.push("(time_archived IS NULL OR time_archived = 0)");
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let sql =
        format!("SELECT id, directory, {title}, {updated}, {project} FROM session{where_clause}");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        let (id, directory, title, updated, project_id) = row?;
        let Some(cwd) = normalized_absolute(Path::new(&directory)) else {
            continue;
        };
        let project_matches = project_ids.contains(&project_id);
        let path_fallback = project_ids.is_empty() && scope.contains(&cwd);
        if !project_matches && !path_fallback {
            continue;
        }
        sessions.push(Session {
            provider: Provider::OpenCode,
            id,
            title: nonempty_title(&title),
            cwd,
            branch: None,
            updated: time_from_millis(updated),
        });
    }
    Ok(sessions)
}

fn opencode_project_ids(connection: &Connection, scope: &ProjectScope) -> Result<HashSet<String>> {
    let mut matches = HashSet::new();
    let project_columns = table_columns(connection, "project")?;
    if project_columns.contains("id") && project_columns.contains("worktree") {
        let mut statement = connection.prepare("SELECT id, worktree FROM project")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, directory) = row?;
            if normalized_absolute(Path::new(&directory))
                .is_some_and(|path| scope.contains_project_root(&path))
            {
                matches.insert(id);
            }
        }
    }
    let directory_columns = table_columns(connection, "project_directory")?;
    if directory_columns.contains("project_id") && directory_columns.contains("directory") {
        let mut statement =
            connection.prepare("SELECT project_id, directory FROM project_directory")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, directory) = row?;
            if normalized_absolute(Path::new(&directory))
                .is_some_and(|path| scope.contains_project_root(&path))
            {
                matches.insert(id);
            }
        }
    }
    Ok(matches)
}

fn open_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn coalesce_text(columns: &HashSet<String>, candidates: &[&str], fallback: &str) -> String {
    let mut expressions = candidates
        .iter()
        .filter(|column| columns.contains(**column))
        .map(|column| format!("NULLIF({column}, '')"))
        .collect::<Vec<_>>();
    expressions.push(fallback.to_string());
    format!("COALESCE({})", expressions.join(", "))
}

fn json_string(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_string)
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => blocks.iter().find_map(json_text),
        Value::Object(object) => object.get("text").and_then(json_text),
        _ => None,
    }
}

fn nonempty_title(value: &str) -> String {
    let value = sanitize(value, 72);
    if value.is_empty() {
        "Untitled session".to_string()
    } else {
        value
    }
}

fn codex_source_is_interactive(source: &str) -> bool {
    let source = source.trim_matches('"').to_ascii_lowercase();
    source.is_empty() || source == "cli" || source == "vscode"
}

fn sanitize(value: &str, limit: usize) -> String {
    let mut cleaned = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for escaped in chars.by_ref() {
                        if ('@'..='~').contains(&escaped) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(escaped) = chars.next() {
                        if escaped == '\u{7}' {
                            break;
                        }
                        if escaped == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if character.is_control() {
            cleaned.push(' ');
        } else {
            cleaned.push(character);
        }
    }
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let mut result = chars
        .by_ref()
        .take(limit)
        .collect::<String>()
        .trim_end()
        .to_string();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf> {
    std::env::home_dir().context("cannot determine home directory")
}

fn default_data_home() -> PathBuf {
    home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".local/share")
}

fn time_from_unknown_epoch(value: u64) -> SystemTime {
    let elapsed = if value >= 100_000_000_000 {
        Duration::from_millis(value)
    } else {
        Duration::from_secs(value)
    };
    UNIX_EPOCH.checked_add(elapsed).unwrap_or(UNIX_EPOCH)
}

fn time_from_millis(value: i64) -> SystemTime {
    u64::try_from(value)
        .ok()
        .and_then(|value| UNIX_EPOCH.checked_add(Duration::from_millis(value)))
        .unwrap_or(UNIX_EPOCH)
}

fn normalized_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
        }
    }
    Some(crate::paths::canonicalize_or_self(&normalized))
}

fn walk_jsonl(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_jsonl(&entry.path(), files);
        } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("jsonl")) {
            files.push(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(root: &Path) -> ProjectScope {
        ProjectScope {
            main_root: root.join("repo"),
            bonsai_dir: root.join("bonsai/host/owner/repo"),
            repo_id: "host/owner/repo".to_string(),
            worktrees: vec![
                (root.join("custom-worktree"), "custom".to_string()),
                (root.join("repo"), "main (root)".to_string()),
            ],
        }
    }

    #[test]
    fn scope_includes_current_and_removed_worktrees_without_prefix_collisions() {
        let root = Path::new("/tmp/resume-scope");
        let scope = scope(root);
        assert!(scope.contains(&root.join("repo/src")));
        assert!(scope.contains(&root.join("bonsai/host/owner/repo/old-branch")));
        assert!(scope.contains(&root.join("custom-worktree/subdir")));
        assert!(!scope.contains(&root.join("repo-other")));
        assert!(!scope.contains(&root.join("bonsai/host/owner/repository")));
        assert!(scope.contains_project_root(&root.join("repo")));
        assert!(!scope.contains_project_root(&root.join("repo/nested-project")));
    }

    #[test]
    fn titles_are_single_line_control_free_and_bounded() {
        let title = sanitize(
            "  fix\n\u{1b}[31m parser\tbehavior with a long explanation  ",
            20,
        );
        assert_eq!(title, "fix parser behavior…");
        assert!(!title.contains('\n'));
        assert!(!title.contains('\u{1b}'));
    }

    #[test]
    fn exact_and_unique_queries_resolve_without_a_picker() {
        let mut sessions = vec![
            Session {
                provider: Provider::Claude,
                id: "old-id".into(),
                title: "Renderer work".into(),
                cwd: "/tmp/repo/old".into(),
                branch: Some("old".into()),
                updated: UNIX_EPOCH + Duration::from_secs(1),
            },
            Session {
                provider: Provider::Codex,
                id: "new-id".into(),
                title: "Parser cleanup".into(),
                cwd: "/tmp/repo/new".into(),
                branch: Some("new".into()),
                updated: UNIX_EPOCH + Duration::from_secs(2),
            },
        ];
        sort_sessions(&mut sessions);
        assert_eq!(sessions[0].id, "new-id");
        assert_eq!(resolve_query(&sessions, Some("old-id")), Some(1));
        assert_eq!(resolve_query(&sessions, Some("parser")), Some(0));
        assert_eq!(resolve_query(&sessions, Some("work")), Some(1));
        assert_eq!(resolve_query(&sessions, Some("repo")), None);
        assert_eq!(resolve_query(&sessions, Some("no-match")), None);
    }

    #[test]
    fn codex_uses_session_recency_before_content_update_time() {
        let temporary = tempfile::tempdir().unwrap();
        let scope = scope(temporary.path());
        let database_path = temporary.path().join("state_5.sqlite");
        let database = Connection::open(&database_path).unwrap();
        database
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    cwd TEXT NOT NULL,
                    title TEXT NOT NULL,
                    preview TEXT NOT NULL,
                    recency_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    archived INTEGER NOT NULL
                );",
            )
            .unwrap();
        let cwd = temporary.path().join("repo");
        database
            .execute(
                "INSERT INTO threads VALUES ('recent', ?1, 'Recent', 'Recent', 2000, 1000, 'cli', 0)",
                [cwd.to_string_lossy().as_ref()],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO threads VALUES ('stale', ?1, 'Stale', 'Stale', 1000, 3000, 'cli', 0)",
                [cwd.to_string_lossy().as_ref()],
            )
            .unwrap();
        drop(database);

        let mut sessions = codex_database_sessions(&database_path, &scope).unwrap();
        sort_sessions(&mut sessions);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "recent");
    }
}
