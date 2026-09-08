mod activity;
pub(crate) mod inventory;
mod security;
pub(crate) mod terminal;
mod tui;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{DefaultBodyLimit, Path as RoutePath, Request, State, WebSocketUpgrade};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::{Cli, Commands};
use crate::config::Config;
use security::Security;
use terminal::TerminalManager;

#[derive(Clone)]
struct AppState {
    config: Config,
    initial_repo: Option<PathBuf>,
    security: Security,
    terminals: Arc<TerminalManager>,
    inventory_cache: Arc<Mutex<Option<CachedInventory>>>,
    notice: Arc<Mutex<Option<String>>>,
}

struct CachedInventory {
    captured: Instant,
    value: Value,
}

pub fn run(config: Config, port: u16, no_open: bool, no_tui: bool) -> Result<()> {
    let initial_repo = std::env::current_dir().ok();
    std::fs::create_dir_all(config.root_dir())
        .context("cannot create the Bonsai workspace root")?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the browser server runtime")?
        .block_on(serve(
            config,
            initial_repo,
            port,
            no_open,
            !no_tui && std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        ))
}

async fn serve(
    config: Config,
    initial_repo: Option<PathBuf>,
    port: u16,
    no_open: bool,
    use_tui: bool,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| format!("cannot listen on 127.0.0.1:{port}; try bonsai hq --port 0"))?;
    let port = listener.local_addr()?.port();
    let token = rand::random::<[u8; 32]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let state = AppState {
        config,
        initial_repo,
        security: Security::new(port, token),
        terminals: Arc::new(TerminalManager::new()),
        inventory_cache: Arc::new(Mutex::new(None)),
        notice: Arc::new(Mutex::new(None)),
    };
    let url = state.security.launch_url();
    println!("{url}");
    std::io::stdout().flush()?;
    eprintln!("Bonsai is running locally. Press Ctrl-C to stop.");
    if !no_open {
        let notice = Arc::clone(&state.notice);
        tokio::task::spawn_blocking(move || {
            if let Err(error) = open_browser(&url) {
                let message = format!("Could not open your browser ({error}); press b to retry.");
                if use_tui {
                    *notice.lock().unwrap_or_else(|error| error.into_inner()) = Some(message);
                } else {
                    eprintln!(
                        "bonsai: could not open your browser ({error}); open the printed link"
                    );
                }
            }
        });
    }

    let stopped = Arc::new(AtomicBool::new(false));
    let (tui_done, finished) = tokio::sync::oneshot::channel();
    let tui_worker = if use_tui {
        let state = state.clone();
        let stopped = Arc::clone(&stopped);
        Some(tokio::task::spawn_blocking(move || {
            let result = tui::run(state, stopped);
            let _ = tui_done.send(());
            result
        }))
    } else {
        None
    };
    let terminals = Arc::clone(&state.terminals);
    let shutdown_terminals = Arc::clone(&terminals);
    let shutdown_stopped = Arc::clone(&stopped);
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown_signal() => {},
                _ = async { if use_tui { let _ = finished.await; } else { std::future::pending::<()>().await; } } => {},
            }
            shutdown_stopped.store(true, Ordering::Release);
            let _ = tokio::task::spawn_blocking(move || shutdown_terminals.shutdown()).await;
        })
        .await;
    stopped.store(true, Ordering::Release);
    tokio::task::spawn_blocking(move || terminals.shutdown()).await?;
    if let Some(worker) = tui_worker {
        worker
            .await
            .context("the terminal interface stopped unexpectedly")??;
    }
    result.context("the browser server stopped unexpectedly")
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(javascript))
        .route("/app.css", get(stylesheet))
        .route("/api/state", get(workspace_state))
        .route("/api/terminals", get(list_terminals).post(create_terminal))
        .route("/api/terminals/{id}", delete(close_terminal))
        .route("/api/terminals/{id}/ws", get(attach_terminal))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "page not found") })
        .method_not_allowed_fallback(|| async {
            ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
        })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), protect))
        .with_state(state)
}

async fn protect(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let mut response =
        match state
            .security
            .authorize(request.method(), request.uri(), request.headers())
        {
            Ok(()) => next.run(request).await,
            Err((status, error)) => ApiError::new(status, error).into_response(),
        };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&state.security.content_security_policy())
            .expect("fixed CSP is valid"),
    );
    response
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../web/dist/index.html"),
    )
}

async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../../web/dist/app.js"),
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../web/dist/app.css"),
    )
}

async fn workspace_state(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let value = tokio::task::spawn_blocking(move || snapshot_value(&state))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(value))
}

