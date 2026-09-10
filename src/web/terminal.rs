use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::sync::watch;

use crate::config::Config;

const SESSION_LIMIT: usize = 24;
const REPLAY_LIMIT: usize = 1024 * 1024;
const INPUT_LIMIT: usize = 64 * 1024;
const INPUT_QUEUE: usize = 32;
const IO_TICK: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub id: String,
    pub title: String,
    pub path: String,
    pub kind: String,
    pub exited: bool,
    pub exit_code: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
pub struct TmuxInventory {
    pub available: bool,
    pub sessions: Vec<TmuxSession>,
    pub panes: Vec<TmuxPane>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmuxPane {
    pub id: String,
    pub session: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub path: String,
    pub command: String,
    pub active: bool,
    pub window_active: bool,
    pub pid: Option<u32>,
    pub last_activity: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct TmuxSession {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub path: String,
}

pub fn tmux_inventory() -> TmuxInventory {
    tmux_inventory_with(&[])
}

fn tmux_inventory_with(prefix: &[&str]) -> TmuxInventory {
    let result = Command::new("tmux")
        .args(prefix)
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_windows}\t#{session_attached}\t#{pane_current_path}",
        ])
        .output();
    match result {
        Ok(output) => TmuxInventory {
            available: true,
            sessions: parse_tmux_sessions(&String::from_utf8_lossy(&output.stdout)),
            panes: Command::new("tmux")
                .args(prefix)
                .args(["list-panes", "-a", "-F", "#{pane_id}\u{1f}#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_name}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}\u{1f}#{pane_active}\u{1f}#{window_active}\u{1f}#{pane_pid}\u{1f}#{window_activity}\u{1e}"])
                .output()
                .map(|output| parse_tmux_panes(&String::from_utf8_lossy(&output.stdout)))
                .unwrap_or_default(),
        },
        Err(_) => TmuxInventory::default(),
    }
}

fn parse_tmux_panes(output: &str) -> Vec<TmuxPane> {
    output
        .split('\u{1e}')
        .filter_map(|record| {
            // Record separators preserve tabs and newlines inside window names and paths.
            let record = record.strip_prefix('\n').unwrap_or(record);
            let mut fields = record.split('\u{1f}');
            Some(TmuxPane {
                id: fields.next()?.to_owned(),
                session: fields.next()?.to_owned(),
                window_id: fields.next()?.to_owned(),
                window_index: fields.next()?.parse().ok()?,
                window_name: fields.next()?.to_owned(),
                path: fields.next()?.to_owned(),
                command: fields.next()?.to_owned(),
                active: fields.next()?.parse::<u8>().ok()? != 0,
                window_active: fields.next()?.parse::<u8>().ok()? != 0,
                pid: fields.next().and_then(|value| value.parse().ok()),
                last_activity: fields.next().and_then(|value| value.parse().ok()),
            })
        })
        .collect()
}

fn parse_tmux_sessions(output: &str) -> Vec<TmuxSession> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            Some(TmuxSession {
                name: fields.next()?.to_owned(),
                windows: fields.next()?.parse().ok()?,
                attached: fields.next()?.parse::<u32>().ok()? > 0,
                path: fields.next()?.to_owned(),
            })
        })
        .collect()
}

fn tmux_attach_target(
    inventory: &TmuxInventory,
    session: &str,
    pane: Option<&str>,
) -> Result<String> {
    ensure!(
        inventory.sessions.iter().any(|entry| entry.name == session),
        "tmux session no longer exists"
    );
    match pane {
        Some(id) => {
            let pane = inventory
                .panes
                .iter()
                .find(|entry| entry.id == id && entry.session == session)
                .context("tmux pane no longer exists in the selected session")?;
            Ok(format!("={session}:{}.{}", pane.window_id, pane.id))
        }
        None => Ok(format!("={session}")),
    }
}

#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<BTreeMap<String, Arc<TerminalSession>>>,
    stopped: AtomicBool,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called on a blocking thread after the server validates the working directory and argv.
    pub fn spawn(
        &self,
        path: &Path,
        args: Option<Vec<String>>,
        tmux: Option<String>,
        tmux_pane: Option<String>,
        new_tmux: bool,
        config: &Config,
    ) -> Result<TerminalSummary> {
        ensure!(path.is_dir(), "terminal working directory no longer exists");
        ensure!(
            tmux_pane.is_none() || (tmux.is_some() && !new_tmux && args.is_none()),
            "a pane target requires an existing tmux session"
        );
        ensure!(
            args.is_none() || (tmux.is_none() && !new_tmux),
            "choose a command or tmux session"
        );
        let executable = std::env::current_exe().context("locate bonsai executable")?;
        let (mut command, title, kind, bootstrap) = if let Some(args) = args {
            ensure!(!args.is_empty(), "a bonsai command is required");
            ensure!(
                args.iter().map(String::len).sum::<usize>() <= INPUT_LIMIT,
                "command is too long"
            );
            let mut command = CommandBuilder::new(&executable);
            command.arg("--root");
            command.arg(config.root_dir());
            if let Some(remote) = &config.remote {
                command.args(["--remote", remote]);
            }
            command.args(&args);
            (
                command,
                format!("bonsai {}", args.join(" ")),
                "command",
                None,
            )
        } else if tmux.is_some() || new_tmux {
            let inventory = tmux_inventory();
            ensure!(
                inventory.available,
                "tmux is not installed or is not on PATH"
            );
            let name = tmux.unwrap_or_else(|| tmux_name(path));
            ensure!(
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
                "invalid tmux session name"
            );
            let environment = std::env::var("TMUX").ok();
            let command = if new_tmux {
                new_tmux_command(environment.as_deref(), &name, path, config)
            } else {
                let target = tmux_attach_target(&inventory, &name, tmux_pane.as_deref())?;
                let mut command = tmux_command(environment.as_deref());
                command.args(["attach-session", "-t", &target]);
                command
            };
            (command, format!("tmux · {name}"), "tmux", None)
        } else {
            let (command, bootstrap) = shell_command(&executable, config)?;
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            (command, format!("Shell · {name}"), "shell", bootstrap)
        };
        command.cwd(path);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("BONSAI_ROOT", config.root_dir());
        command.env_remove("_BONSAI_WRAPPED");
        command.env_remove("_BONSAI_WRAPPER_ACTIVE");
        command.env("_BONSAI_HQ_STORE", config.root_dir().join(".hq"));
        if let Some(remote) = &config.remote {
            command.env("BONSAI_REMOTE", remote);
        }
        self.spawn_command(command, path, title, kind, bootstrap)
    }

    fn spawn_command(
        &self,
        mut command: CommandBuilder,
        path: &Path,
        title: String,
        kind: &str,
        bootstrap: Option<TempDir>,
    ) -> Result<TerminalSummary> {
        let mut sessions = self.sessions.lock().unwrap();
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "terminal server is shutting down"
        );
        if sessions.len() >= SESSION_LIMIT {
            // Keep completed output available until another terminal actually needs its slot.
            let completed = sessions
                .iter()
                .find_map(|(id, session)| session.exited().then(|| id.clone()));
            if let Some(id) = completed {
                sessions.remove(&id);
            }
        }
        ensure!(
            sessions.len() < SESSION_LIMIT,
            "close a terminal before opening another (limit: {SESSION_LIMIT})"
        );
        let pair = native_pty_system().openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        #[cfg(unix)]
        set_nonblocking(pair.master.as_ref())?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let id = format!("{:032x}", rand::random::<u128>());
        command.env("_BONSAI_HQ_TERMINAL_ID", &id);
        command.env_remove("TMUX_PANE");
        let child = pair
            .slave
            .spawn_command(command)
            .context("start terminal process")?;
        drop(pair.slave);
        let summary = TerminalSummary {
            id,
            title,
            path: path.to_string_lossy().into_owned(),
            kind: kind.to_owned(),
            exited: false,
            exit_code: None,
        };
        let (changed, _) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            output: Mutex::new(OutputLog::default()),
            stop: AtomicBool::new(false),
            changed,
        });
        let (input, receive) = mpsc::sync_channel(INPUT_QUEUE);
        let worker_shared = shared.clone();
        let worker = std::thread::spawn(move || {
            let _bootstrap = bootstrap;
            run_pty(pair.master, child, reader, writer, receive, worker_shared);
        });
        let session = Arc::new(TerminalSession {
            summary: summary.clone(),
            shared,
            input,
            worker: Mutex::new(Some(worker)),
        });
        sessions.insert(summary.id.clone(), session);
        Ok(summary)
    }

    pub fn list(&self) -> Vec<TerminalSummary> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|session| {
                let mut summary = session.summary.clone();
                let output = session.shared.output.lock().unwrap();
                summary.exited = output.exit.is_some();
                summary.exit_code = output.exit.flatten();
                summary
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<TerminalSession>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub fn close(&self, id: &str) -> bool {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(session) = session {
            session.close();
            true
        } else {
            false
        }
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
        for session in sessions.values() {
            session.shared.stop.store(true, Ordering::Release);
        }
        for session in sessions.values() {
            session.close();
        }
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct TerminalSession {
    summary: TerminalSummary,
    shared: Arc<Shared>,
    input: mpsc::SyncSender<Input>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl TerminalSession {
    pub fn exited(&self) -> bool {
        self.shared.output.lock().unwrap().exit.is_some()
    }

    fn input(&self, bytes: Vec<u8>) -> Result<()> {
        ensure!(bytes.len() <= INPUT_LIMIT, "terminal input exceeds 64 KiB");
        ensure!(!self.exited(), "terminal has exited");
        self.input
            .try_send(Input::Bytes(bytes))
            .context("terminal input queue is full or closed")
    }

    async fn input_async(&self, bytes: Vec<u8>) -> Result<()> {
        ensure!(bytes.len() <= INPUT_LIMIT, "terminal input exceeds 64 KiB");
        let mut pending = Input::Bytes(bytes);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            ensure!(
                !self.exited() && !self.shared.stop.load(Ordering::Acquire),
                "terminal has exited"
            );
            match self.input.try_send(pending) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Disconnected(_)) => bail!("terminal input is closed"),
                Err(mpsc::TrySendError::Full(message)) => {
                    ensure!(
                        Instant::now() < deadline,
                        "terminal stopped accepting input"
                    );
                    pending = message;
                    // Pause this socket's reads so TCP applies backpressure to rapid typing
                    // and paste without growing the queue or blocking a Tokio worker.
                    tokio::time::sleep(IO_TICK).await;
                }
            }
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        ensure!(
            (1..=500).contains(&cols) && (1..=300).contains(&rows),
            "invalid terminal dimensions"
        );
        self.input
            .try_send(Input::Resize(cols, rows))
            .context("terminal input queue is full or closed")
    }

    fn close(&self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }

    pub async fn bridge(self: Arc<Self>, socket: WebSocket) {
        let (mut sender, mut receiver) = socket.split();
        let mut changed = self.shared.changed.subscribe();
        let mut cursor = 0;
        loop {
            // Subscribe before taking the snapshot: output arriving during replay remains visible.
            changed.borrow_and_update();
            let snapshot = self.shared.output.lock().unwrap().snapshot(cursor);
            cursor = snapshot.cursor;
            if snapshot.truncated
                && !send_frame(&mut sender, Message::Text("{\"type\":\"reset\"}".into())).await
            {
                return;
            }
            if !snapshot.data.is_empty()
                && !send_frame(&mut sender, Message::Binary(snapshot.data.into())).await
            {
                return;
            }
            if let Some(code) = snapshot.exit {
                let message = serde_json::json!({ "type": "exit", "code": code }).to_string();
                send_frame(&mut sender, Message::Text(message.into())).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), sender.close()).await;
                return;
            }
            tokio::select! {
                result = changed.changed() => if result.is_err() { return; },
                message = receiver.next() => {
                    let result = match message {
                        Some(Ok(Message::Binary(bytes))) => self.input_async(bytes.to_vec()).await,
                        Some(Ok(Message::Text(text))) if text.len() <= 256 => {
                            match serde_json::from_str::<Control>(&text) {
                                Ok(Control::Resize { cols, rows }) => self.resize(cols, rows),
                                Err(error) => Err(error.into()),
                            }
                        }
                        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => Ok(()),
                        _ => return,
                    };
                    if let Err(error) = result {
                        let message = serde_json::json!({ "type": "error", "message": error.to_string() }).to_string();
                        send_frame(&mut sender, Message::Text(message.into())).await;
                        return;
                    }
                }
            }
        }
    }
}

async fn send_frame(sender: &mut SplitSink<WebSocket, Message>, message: Message) -> bool {
    tokio::time::timeout(Duration::from_secs(5), sender.send(message))
        .await
        .is_ok_and(|result| result.is_ok())
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Attach the native terminal to a session shared with browser tabs. The caller must leave
/// its alternate screen first; Ctrl-] detaches, and every other byte goes to the PTY.
pub fn attach_local(session: Arc<TerminalSession>) -> Result<()> {
    use std::io::IsTerminal;
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "local attachment requires a terminal"
    );
    let _raw = LocalRawMode::enter()?;
    let mut output = std::io::stdout().lock();
    output.write_all(b"\x1b[0m\x1b[2J\x1b[H")?;
    #[cfg(unix)]
    let input = std::io::stdin();
    attach_loop(
        &session,
        &mut output,
        || {
            #[cfg(unix)]
            {
                read_local_input(&input)
            }
            #[cfg(not(unix))]
            {
                read_local_input()
            }
        },
        || crossterm::terminal::size().ok(),
    )
}

struct LocalRawMode {
    was_raw: bool,
}

impl LocalRawMode {
    fn enter() -> Result<Self> {
        let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
        if !was_raw {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self { was_raw })
    }
}