fn snapshot_value(state: &AppState) -> Result<Value> {
    // Both frontends share a scan, preventing independent requests from
    // multiplying the inventory's bounded Git worker pool.
    let mut cache = state
        .inventory_cache
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut value = match cache.as_ref() {
        Some(cached) if cached.captured.elapsed() < Duration::from_secs(1) => cached.value.clone(),
        _ => {
            let snapshot = inventory::snapshot(&state.config, state.initial_repo.as_deref())?;
            let mut value = serde_json::to_value(snapshot)?;
            value["tmux"] = serde_json::to_value(terminal::tmux_inventory())?;
            *cache = Some(CachedInventory {
                captured: Instant::now(),
                value: value.clone(),
            });
            value
        }
    };
    drop(cache);
    value["terminals"] = serde_json::to_value(state.terminals.list())?;
    Ok(activity::annotate(value))
}

async fn list_terminals(State(state): State<AppState>) -> Json<Vec<terminal::TerminalSummary>> {
    Json(state.terminals.list())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateTerminal {
    path: PathBuf,
    args: Option<Vec<String>>,
    tmux: Option<String>,
    #[serde(default)]
    new_tmux: bool,
}

async fn create_terminal(
    State(state): State<AppState>,
    payload: Result<Json<CreateTerminal>, JsonRejection>,
) -> Result<(StatusCode, Json<terminal::TerminalSummary>), ApiError> {
    let Json(request) =
        payload.map_err(|error| ApiError::new(error.status(), error.body_text()))?;
    let terminal = tokio::task::spawn_blocking(move || spawn_terminal(&state, request))
        .await
        .map_err(ApiError::internal)??;
    Ok((StatusCode::CREATED, Json(terminal)))
}

fn spawn_terminal(
    state: &AppState,
    request: CreateTerminal,
) -> Result<terminal::TerminalSummary, ApiError> {
    validate_terminal_request(&request)?;
    let path = terminal_path(state, &request)?;
    state
        .terminals
        .spawn(
            &path,
            request.args,
            request.tmux,
            request.new_tmux,
            &state.config,
        )
        .map_err(ApiError::internal)
}

fn validate_terminal_request(request: &CreateTerminal) -> Result<(), ApiError> {
    if request.tmux.is_some() && (request.args.is_some() || request.new_tmux) {
        return Err(ApiError::bad_request(
            "attach a tmux session without args or newTmux",
        ));
    }
    if request.new_tmux && request.args.is_some() {
        return Err(ApiError::bad_request(
            "create a tmux shell without command args",
        ));
    }
    if let Some(args) = &request.args {
        validate_bonsai_args(args)?;
    }
    Ok(())
}

fn validate_bonsai_args(args: &[String]) -> Result<(), ApiError> {
    if args.len() > 128 || args.iter().map(String::len).sum::<usize>() > 16 * 1024 {
        return Err(ApiError::bad_request("command arguments are too large"));
    }
    if args.iter().any(|argument| argument.contains('\0')) {
        return Err(ApiError::bad_request(
            "command arguments cannot contain NUL bytes",
        ));
    }
    if args.iter().any(|argument| {
        argument == "--root"
            || argument.starts_with("--root=")
            || argument == "--remote"
            || argument.starts_with("--remote=")
    }) {
        return Err(ApiError::bad_request(
            "browser commands use the server's root and remote",
        ));
    }
    let cli =
        match Cli::try_parse_from(std::iter::once("bonsai").chain(args.iter().map(String::as_str)))
        {
            Ok(cli) => cli,
            Err(error)
                if matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(ApiError::bad_request(error.to_string())),
        };
    if matches!(cli.command, Commands::Hq { .. }) {
        return Err(ApiError::bad_request(
            "the browser server is already running",
        ));
    }
    Ok(())
}

fn terminal_path(state: &AppState, request: &CreateTerminal) -> Result<PathBuf, ApiError> {
    if let Some(name) = &request.tmux {
        let session = terminal::tmux_inventory()
            .sessions
            .into_iter()
            .find(|session| session.name == *name)
            .ok_or_else(|| {
                ApiError::bad_request("tmux session no longer exists; refresh and try again")
            })?;
        return tmux_client_directory(Path::new(&session.path), &state.config.root_dir());
    }

    let path = canonical_directory(&request.path)?;
    if path == state.config.root_dir() {
        return Ok(path);
    }
    let mut cache = state
        .inventory_cache
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let snapshot = inventory::snapshot(&state.config, state.initial_repo.as_deref())
        .map_err(ApiError::internal)?;
    *cache = None;
    let known = snapshot
        .projects
        .iter()
        .flat_map(|project| &project.worktrees)
        .any(|worktree| {
            crate::paths::canonicalize_ok(&worktree.path).is_some_and(|known| known == path)
        });
    if !known {
        let adding_project = request.args.as_ref().is_some_and(|args| {
            Cli::try_parse_from(std::iter::once("bonsai").chain(args.iter().map(String::as_str)))
                .is_ok_and(|cli| matches!(cli.command, Commands::Add { .. }))
        });
        if adding_project
            && crate::git::Git::at(&path)
                .out(&["rev-parse", "--is-inside-work-tree"])
                .is_ok_and(|inside| inside == "true")
        {
            return Ok(path);
        }
        return Err(ApiError::bad_request(
            "choose an existing worktree from the workspace graph",
        ));
    }
    Ok(path)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ApiError> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(ApiError::bad_request(
            "terminal path must be an existing absolute directory",
        ));
    }
    crate::paths::canonicalize_ok(path)
        .ok_or_else(|| ApiError::bad_request("terminal directory is unavailable"))
}