impl Drop for LocalRawMode {
    fn drop(&mut self) {
        let mut output = std::io::stdout();
        // A detached full-screen application keeps running. Restore the outer terminal's
        // screen, cursor, mouse, focus, paste, and keyboard modes before returning to HQ.
        let _ = output.write_all(b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[<u\x1b[>4;0m\x1b[?25h\x1b[0m");
        let _ = output.flush();
        if !self.was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

enum LocalInput {
    Bytes(Vec<u8>),
    Idle,
    #[cfg(unix)]
    End,
}

fn attach_loop(
    session: &TerminalSession,
    output: &mut impl Write,
    mut read_input: impl FnMut() -> Result<LocalInput>,
    mut terminal_size: impl FnMut() -> Option<(u16, u16)>,
) -> Result<()> {
    let mut cursor = 0;
    let mut size = None;
    loop {
        let snapshot = session.shared.output.lock().unwrap().snapshot(cursor);
        cursor = snapshot.cursor;
        if snapshot.truncated {
            output.write_all(b"\x1bc")?;
        }
        output.write_all(&snapshot.data)?;
        output.flush()?;
        if snapshot.exit.is_some() || session.shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let current_size =
            terminal_size().map(|(cols, rows)| (cols.clamp(1, 500), rows.clamp(1, 300)));
        if current_size != size {
            if let Some((cols, rows)) = current_size {
                session.resize(cols, rows)?;
            }
            size = current_size;
        }
        match read_input()? {
            LocalInput::Bytes(bytes) => {
                let (bytes, detach) = local_input_prefix(&bytes);
                for chunk in bytes.chunks(INPUT_LIMIT) {
                    if let Err(error) = session.input(chunk.to_vec()) {
                        if session.exited() {
                            return Ok(());
                        }
                        return Err(error);
                    }
                }
                if detach {
                    return Ok(());
                }
            }
            LocalInput::Idle => {}
            #[cfg(unix)]
            LocalInput::End => return Ok(()),
        }
    }
}

fn local_input_prefix(bytes: &[u8]) -> (&[u8], bool) {
    match bytes.iter().position(|byte| *byte == 0x1d) {
        Some(offset) => (&bytes[..offset], true),
        None => (bytes, false),
    }
}

#[cfg(unix)]
fn read_local_input(input: &impl std::os::fd::AsFd) -> Result<LocalInput> {
    use nix::poll::{PollFd, PollFlags, poll};
    let mut descriptors = [PollFd::new(input.as_fd(), PollFlags::POLLIN)];
    match poll(&mut descriptors, 20u16) {
        Ok(0) | Err(nix::errno::Errno::EINTR) => return Ok(LocalInput::Idle),
        Err(error) => return Err(error.into()),
        _ => {}
    }
    let events = descriptors[0].revents().unwrap_or_else(PollFlags::empty);
    if events.contains(PollFlags::POLLIN) {
        let mut bytes = vec![0; 8192];
        match nix::unistd::read(input, &mut bytes) {
            Ok(0) => return Ok(LocalInput::End),
            Ok(count) => {
                bytes.truncate(count);
                return Ok(LocalInput::Bytes(bytes));
            }
            Err(nix::errno::Errno::EINTR) => return Ok(LocalInput::Idle),
            Err(error) => return Err(error.into()),
        }
    }
    if events.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
        return Ok(LocalInput::End);
    }
    Ok(LocalInput::Idle)
}

#[cfg(not(unix))]
fn read_local_input() -> Result<LocalInput> {
    use crossterm::event::{self, Event};
    if !event::poll(Duration::from_millis(20))? {
        return Ok(LocalInput::Idle);
    }
    match event::read()? {
        Event::Key(key) => Ok(key_bytes(key).map_or(LocalInput::Idle, LocalInput::Bytes)),
        Event::Paste(paste) => {
            ensure!(paste.len() <= REPLAY_LIMIT, "paste exceeds 1 MiB");
            Ok(LocalInput::Bytes(
                format!("\x1b[200~{paste}\x1b[201~").into_bytes(),
            ))
        }
        _ => Ok(LocalInput::Idle),
    }
}

#[cfg(any(not(unix), test))]
fn key_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let cursor = |letter| {
        if modifier == 1 {
            format!("\x1b[{letter}")
        } else {
            format!("\x1b[1;{modifier}{letter}")
        }
        .into_bytes()
    };
    let numbered = |number| {
        if modifier == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{modifier}~")
        }
        .into_bytes()
    };
    let character = match key.code {
        KeyCode::Char(character) if control => {
            let byte = match character.to_ascii_uppercase() {
                '@' | ' ' | '2' => 0,
                'A'..='Z' => character.to_ascii_uppercase() as u8 - b'@',
                '[' | '3' => 27,
                '\\' | '4' => 28,
                ']' | '5' => 29,
                '^' | '6' => 30,
                '_' | '7' => 31,
                '?' | '8' => 127,
                _ => return None,
            };
            vec![byte]
        }
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab if shift => return Some(b"\x1b[Z".to_vec()),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => return Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => vec![if control { 8 } else { 127 }],
        KeyCode::Esc => vec![27],
        KeyCode::Null => vec![0],
        KeyCode::Up => return Some(cursor('A')),
        KeyCode::Down => return Some(cursor('B')),
        KeyCode::Right => return Some(cursor('C')),
        KeyCode::Left => return Some(cursor('D')),
        KeyCode::Home => return Some(cursor('H')),
        KeyCode::End => return Some(cursor('F')),
        KeyCode::Insert => return Some(numbered(2)),
        KeyCode::Delete => return Some(numbered(3)),
        KeyCode::PageUp => return Some(numbered(5)),
        KeyCode::PageDown => return Some(numbered(6)),
        KeyCode::F(number @ 1..=4) => {
            let letter = char::from(b'P' + number - 1);
            return Some(if modifier == 1 {
                format!("\x1bO{letter}").into_bytes()
            } else {
                cursor(letter)
            });
        }
        KeyCode::F(number @ 5..=12) => {
            return Some(numbered(
                [15, 17, 18, 19, 20, 21, 23, 24][usize::from(number - 5)],
            ));
        }
        _ => return None,
    };
    Some(if alt {
        [vec![27], character].concat()
    } else {
        character
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum Control {
    Resize { cols: u16, rows: u16 },
}

enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

struct Shared {
    output: Mutex<OutputLog>,
    stop: AtomicBool,
    changed: watch::Sender<u64>,
}

impl Shared {
    fn append(&self, bytes: &[u8]) {
        self.output.lock().unwrap().append(bytes);
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    fn finish(&self, code: Option<u32>) {
        self.output.lock().unwrap().exit = Some(code);
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

#[derive(Default)]
struct OutputLog {
    bytes: VecDeque<u8>,
    end: u64,
    exit: Option<Option<u32>>,
}

struct Snapshot {
    data: Vec<u8>,
    cursor: u64,
    truncated: bool,
    exit: Option<Option<u32>>,
}

impl OutputLog {
    fn append(&mut self, bytes: &[u8]) {
        self.end += bytes.len() as u64;
        if bytes.len() >= REPLAY_LIMIT {
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - REPLAY_LIMIT..]);
        } else {
            let remove = (self.bytes.len() + bytes.len()).saturating_sub(REPLAY_LIMIT);
            self.bytes.drain(..remove);
            self.bytes.extend(bytes);
        }
    }

    fn snapshot(&self, cursor: u64) -> Snapshot {
        let start = self.end - self.bytes.len() as u64;
        let offset = cursor.saturating_sub(start).min(self.bytes.len() as u64) as usize;
        Snapshot {
            data: self.bytes.iter().skip(offset).copied().collect(),
            cursor: self.end,
            truncated: cursor < start,
            exit: self.exit,
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(master: &dyn MasterPty) -> Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use std::os::fd::BorrowedFd;
    let raw = master
        .as_raw_fd()
        .context("PTY does not expose a file descriptor")?;
    // The borrowed descriptor cannot outlive master, which remains owned by the worker.
    let fd = unsafe { BorrowedFd::borrow_raw(raw) };
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

fn run_pty(
    master: Box<dyn MasterPty + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    mut reader: Box<dyn Read + Send>,
    mut writer: Box<dyn Write + Send>,
    input: mpsc::Receiver<Input>,
    shared: Arc<Shared>,
) {
    #[cfg(not(unix))]
    let reader = {
        let (send, receive) = mpsc::sync_channel::<Vec<u8>>(16);
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                // ConPTY may write while ClosePseudoConsole runs. Keep draining after
                // the owner drops its receiver so older Windows versions cannot deadlock.
                let _ = send.send(buffer[..count].to_vec());
            }
        });
        receive
    };
    #[cfg(not(unix))]
    let writer = {
        let (send, receive) = mpsc::sync_channel::<Vec<u8>>(2);
        std::thread::spawn(move || {
            for bytes in receive {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        send
    };
    let mut pending: Option<(Vec<u8>, usize)> = None;
    let mut exit = None;
    let mut exited_at = None;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            terminate(&*master, child.as_mut());
            exit = child
                .try_wait()
                .ok()
                .flatten()
                .map(|status| status.exit_code());
            break;
        }
        #[cfg(unix)]
        {
            let mut buffer = [0; 8192];
            for _ in 0..32 {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => shared.append(&buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        #[cfg(not(unix))]
        for bytes in reader.try_iter().take(32) {
            shared.append(&bytes);
        }
        if exited_at.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            exit = Some(status.exit_code());
            exited_at = Some(Instant::now());
        }
        // Drain final output before publishing exit, without waiting indefinitely on descendants.
        if exited_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(100)) {
            // Interactive shells give background jobs their own process groups. Closing only
            // the shell's group would leave those jobs alive after their browser terminal exits.
            terminate(&*master, child.as_mut());
            break;
        }
        for _ in 0..16 {
            if pending.is_none() {
                match input.try_recv() {
                    Ok(Input::Resize(cols, rows)) => {
                        let _ = master.resize(PtySize {
                            cols,
                            rows,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                        continue;
                    }
                    Ok(Input::Bytes(bytes)) => pending = Some((bytes, 0)),
                    Err(_) => break,
                }
            }
            if let Some((bytes, offset)) = &mut pending {
                #[cfg(unix)]
                let written = writer.write(&bytes[*offset..]);
                #[cfg(not(unix))]
                let written: std::io::Result<usize> = match writer
                    .try_send(bytes[*offset..].to_vec())
                {
                    Ok(()) => Ok(bytes.len() - *offset),
                    Err(mpsc::TrySendError::Full(_)) => Err(std::io::ErrorKind::WouldBlock.into()),
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        Err(std::io::ErrorKind::BrokenPipe.into())
                    }
                };
                match written {
                    Ok(count) if count > 0 => {
                        *offset += count;
                        if *offset == bytes.len() {
                            pending = None;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    _ => {
                        pending = None;
                        break;
                    }
                }
            }
        }
        std::thread::sleep(IO_TICK);
    }
    drop(writer);
    drop(reader);
    drop(master);
    shared.finish(exit);
}

fn terminate(master: &dyn MasterPty, child: &mut dyn Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill, killpg};
        use nix::unistd::Pid;
        let groups = [
            child.process_id().map(|pid| pid as i32),
            master.process_group_leader(),
        ];
        for signal in [Signal::SIGHUP, Signal::SIGKILL] {
            if let Some(session) = child.process_id() {
                signal_session(session as i32, signal);
            }
            for group in groups.into_iter().flatten().filter(|pid| *pid > 1) {
                let _ = killpg(Pid::from_raw(group), signal);
            }
            if let Some(pid) = child.process_id() {
                let _ = kill(Pid::from_raw(pid as i32), signal);
            }
            if signal == Signal::SIGHUP {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = master;
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(unix)]
fn signal_session(session: i32, signal: nix::sys::signal::Signal) {
    use nix::sys::signal::kill;
    use nix::unistd::{Pid, getsid};
    // Session membership survives reparenting when the shell exits, unlike a PPID tree.
    // tmux servers create their own sessions and are therefore preserved when clients close.
    if let Ok(output) = Command::new("ps").args(["-A", "-o", "pid="]).output() {
        for pid in String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .filter_map(|pid| pid.parse::<i32>().ok())
            .filter(|pid| *pid > 1)
        {
            let pid = Pid::from_raw(pid);
            if getsid(Some(pid)).is_ok_and(|id| id.as_raw() == session) {
                let _ = kill(pid, signal);
            }
        }
    }
}

fn tmux_name(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    format!(
        "bonsai-{}-{:02x}{:02x}{:02x}{:02x}",
        slug::slugify(name),
        digest[0],
        digest[1],
        digest[2],
        digest[3]
    )
}

fn tmux_socket(environment: &str) -> Option<&str> {
    let (socket_and_pid, _) = environment.rsplit_once(',')?;
    let (socket, _) = socket_and_pid.rsplit_once(',')?;
    (!socket.is_empty()).then_some(socket)
}

fn tmux_command(environment: Option<&str>) -> CommandBuilder {
    let mut command = CommandBuilder::new("tmux");
    if let Some(socket) = environment.and_then(tmux_socket) {
        command.arg("-S");
        command.arg(socket);
    }
    command.env_remove("TMUX");
    command
}

fn new_tmux_command(
    environment: Option<&str>,
    name: &str,
    path: &Path,
    config: &Config,
) -> CommandBuilder {
    let mut command = tmux_command(environment);
    command.args(["new-session", "-A", "-s", name, "-c"]);
    command.arg(path);
    command.arg("-e");
    command.arg(format!("BONSAI_ROOT={}", config.root_dir().display()));
    if let Some(remote) = &config.remote {
        command.arg("-e");
        command.arg(format!("BONSAI_REMOTE={remote}"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        let mut environment = std::ffi::OsString::from("PATH=");
        environment.push(path);
        command.arg("-e");
        command.arg(environment);
    }
    // Leaving shell-command absent preserves tmux's default-shell/default-command and
    // lets it own pane initialization, respawn, and new windows after HQ exits.
    command
}

fn shell_command(executable: &Path, config: &Config) -> Result<(CommandBuilder, Option<TempDir>)> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| {
        #[cfg(unix)]
        {
            "/bin/sh".into()
        }
        #[cfg(not(unix))]
        {
            std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into())
        }
    });
    configured_shell_command(executable, config, shell)
}

fn configured_shell_command(
    executable: &Path,
    config: &Config,
    shell: std::ffi::OsString,
) -> Result<(CommandBuilder, Option<TempDir>)> {
    let name = Path::new(&shell)
        .file_stem()
        .unwrap_or(&shell)
        .to_string_lossy();
    let mut command = CommandBuilder::new(&shell);
    command.env("BONSAI_ROOT", config.root_dir());
    if let Some(remote) = &config.remote {
        command.env("BONSAI_REMOTE", remote);
    }
    let directory = executable
        .parent()
        .context("bonsai executable has no directory")?;
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let paths =
        std::iter::once(directory.to_path_buf()).chain(std::env::split_paths(&inherited_path));
    command.env("PATH", std::env::join_paths(paths)?);
    command.env("_BONSAI_WEB_BIN", directory);
    if !matches!(name.as_ref(), "bash" | "zsh" | "fish") {
        #[cfg(unix)]
        command.args(["-l", "-i"]);
        return Ok((command, None));
    }
    let bootstrap = tempfile::Builder::new()
        .prefix("bonsai-terminal-")
        .tempdir()?;
    command.env("_BONSAI_WEB_BOOT", bootstrap.path());
    match name.as_ref() {
        "zsh" => {
            command.env(
                "_BONSAI_WEB_USER_ZDOTDIR",
                std::env::var_os("ZDOTDIR")
                    .or_else(|| std::env::var_os("HOME"))
                    .unwrap_or_default(),
            );
            command.env("ZDOTDIR", bootstrap.path());
            for file in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
                let mut script = format!(
                    "[[ -r \"$_BONSAI_WEB_USER_ZDOTDIR/{file}\" ]] && source \"$_BONSAI_WEB_USER_ZDOTDIR/{file}\"\nZDOTDIR=\"$_BONSAI_WEB_BOOT\"\n"
                );
                if file == ".zlogin" {
                    script.push_str("ZDOTDIR=\"$_BONSAI_WEB_USER_ZDOTDIR\"\nexport PATH=\"$_BONSAI_WEB_BIN:$PATH\"\n");
                    script.push_str(&crate::shell::init_script(crate::shell::Shell::Zsh));
                }
                std::fs::write(bootstrap.path().join(file), script)?;
            }
            command.args(["-l", "-i"]);
        }
        "bash" => {
            let mut script = String::from(
                "[[ -r /etc/profile ]] && source /etc/profile\nfor bonsai_profile in \"$HOME/.bash_profile\" \"$HOME/.bash_login\" \"$HOME/.profile\"; do\n  if [[ -r \"$bonsai_profile\" ]]; then source \"$bonsai_profile\"; break; fi\ndone\nunset bonsai_profile\n[[ -r \"$HOME/.bashrc\" ]] && source \"$HOME/.bashrc\"\nexport PATH=\"$_BONSAI_WEB_BIN:$PATH\"\n",
            );
            script.push_str(&crate::shell::init_script(crate::shell::Shell::Bash));
            let rc = bootstrap.path().join("bashrc");
            std::fs::write(&rc, script)?;
            command.arg("--rcfile");
            command.arg(rc);
            command.arg("-i");
        }
        "fish" => {
            std::fs::write(
                bootstrap.path().join("fish-init"),
                crate::shell::init_script(crate::shell::Shell::Fish),
            )?;
            command.args([
                "-l",
                "-i",
                "-C",
                "set -gx PATH \"$_BONSAI_WEB_BIN\" $PATH; source \"$_BONSAI_WEB_BOOT/fish-init\"",
            ]);
        }
        _ => bail!("unsupported shell"),
    }
    Ok((command, Some(bootstrap)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_is_bounded_and_subscribers_have_independent_cursors() {
        let mut log = OutputLog::default();
        log.append(b"hello");
        assert_eq!(log.snapshot(0).data, b"hello");
        assert_eq!(log.snapshot(0).data, b"hello");
        let cursor = log.end;
        log.append(b" world");
        assert_eq!(log.snapshot(cursor).data, b" world");
        log.append(&vec![b'x'; REPLAY_LIMIT + 20]);
        let snapshot = log.snapshot(0);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.data.len(), REPLAY_LIMIT);
        assert!(snapshot.data.iter().all(|byte| *byte == b'x'));
    }

    #[cfg(unix)]
    fn shell(manager: &TerminalManager, script: &str) -> Arc<TerminalSession> {
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", script]);
        let summary = manager
            .spawn_command(command, Path::new("/tmp"), "test".into(), "command", None)
            .unwrap();
        manager.get(&summary.id).unwrap()
    }

    #[cfg(unix)]
    fn wait_for(session: &TerminalSession, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = session.shared.output.lock().unwrap().snapshot(0);
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "terminal output: {:?}",
                String::from_utf8_lossy(&snapshot.data)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn pty_input_resize_exit_and_reconnect_preserve_output() {
        let manager = TerminalManager::new();
        let session = shell(
            &manager,
            "stty -echo; printf ready; read answer; stty size; printf 'answer:%s' \"$answer\"; exit 7",
        );
        wait_for(&session, |snapshot| snapshot.data.ends_with(b"ready"));
        session.resize(111, 37).unwrap();
        session.input(b"bonsai\n".to_vec()).unwrap();
        let first = wait_for(&session, |snapshot| snapshot.exit.is_some());
        assert!(String::from_utf8_lossy(&first.data).contains("37 111"));
        assert!(String::from_utf8_lossy(&first.data).contains("answer:bonsai"));
        assert_eq!(first.exit, Some(Some(7)));
        let replay = session.shared.output.lock().unwrap().snapshot(0);
        assert_eq!(first.data, replay.data);
        assert!(session.resize(0, 20).is_err());
        assert!(session.input(vec![0; INPUT_LIMIT + 1]).is_err());
        assert!(manager.close(&session.summary.id));
        assert!(manager.list().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rapid_websocket_input_applies_backpressure_without_losing_bytes() {
        let manager = TerminalManager::new();
        let session = shell(
            &manager,
            "stty -echo; printf ready; IFS= read -r answer; printf 'received:%s' \"$answer\"",
        );
        wait_for(&session, |snapshot| snapshot.data.ends_with(b"ready"));
        let payload = "full-speed-input".repeat(40);
        for byte in payload.bytes().chain(std::iter::once(b'\n')) {
            session.input_async(vec![byte]).await.unwrap();
        }
        let output = wait_for(&session, |snapshot| snapshot.exit.is_some());
        assert!(String::from_utf8_lossy(&output.data).ends_with(&format!("received:{payload}")));
    }

    #[cfg(unix)]
    #[test]
    fn close_kills_a_shell_that_ignores_hangup_and_cleans_worker() {
        let manager = TerminalManager::new();
        let session = shell(
            &manager,
            "trap '' HUP; printf ready; while :; do sleep 1; done",
        );
        wait_for(&session, |snapshot| snapshot.data.ends_with(b"ready"));
        let started = Instant::now();
        assert!(manager.close(&session.summary.id));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(session.worker.lock().unwrap().is_none());
        assert!(session.shared.output.lock().unwrap().exit.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn control_c_interrupts_the_foreground_process() {
        let manager = TerminalManager::new();
        let session = shell(&manager, "printf ready; exec sleep 100");
        wait_for(&session, |snapshot| snapshot.data.ends_with(b"ready"));
        session.input(vec![3]).unwrap();
        wait_for(&session, |snapshot| snapshot.exit.is_some());
    }

    #[test]
    fn native_input_preserves_terminal_bytes_and_reserves_only_control_bracket() {
        assert_eq!(
            local_input_prefix(b"\x03\x1b[A\xc3\xa9"),
            (&b"\x03\x1b[A\xc3\xa9"[..], false)
        );
        assert_eq!(
            local_input_prefix(b"hello\x1dignored"),
            (&b"hello"[..], true)
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_input_poll_reads_bytes_and_stops_at_eof_without_changing_flags() {
        use nix::fcntl::{FcntlArg, fcntl};
        let (read, write) = nix::unistd::pipe().unwrap();
        let flags = fcntl(&read, FcntlArg::F_GETFL).unwrap();
        nix::unistd::write(&write, b"\x03\x1b[A\xc3\xa9\x1d").unwrap();
        let LocalInput::Bytes(bytes) = read_local_input(&read).unwrap() else {
            panic!("expected terminal bytes");
        };
        assert_eq!(bytes, b"\x03\x1b[A\xc3\xa9\x1d");
        assert_eq!(flags, fcntl(&read, FcntlArg::F_GETFL).unwrap());
        drop(write);
        assert!(matches!(read_local_input(&read).unwrap(), LocalInput::End));
    }

    #[test]
    fn windows_key_mapping_preserves_controls_unicode_modifiers_and_release() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let key = |code, modifiers| key_bytes(KeyEvent::new(code, modifiers));
        assert_eq!(
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(vec![3])
        );
        assert_eq!(
            key(KeyCode::Char(']'), KeyModifiers::CONTROL),
            Some(vec![29])
        );
        assert_eq!(
            key(KeyCode::Char('é'), KeyModifiers::ALT),
            Some(b"\x1b\xc3\xa9".to_vec())
        );
        assert_eq!(
            key(KeyCode::Left, KeyModifiers::CONTROL),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            key(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            key(KeyCode::F(5), KeyModifiers::NONE),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            key_bytes(KeyEvent::new_with_kind(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                KeyEventKind::Release
            )),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_attachment_detaches_and_replays_the_same_browser_session() {
        let manager = TerminalManager::new();
        let session = shell(
            &manager,
            "stty -echo; printf ready; read answer; printf 'seen:%s' \"$answer\"; exec sleep 100",
        );
        wait_for(&session, |snapshot| snapshot.data.ends_with(b"ready"));
        let mut output = Vec::new();
        let mut sent = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        attach_loop(
            &session,
            &mut output,
            || {
                assert!(Instant::now() < deadline);
                if !sent {
                    sent = true;
                    return Ok(LocalInput::Bytes(b"bonsai\n".to_vec()));
                }
                if session
                    .shared
                    .output
                    .lock()
                    .unwrap()
                    .snapshot(0)
                    .data
                    .ends_with(b"seen:bonsai")
                {
                    return Ok(LocalInput::Bytes(vec![29]));
                }
                std::thread::sleep(IO_TICK);
                Ok(LocalInput::Idle)
            },
            || Some((80, 24)),
        )
        .unwrap();
        assert!(!session.exited());
        assert!(String::from_utf8_lossy(&output).contains("seen:bonsai"));
        let mut replay = Vec::new();
        attach_loop(
            &session,
            &mut replay,
            || Ok(LocalInput::Bytes(vec![29])),
            || Some((80, 24)),
        )
        .unwrap();
        assert_eq!(output, replay);
        assert_eq!(
            session.shared.output.lock().unwrap().snapshot(0).data,
            replay
        );
    }

    #[cfg(unix)]
    #[test]
    fn manager_drop_closes_owned_terminals() {
        let manager = TerminalManager::new();
        let session = shell(&manager, "printf ready; exec sleep 100");
        wait_for(&session, |snapshot| snapshot.data.ends_with(b"ready"));
        drop(manager);
        assert!(session.shared.output.lock().unwrap().exit.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn normal_shell_exit_terminates_background_jobs_in_other_groups() {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        let manager = TerminalManager::new();
        let session = shell(
            &manager,
            "set -m; (trap '' HUP; exec sleep 100) & printf 'child:%s\\n' \"$!\"; sleep 0.1; exit 0",
        );
        let output = wait_for(&session, |snapshot| snapshot.exit.is_some());
        let output = String::from_utf8_lossy(&output.data);
        let pid = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("child:"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while kill(Pid::from_raw(pid), None).is_ok() {
            assert!(
                Instant::now() < deadline,
                "background process {pid} survived its terminal"
            );
            std::thread::sleep(IO_TICK);
        }
    }

    #[cfg(unix)]
    #[test]
    fn browser_shell_loads_user_config_then_bonsai_directory_integration() {
        use std::os::unix::fs::PermissionsExt;
        for shell_path in ["/bin/bash", "/bin/zsh"] {
            if !Path::new(shell_path).exists() {
                continue;
            }
            let home = tempfile::tempdir().unwrap();
            let target = home.path().join("worktree with spaces");
            std::fs::create_dir(&target).unwrap();
            let executable = home.path().join("bonsai");
            std::fs::write(&executable, "#!/bin/sh\ncase \"$1\" in completions) exit 0;; *) printf '__bonsai_cd\\037%s\\n' \"$_BONSAI_TEST_TARGET\";; esac\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            for file in [".bashrc", ".zshrc"] {
                std::fs::write(
                    home.path().join(file),
                    "export PATH=/usr/bin:/bin\nexport _BONSAI_TEST_CONFIG=loaded\n",
                )
                .unwrap();
            }
            let (mut command, bootstrap) =
                configured_shell_command(&executable, &Config::default(), shell_path.into())
                    .unwrap();
            command.env("HOME", home.path());
            command.env("_BONSAI_WEB_USER_ZDOTDIR", home.path());
            command.env("_BONSAI_TEST_TARGET", &target);
            command.cwd(home.path());
            let manager = TerminalManager::new();
            let summary = manager
                .spawn_command(command, home.path(), "test".into(), "shell", bootstrap)
                .unwrap();
            let session = manager.get(&summary.id).unwrap();
            session.input(b"bonsai cd target; printf '\\nCONFIG=%s CWD=%s\\n' \"$_BONSAI_TEST_CONFIG\" \"$PWD\"; exit\n".to_vec()).unwrap();
            let output = wait_for(&session, |snapshot| snapshot.exit.is_some());
            let output = String::from_utf8_lossy(&output.data);
            assert!(
                output.contains(&format!("CONFIG=loaded CWD={}", target.display())),
                "{shell_path}: {output}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tmux_detach_preserves_session_on_an_isolated_socket() {
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        struct Socket(tempfile::TempDir);
        impl Drop for Socket {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .arg("-S")
                    .arg(self.0.path().join("socket"))
                    .arg("kill-server")
                    .output();
            }
        }
        let socket = Socket(tempfile::tempdir().unwrap());
        let path = socket.0.path().join("socket");
        let status = Command::new("tmux")
            .arg("-S")
            .arg(&path)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "bonsai-test",
                "-c",
            ])
            .arg(socket.0.path())
            .args(["/bin/sh", "-i"])
            .status()
            .unwrap();
        assert!(status.success());
        let inventory = tmux_inventory_with(&["-S", path.to_str().unwrap()]);
        assert_eq!(inventory.sessions.len(), 1);
        assert_eq!(inventory.sessions[0].name, "bonsai-test");
        let manager = TerminalManager::new();
        let inherited_tmux = format!("{},123,0", path.display());
        let mut command = tmux_command(Some(&inherited_tmux));
        command.args(["attach-session", "-t", "=bonsai-test"]);
        command.env("TERM", "xterm-256color");
        command.env_remove("TMUX");
        let summary = manager
            .spawn_command(command, socket.0.path(), "tmux".into(), "tmux", None)
            .unwrap();
        let session = manager.get(&summary.id).unwrap();
        session
            .input(b"printf 'tmux-%s\\n' alive\n".to_vec())
            .unwrap();
        wait_for(&session, |snapshot| {
            String::from_utf8_lossy(&snapshot.data).contains("tmux-alive")
        });
        session
            .input(b"cd /; printf 'moved-%s\\n' yes\n".to_vec())
            .unwrap();
        wait_for(&session, |snapshot| {
            String::from_utf8_lossy(&snapshot.data).contains("moved-yes")
        });
        let inventory = tmux_inventory_with(&["-S", path.to_str().unwrap()]);
        assert_eq!(inventory.sessions[0].path, "/");
        manager.close(&summary.id);
        assert!(
            Command::new("tmux")
                .arg("-S")
                .arg(&path)
                .args(["has-session", "-t", "=bonsai-test"])
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn tmux_inventory_preserves_paths_with_spaces_and_matches_exact_names() {
        let sessions =
            parse_tmux_sessions("feature\t2\t1\t/tmp/a project\nfeature-long\t1\t0\t/tmp/b\n");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "feature");
        assert_eq!(sessions[0].path, "/tmp/a project");
        assert!(sessions[0].attached);
    }

    #[test]
    fn pane_inventory_preserves_window_names_and_paths_with_tabs_and_newlines() {
        let panes = parse_tmux_panes(
            "%4\u{1f}project\u{1f}@2\u{1f}3\u{1f}editor\twindow\nname\u{1f}/tmp/a\tb\nc\u{1f}vim\u{1f}0\u{1f}1\u{1e}\n%5\u{1f}other\u{1f}@3\u{1f}0\u{1f}shell\u{1f}/tmp/other\u{1f}zsh\u{1f}1\u{1f}0\u{1e}\n",
        );
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].id, "%4");
        assert_eq!(panes[0].window_id, "@2");
        assert_eq!(panes[0].window_name, "editor\twindow\nname");
        assert_eq!(panes[0].path, "/tmp/a\tb\nc");
        assert!(!panes[0].active);
        assert!(panes[0].window_active);
        assert_eq!(panes[1].session, "other");
        assert!(!panes[1].window_active);
    }

    #[test]
    fn exact_tmux_targets_preserve_the_selected_session_window_and_pane() {
        let inventory = TmuxInventory {
            available: true,
            sessions: parse_tmux_sessions("project\t2\t1\t/tmp\nproject-long\t1\t0\t/tmp\n"),
            panes: parse_tmux_panes(
                "%4\u{1f}project\u{1f}@2\u{1f}3\u{1f}worker\u{1f}/tmp\u{1f}node\u{1f}0\u{1f}0\u{1f}1234\u{1f}1788990000\u{1e}\n",
            ),
        };
        assert_eq!(inventory.panes[0].pid, Some(1234));
        assert_eq!(inventory.panes[0].last_activity, Some(1788990000));
        assert_eq!(
            tmux_attach_target(&inventory, "project", Some("%4")).unwrap(),
            "=project:@2.%4"
        );
        assert!(tmux_attach_target(&inventory, "project-long", Some("%4")).is_err());
        assert!(tmux_attach_target(&inventory, "project", Some("%5")).is_err());
        assert!(tmux_attach_target(&inventory, "proj", None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn exact_pane_attachment_selects_an_inactive_window_without_retargeting() {
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        struct Socket(tempfile::TempDir);
        impl Drop for Socket {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .arg("-S")
                    .arg(self.0.path().join("socket"))
                    .arg("kill-server")
                    .output();
            }
        }
        let socket = Socket(tempfile::tempdir().unwrap());
        let path = socket.0.path().join("socket");
        let tmux = |args: &[&str]| {
            let output = Command::new("tmux")
                .arg("-S")
                .arg(&path)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "exact",
            "/bin/sh",
            "-i",
        ]);
        let pane = tmux(&[
            "new-window",
            "-d",
            "-t",
            "=exact",
            "-P",
            "-F",
            "#{pane_id}",
            "/bin/sh",
            "-i",
        ]);
        let other = tmux(&[
            "split-window",
            "-d",
            "-t",
            &pane,
            "-P",
            "-F",
            "#{pane_id}",
            "/bin/sh",
            "-i",
        ]);
        let inventory = tmux_inventory_with(&["-S", path.to_str().unwrap()]);
        assert!(
            !inventory
                .panes
                .iter()
                .find(|entry| entry.id == other)
                .unwrap()
                .window_active
        );
        let target = tmux_attach_target(&inventory, "exact", Some(&other)).unwrap();
        let inherited = format!("{},123,0", path.display());
        let mut command = tmux_command(Some(&inherited));
        command.args(["attach-session", "-t", &target]);
        command.env("TERM", "xterm-256color");
        let manager = TerminalManager::new();
        let summary = manager
            .spawn_command(command, socket.0.path(), "tmux".into(), "tmux", None)
            .unwrap();
        let session = manager.get(&summary.id).unwrap();
        session
            .input(b"printf 'EXACT_%s\\n' PANE\n".to_vec())
            .unwrap();
        wait_for(&session, |snapshot| {
            String::from_utf8_lossy(&snapshot.data).contains("EXACT_PANE")
        });
        let focused = tmux_inventory_with(&["-S", path.to_str().unwrap()]);
        assert_eq!(
            focused
                .panes
                .iter()
                .find(|pane| pane.active && pane.window_active)
                .map(|pane| pane.id.as_str()),
            Some(other.as_str())
        );
        let other_content = tmux(&["capture-pane", "-p", "-t", &pane]);
        assert!(!other_content.contains("EXACT_PANE"));
        manager.close(&summary.id);
        tmux(&["has-session", "-t", "=exact"]);
        tmux(&["kill-pane", "-t", &other]);
        let inventory = tmux_inventory_with(&["-S", path.to_str().unwrap()]);
        assert!(tmux_attach_target(&inventory, "exact", Some(&other)).is_err());
    }

    #[test]
    fn tmux_connection_preserves_custom_socket_when_clearing_nested_session_marker() {
        assert_eq!(
            tmux_socket("/tmp/custom socket,123,0"),
            Some("/tmp/custom socket")
        );
        assert_eq!(
            tmux_socket("/tmp/socket,with,commas,123,0"),
            Some("/tmp/socket,with,commas")
        );
        assert_eq!(tmux_socket("invalid"), None);
        assert_eq!(tmux_socket(",123,0"), None);
    }

    #[test]
    fn shell_internal_environment_does_not_become_bonsai_configuration() {
        let config = Config::default();
        let (command, _bootstrap) =
            configured_shell_command(&std::env::current_exe().unwrap(), &config, "bash".into())
                .unwrap();
        let overlay: serde_json::Map<String, serde_json::Value> = command
            .iter_extra_env_as_str()
            .filter_map(|(name, value)| {
                name.strip_prefix("BONSAI_")
                    .map(|name| (name.to_lowercase(), value.into()))
            })
            .collect();
        let parsed = figment::Figment::from(figment::providers::Serialized::defaults(config))
            .merge(figment::providers::Serialized::defaults(overlay))
            .extract::<Config>();
        assert!(
            parsed.is_ok(),
            "internal terminal variables polluted configuration: {parsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_tmux_startup_survives_hq_close_respawn_and_new_windows() {
        use std::os::unix::fs::PermissionsExt;
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        struct Socket(tempfile::TempDir);
        impl Drop for Socket {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .arg("-S")
                    .arg(self.0.path().join("socket"))
                    .arg("kill-server")
                    .output();
            }
        }
        let socket = Socket(tempfile::tempdir().unwrap());
        let socket_path = socket.0.path().join("socket");
        let worktree = socket.0.path().join("native worktree");
        std::fs::create_dir(&worktree).unwrap();
        let worktree = std::fs::canonicalize(worktree).unwrap();
        let config = Config {
            root: socket.0.path().join("root").to_string_lossy().into_owned(),
            remote: Some("upstream".into()),
            ..Config::default()
        };
        std::fs::create_dir_all(config.root_dir()).unwrap();
        let startup = socket.0.path().join("user-startup");
        std::fs::write(&startup, "#!/bin/sh\nexport _BONSAI_NATIVE_INIT=loaded\nprintf 'USER_STARTUP\\n'\nexec /bin/sh -i\n").unwrap();
        std::fs::set_permissions(&startup, std::fs::Permissions::from_mode(0o700)).unwrap();
        let run = |args: &[&str]| {
            let output = Command::new("tmux")
                .current_dir(&worktree)
                .arg("-S")
                .arg(&socket_path)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "existing",
            "/bin/sh",
            "-i",
        ]);
        let default_command = format!("'{}'", startup.to_string_lossy().replace('\'', "'\\''"));
        run(&["set-option", "-g", "default-shell", "/bin/sh"]);
        run(&["set-option", "-g", "default-command", &default_command]);
        let mut command = new_tmux_command(
            Some(&format!("{},123,0", socket_path.display())),
            "managed",
            &worktree,
            &config,
        );
        command.cwd(&worktree);
        command.env("TERM", "xterm-256color");
        let manager = TerminalManager::new();
        let summary = manager
            .spawn_command(command, &worktree, "tmux".into(), "tmux", None)
            .unwrap();
        let session = manager.get(&summary.id).unwrap();
        wait_for(&session, |snapshot| {
            String::from_utf8_lossy(&snapshot.data).contains("USER_STARTUP")
        });
        manager.close(&summary.id);
        drop(manager);
        assert_eq!(
            run(&["show-options", "-gv", "default-command"]).trim(),
            default_command
        );
        assert_eq!(
            run(&["show-options", "-gv", "default-shell"]).trim(),
            "/bin/sh"
        );
        let verify = |pane: &str| {
            run(&[
                "send-keys",
                "-t",
                pane,
                "-l",
                "printf 'INIT=%s REMOTE=%s\\n' \"$_BONSAI_NATIVE_INIT\" \"$BONSAI_REMOTE\"; test -d \"$BONSAI_ROOT\" && printf 'ROOT_%s\\n' OK",
            ]);
            run(&["send-keys", "-t", pane, "Enter"]);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let output = run(&["capture-pane", "-p", "-t", pane]);
                if output.contains("USER_STARTUP")
                    && output.contains("INIT=loaded REMOTE=upstream")
                    && output.contains("ROOT_OK")
                {
                    break;
                }
                assert!(Instant::now() < deadline, "{pane}: {output}");
                std::thread::sleep(IO_TICK);
            }
            assert_eq!(
                run(&["display-message", "-p", "-t", pane, "#{pane_current_path}"]).trim(),
                worktree.to_string_lossy(),
                "{pane} started outside the worktree"
            );
        };
        run(&["respawn-pane", "-k", "-t", "managed:0.0"]);
        verify("managed:0.0");
        run(&[
            "new-window",
            "-d",
            "-t",
            "managed:",
            "-n",
            "fresh",
            "-c",
            worktree.to_str().unwrap(),
        ]);
        verify("managed:fresh.0");
        let nested = worktree.join("nested");
        std::fs::create_dir(&nested).unwrap();
        run(&[
            "split-window",
            "-d",
            "-t",
            "managed:0.0",
            "-c",
            nested.to_str().unwrap(),
        ]);
        let inventory = tmux_inventory_with(&["-S", socket_path.to_str().unwrap()]);
        assert_eq!(inventory.sessions.len(), 2);
        assert_eq!(inventory.panes.len(), 4);
        let nested_pane = inventory
            .panes
            .iter()
            .find(|pane| Path::new(&pane.path) == nested)
            .unwrap();
        assert_eq!(nested_pane.session, "managed");
        assert!(nested_pane.id.starts_with('%'));
        assert!(nested_pane.window_id.starts_with('@'));
        assert!(!nested_pane.active);
        assert!(nested_pane.window_active);
        assert!(
            inventory
                .panes
                .iter()
                .any(|pane| pane.window_name == "fresh" && !pane.window_active)
        );
        assert!(
            inventory
                .panes
                .iter()
                .any(|pane| pane.session == "existing")
        );
        assert!(
            std::fs::read_dir(config.root_dir())
                .unwrap()
                .next()
                .is_none()
        );
    }
}