fn tmux_client_directory(session_path: &Path, root: &Path) -> Result<PathBuf, ApiError> {
    canonical_directory(session_path).or_else(|_| canonical_directory(root))
}

async fn close_terminal(
    State(state): State<AppState>,
    RoutePath(id): RoutePath<String>,
) -> Result<StatusCode, ApiError> {
    let closed = tokio::task::spawn_blocking(move || state.terminals.close(&id))
        .await
        .map_err(ApiError::internal)?;
    if !closed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "terminal no longer exists",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn attach_terminal(
    State(state): State<AppState>,
    RoutePath(id): RoutePath<String>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let session = state
        .terminals
        .get(&id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "terminal no longer exists"))?;
    let upgrade = upgrade.map_err(|error| ApiError::new(error.status(), error.body_text()))?;
    Ok(upgrade
        .max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| session.bridge(socket)))
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    let child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    finish_browser_launch(child)
}

fn finish_browser_launch(mut child: Child) -> Result<()> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "browser opener exited with {status}");
            return Ok(());
        }
        if started.elapsed() >= Duration::from_millis(250) {
            // Some openers stay alive for the browser's lifetime. Reap them
            // outside Tokio, whose shutdown waits for all blocking tasks.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Method;
    use tower::ServiceExt;

    #[cfg(unix)]
    #[test]
    fn browser_process_lifetime_does_not_block_hq_shutdown() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read reply"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let (release, receive) = std::sync::mpsc::channel::<()>();
        let writer = std::thread::spawn(move || {
            let _ = receive.recv_timeout(Duration::from_millis(1500));
            let _ = input.write_all(b"done\n");
        });
        let started = Instant::now();
        let result = finish_browser_launch(child);
        let elapsed = started.elapsed();
        let _ = release.send(());
        writer.join().unwrap();
        assert!(result.is_ok());
        assert!(
            elapsed < Duration::from_secs(1),
            "opener blocked for {elapsed:?}"
        );
    }

    fn state(root: &Path) -> AppState {
        AppState {
            config: Config {
                root: root.to_string_lossy().into_owned(),
                ..Config::default()
            },
            initial_repo: None,
            security: Security::new(4837, "secret".into()),
            terminals: Arc::new(TerminalManager::new()),
            inventory_cache: Arc::new(Mutex::new(None)),
            notice: Arc::new(Mutex::new(None)),
        }
    }

    fn app(root: &Path) -> Router {
        router(state(root))
    }

    fn request(method: Method, path: &str, body: Body) -> Request {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:4837")
            .header(header::ORIGIN, "http://127.0.0.1:4837")
            .header(header::AUTHORIZATION, "Bearer secret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn serves_only_embedded_assets_with_security_headers() {
        let root = tempfile::tempdir().unwrap();
        for (path, mime) in [
            ("/", "text/html"),
            ("/app.js", "text/javascript"),
            ("/app.css", "text/css"),
        ] {
            let mut req = request(Method::GET, path, Body::empty());
            req.headers_mut().remove(header::AUTHORIZATION);
            let response = app(root.path()).oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()[header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with(mime)
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
            assert!(
                response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
            assert!(
                !to_bytes(response.into_body(), 4 * 1024 * 1024)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        for path in ["/../../Cargo.toml", "/.git/config", "/missing.js"] {
            let response = app(root.path())
                .oneshot(request(Method::GET, path, Body::empty()))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn security_runs_before_api_and_websocket_handlers() {
        let root = tempfile::tempdir().unwrap();
        for path in ["/api/state", "/api/terminals", "/api/terminals/missing/ws"] {
            let mut req = request(Method::GET, path, Body::empty());
            req.headers_mut().remove(header::AUTHORIZATION);
            let response = app(root.path()).oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert!(serde_json::from_slice::<Value>(&body).unwrap()["error"].is_string());
        }
    }

    #[tokio::test]
    async fn invalid_creation_cannot_spawn_a_terminal() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let app = app(root.path());
        let bodies = [
            json!({"path": root.path(), "args": ["not-a-command"]}),
            json!({"path": root.path(), "args": ["hq"]}),
            json!({"path": root.path(), "args": ["list", "--root=/tmp"]}),
            json!({"path": root.path(), "args": ["list", "--remote", "other"]}),
            json!({"path": root.path(), "args": ["list"], "newTmux": true}),
            json!({"path": other.path()}),
            json!({"path": root.path().join("missing")}),
            json!({"path": "relative"}),
            json!({"path": root.path(), "unexpected": true}),
        ];
        for body in bodies {
            let response = app
                .clone()
                .oneshot(request(
                    Method::POST,
                    "/api/terminals",
                    Body::from(body.to_string()),
                ))
                .await
                .unwrap();
            assert!(
                response.status().is_client_error(),
                "body {body}: {}",
                response.status()
            );
        }
        let response = app
            .oneshot(request(Method::GET, "/api/terminals", Body::empty()))
            .await
            .unwrap();
        assert_eq!(
            &to_bytes(response.into_body(), 1024).await.unwrap()[..],
            b"[]"
        );
    }

    #[test]
    fn validates_the_real_cli_while_preserving_interactive_commands() {
        for args in [
            vec!["add"],
            vec!["start"],
            vec!["clean"],
            vec!["resume"],
            vec!["prune", "--all"],
            vec!["skill", "install"],
            vec!["--help"],
            vec!["--version"],
            vec!["remove", "--force", "fix"],
        ] {
            assert!(
                validate_bonsai_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
                    .is_ok(),
                "{args:?}"
            );
        }
        for args in [
            vec![],
            vec!["unknown"],
            vec!["hq"],
            vec!["ui"],
            vec!["list", "--unknown"],
            vec!["--root", "/tmp", "list"],
        ] {
            assert!(
                validate_bonsai_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
                    .is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn empty_workspace_root_is_a_valid_terminal_directory() {
        let root = tempfile::tempdir().unwrap();
        let state = state(root.path());
        let request = CreateTerminal {
            path: root.path().join("."),
            args: None,
            tmux: None,
            new_tmux: false,
        };
        assert_eq!(
            terminal_path(&state, &request).ok(),
            Some(state.config.root_dir())
        );
    }

    #[test]
    fn known_tmux_client_can_attach_after_its_original_directory_was_removed() {
        let root = tempfile::tempdir().unwrap();
        let old_directory = root.path().join("removed-worktree");
        assert_eq!(
            tmux_client_directory(&old_directory, root.path()).ok(),
            crate::paths::canonicalize_ok(root.path())
        );
        let current_directory = tempfile::tempdir().unwrap();
        assert_eq!(
            tmux_client_directory(current_directory.path(), root.path()).ok(),
            crate::paths::canonicalize_ok(current_directory.path())
        );
        assert!(tmux_client_directory(&old_directory, &old_directory).is_err());
    }

    #[test]
    fn add_can_onboard_an_existing_project_but_shells_stay_in_known_directories() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        crate::git::Git::at(project.path()).out(&["init"]).unwrap();
        let state = state(root.path());
        let mut request = CreateTerminal {
            path: project.path().to_path_buf(),
            args: Some(vec!["add".into(), "ab/new-worktree".into()]),
            tmux: None,
            new_tmux: false,
        };
        assert_eq!(
            terminal_path(&state, &request).ok(),
            crate::paths::canonicalize_ok(project.path())
        );
        request.args = None;
        assert!(terminal_path(&state, &request).is_err());
        request.args = Some(vec!["list".into()]);
        assert!(terminal_path(&state, &request).is_err());
        let unrelated = tempfile::tempdir().unwrap();
        request.path = unrelated.path().to_path_buf();
        request.args = Some(vec!["add".into()]);
        assert!(terminal_path(&state, &request).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn path_checks_resolve_symlinks_without_allowing_unregistered_directories() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("outside");
        std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
        let request = CreateTerminal {
            path: alias,
            args: None,
            tmux: None,
            new_tmux: false,
        };
        assert!(terminal_path(&state(root.path()), &request).is_err());
    }
}
