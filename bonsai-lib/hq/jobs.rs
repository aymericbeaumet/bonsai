//! Commands started from headquarters.
//!
//! Bonsai's own subcommands run as child processes of the current executable,
//! so their output streams into the output pane while the list stays usable.
//! Coding agents, editors, and prompts that need the terminal run in the
//! foreground while the interface is suspended.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Lines kept in the output pane.
const KEPT_LINES: usize = 400;

/// How long output may keep arriving after the command exits, from a
/// descendant that still holds its pipes, before the exit is reported.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// What the session does once a background command exits successfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum After {
    /// Reload the inventory.
    Refresh,
    /// Reload, then select the path the command printed last (`bonsai add`).
    SelectPrinted,
    /// Show the `bonsai clean --dry-run --json` plan for confirmation.
    CleanPlan {
        project: PathBuf,
        targets: Vec<PathBuf>,
    },
    /// Run the clean only if a fresh plan removes exactly these checkouts.
    CleanVerify {
        project: PathBuf,
        expected: Vec<PathBuf>,
    },
    /// Show the `bonsai trim --dry-run --json` plan for these checkouts
    /// (every worktree of the project when empty) for confirmation.
    TrimPlan {
        project: PathBuf,
        targets: Vec<PathBuf>,
    },
    PrunePlan {
        project: PathBuf,
        targets: Vec<PathBuf>,
    },
}

#[derive(Clone, Debug)]
pub struct Spec {
    /// Shown as the pane title, e.g. `bonsai add feat-x`.
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub dir: PathBuf,
    pub after: After,
    pub projects: Vec<PathBuf>,
    /// Original preview checked by the CLI before applying a confirmed action.
    pub reviewed: Option<String>,
}

impl Spec {
    /// `bonsai -C <dir> <flags…> <args…>` with the running executable.
    /// `flags` carries the global options hq itself was started with.
    pub fn bonsai(dir: &Path, flags: &[OsString], args: &[&str], after: After) -> Result<Self> {
        let program = std::env::current_exe().context("cannot locate the bonsai executable")?;
        let mut full: Vec<OsString> = vec!["-C".into(), dir.as_os_str().to_owned()];
        full.extend(flags.iter().cloned());
        full.extend(args.iter().map(OsString::from));
        Ok(Self {
            label: format!("bonsai {}", args.join(" ")),
            program,
            args: full,
            dir: dir.to_path_buf(),
            after,
            projects: Vec::new(),
            reviewed: None,
        })
    }

    pub fn git(dir: &Path, args: &[&str], after: After) -> Self {
        Self {
            label: format!("git {}", args.join(" ")),
            program: PathBuf::from("git"),
            args: args.iter().map(OsString::from).collect(),
            dir: dir.to_path_buf(),
            after,
            projects: Vec::new(),
            reviewed: None,
        }
    }

    fn command(&self) -> Result<Command> {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(&self.dir);
        if !self.projects.is_empty() {
            command.env("_BONSAI_PROJECTS", serde_json::to_string(&self.projects)?);
        } else {
            command.env_remove("_BONSAI_PROJECTS");
        }
        scrub_wrapper_env(&mut command);
        Ok(command)
    }
}

/// Children must not believe they run under the shell wrapper: they would
/// print cd sentinels or wrapper warnings into the pane.
fn scrub_wrapper_env(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("_BONSAI_WRAPPE") {
            command.env_remove(key);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Succeeded,
    Failed(String),
}

/// Output of the latest background command, shown in the output pane.
pub struct Pane {
    pub label: String,
    pub lines: VecDeque<String>,
    pub state: State,
    /// The last line is a carriage-return progress line that the next one
    /// replaces, as a terminal would.
    progress: bool,
}

impl Pane {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            lines: VecDeque::new(),
            state: State::Running,
            progress: false,
        }
    }

    pub fn push(&mut self, chunk: Chunk) {
        if self.progress {
            self.lines.pop_back();
        }
        self.progress = chunk.progress;
        if chunk.text.is_empty() && chunk.progress {
            return;
        }
        self.lines.push_back(chunk.text);
        while self.lines.len() > KEPT_LINES {
            self.lines.pop_front();
        }
    }

    pub fn running(&self) -> bool {
        self.state == State::Running
    }
}

/// One line of child output. `progress` lines ended with a carriage return.
#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub progress: bool,
    pub stdout: bool,
}

pub struct Background {
    pub spec: Spec,
    child: Child,
    chunks: Receiver<Chunk>,
    /// Complete stdout lines, for commands whose result is parsed.
    pub stdout: Vec<String>,
    readers: usize,
    exited: Option<(ExitStatus, Instant)>,
    _reviewed: Option<tempfile::NamedTempFile>,
}

pub enum Poll {
    Chunk(Chunk),
    Pending,
    Exited(ExitStatus),
}

impl Background {
    pub fn start(spec: Spec) -> Result<Self> {
        let mut command = spec.command()?;
        let reviewed = spec
            .reviewed
            .as_ref()
            .map(|report| -> Result<_> {
                let mut file =
                    tempfile::NamedTempFile::new().context("cannot store the reviewed plan")?;
                file.write_all(report.as_bytes())?;
                file.flush()?;
                command.arg("--reviewed-plan").arg(file.path());
                Ok(file)
            })
            .transpose()?;
        detach(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("_BONSAI_PROGRESS", "1")
            .spawn()
            .with_context(|| format!("cannot run {}", spec.label))?;
        let (sender, chunks) = std::sync::mpsc::channel();
        let mut readers = 0;
        if let Some(stdout) = child.stdout.take() {
            readers += 1;
            spawn_reader(stdout, true, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            readers += 1;
            spawn_reader(stderr, false, sender);
        }
        Ok(Self {
            spec,
            child,
            chunks,
            stdout: Vec::new(),
            readers,
            exited: None,
            _reviewed: reviewed,
        })
    }

    /// Next output chunk, or the exit status once both streams are drained.
    pub fn poll(&mut self) -> Result<Poll> {
        match self.chunks.try_recv() {
            Ok(chunk) => {
                if chunk.stdout && !chunk.progress {
                    self.stdout.push(chunk.text.clone());
                }
                return Ok(Poll::Chunk(chunk));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.readers = 0,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if self.exited.is_none()
            && let Some(status) = self
                .child
                .try_wait()
                .context("cannot wait for the command")?
        {
            self.exited = Some((status, Instant::now()));
        }
        // A descendant may keep the pipes open after the command exits.
        match self.exited {
            Some((status, _)) if self.readers == 0 => Ok(Poll::Exited(status)),
            Some((status, at)) if at.elapsed() >= DRAIN_GRACE => Ok(Poll::Exited(status)),
            _ => Ok(Poll::Pending),
        }
    }

    /// Ask the command and its descendants to stop the way Ctrl-C would, so
    /// they can clean up.
    pub fn interrupt(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // SAFETY: plain signal delivery to the process group the child
            // leads (see `detach`).
            unsafe {
                libc::kill(-pid, libc::SIGINT);
            }
            return;
        }
        let _ = self.child.kill();
    }

    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("cannot wait for the command")
    }
}

/// Background commands get their own session, without a controlling
/// terminal: prompts from ssh, credential helpers, or hooks fail instead of
/// reading keys meant for hq.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) {}

/// While a foreground program runs, Ctrl-C and Ctrl-\\ belong to it; hq
/// keeps waiting instead of dying. Handlers, unlike ignored signals, reset
/// when the program starts, so it still receives them normally.
pub struct HoldInterrupts {
    #[cfg(unix)]
    registrations: Vec<signal_hook::SigId>,
}

impl HoldInterrupts {
    pub fn new() -> Self {
        #[cfg(unix)]
        {
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let registrations = [libc::SIGINT, libc::SIGQUIT]
                .into_iter()
                .filter_map(|signal| {
                    signal_hook::flag::register(signal, std::sync::Arc::clone(&flag)).ok()
                })
                .collect();
            Self { registrations }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }
}

impl Drop for HoldInterrupts {
    fn drop(&mut self) {
        #[cfg(unix)]
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

fn spawn_reader(mut stream: impl Read + Send + 'static, stdout: bool, sender: Sender<Chunk>) {
    std::thread::spawn(move || {
        let mut splitter = Splitter::default();
        let mut buffer = [0u8; 8192];
        loop {
            let read = match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            for (text, progress) in splitter.feed(&buffer[..read]) {
                let chunk = Chunk {
                    text,
                    progress,
                    stdout,
                };
                if sender.send(chunk).is_err() {
                    return;
                }
            }
        }
        if let Some(text) = splitter.finish() {
            let _ = sender.send(Chunk {
                text,
                progress: false,
                stdout,
            });
        }
    });
}

/// Splits a byte stream into lines at `\n` (complete) and `\r` (progress),
/// dropping terminal escape sequences. A `\r\n` pair is one line ending,
/// even when a read splits it.
#[derive(Default)]
struct Splitter {
    pending: Vec<u8>,
    carriage_return: bool,
}

impl Splitter {
    fn feed(&mut self, bytes: &[u8]) -> Vec<(String, bool)> {
        let mut lines = Vec::new();
        for &byte in bytes {
            if std::mem::take(&mut self.carriage_return) {
                if byte == b'\n' {
                    lines.push((take_line(&mut self.pending), false));
                    continue;
                }
                lines.push((take_line(&mut self.pending), true));
            }
            match byte {
                b'\n' => lines.push((take_line(&mut self.pending), false)),
                b'\r' => self.carriage_return = true,
                _ => self.pending.push(byte),
            }
        }
        lines
    }

    fn finish(&mut self) -> Option<String> {
        let ended = std::mem::take(&mut self.carriage_return);
        (ended || !self.pending.is_empty()).then(|| take_line(&mut self.pending))
    }
}

fn take_line(pending: &mut Vec<u8>) -> String {
    let text = strip_escapes(&String::from_utf8_lossy(pending));
    pending.clear();
    text
}

/// Remove ANSI control sequences (CSI, OSC) and other control characters.
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if ch == '\t' {
            out.push_str("    ");
        } else if !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

/// Run a program on the terminal and wait for it. The caller suspends the
/// interface first. The program gets the terminal even when the shell wrapper
/// captures bonsai's stdout.
pub fn foreground(program: &Path, args: &[OsString], dir: &Path) -> Result<ExitStatus> {
    let mut command = Command::new(program);
    command.args(args).current_dir(dir);
    scrub_wrapper_env(&mut command);
    attach_terminal(&mut command);
    command
        .status()
        .with_context(|| format!("cannot run {}", program.display()))
}

#[cfg(unix)]
fn attach_terminal(command: &mut Command) {
    let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    else {
        return;
    };
    let (Ok(input), Ok(output)) = (tty.try_clone(), tty.try_clone()) else {
        return;
    };
    command
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(tty));
}

#[cfg(not(unix))]
fn attach_terminal(_command: &mut Command) {}

/// `$VISUAL`, then `$EDITOR`, split like a shell would.
pub fn editor() -> Option<(PathBuf, Vec<OsString>)> {
    ["VISUAL", "EDITOR"].into_iter().find_map(|name| {
        let value = std::env::var(name).ok()?;
        let mut words = shlex::split(&value)?.into_iter();
        let program = PathBuf::from(words.next()?);
        Some((program, words.map(OsString::from).collect()))
    })
}

/// Copy text to the system clipboard. Falls back to the OSC 52 escape, which
/// most terminals forward to the clipboard.
pub fn copy(text: &str) -> &'static str {
    const TOOLS: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("clip.exe", &[]),
    ];
    for (tool, args) in TOOLS {
        let Ok(mut child) = Command::new(tool)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        if written && child.wait().is_ok_and(|status| status.success()) {
            return "clipboard";
        }
    }
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "\u{1b}]52;c;{}\u{7}", base64(text.as_bytes()));
    let _ = stderr.flush();
    "terminal clipboard"
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(TABLE[((n >> (18 - 6 * index)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_separates_lines_progress_and_escapes() {
        let mut splitter = Splitter::default();
        let lines = splitter.feed(b"\x1b[32mok\x1b[0m\nReceiving 10%\rReceiving 100%\r\ndone");
        assert_eq!(
            lines,
            vec![
                ("ok".to_string(), false),
                ("Receiving 10%".to_string(), true),
                ("Receiving 100%".to_string(), false),
            ]
        );
        assert_eq!(splitter.finish().as_deref(), Some("done"));
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn a_crlf_split_across_reads_is_one_line_ending() {
        let mut splitter = Splitter::default();
        assert!(splitter.feed(b"first\r").is_empty());
        assert_eq!(
            splitter.feed(b"\nnext\n"),
            vec![("first".to_string(), false), ("next".to_string(), false)]
        );
    }

    #[test]
    fn progress_lines_replace_each_other_in_the_pane() {
        let mut pane = Pane::new("bonsai add x");
        for (text, progress) in [("a", false), ("50%", true), ("90%", true), ("b", false)] {
            pane.push(Chunk {
                text: text.to_string(),
                progress,
                stdout: false,
            });
        }
        assert_eq!(pane.lines, ["a", "b"]);
    }

    #[test]
    fn osc_sequences_and_controls_are_dropped() {
        assert_eq!(
            strip_escapes("a\u{1b}]0;title\u{7}b\u{1b}]8;;x\u{1b}\\c\u{8}"),
            "abc"
        );
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"/Users/ab"), "L1VzZXJzL2Fi");
    }

    #[test]
    fn background_commands_stream_output_and_report_their_exit() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::git(dir.path(), &["--version"], After::Refresh);
        let mut job = Background::start(spec).unwrap();
        let mut lines = Vec::new();
        let status = loop {
            match job.poll().unwrap() {
                Poll::Chunk(chunk) => lines.push(chunk.text),
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(5)),
                Poll::Exited(status) => break status,
            }
        };
        assert!(status.success());
        assert!(lines.iter().any(|line| line.starts_with("git version")));
        assert_eq!(job.stdout, lines);
    }

    #[cfg(unix)]
    #[test]
    fn reviewed_plan_and_project_context_live_until_the_child_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = Spec::git(dir.path(), &[], After::Refresh);
        spec.program = "/bin/sh".into();
        spec.args = vec![
            "-c".into(),
            "cat \"$1\"; printf '\\n%s\\n' \"$_BONSAI_PROJECTS\"".into(),
        ];
        spec.projects = vec![dir.path().to_path_buf()];
        spec.reviewed = Some("{\"planned\":[]}".into());
        let mut job = Background::start(spec).unwrap();
        let manifest = job._reviewed.as_ref().unwrap().path().to_path_buf();
        assert!(manifest.exists());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "child did not finish");
            match job.poll().unwrap() {
                Poll::Exited(status) => {
                    assert!(status.success());
                    break;
                }
                Poll::Pending => thread_sleep(),
                Poll::Chunk(_) => {}
            }
        }
        assert_eq!(job.stdout[0], "{\"planned\":[]}");
        let roots: Vec<PathBuf> = serde_json::from_str(job.stdout.last().unwrap()).unwrap();
        assert_eq!(roots, [dir.path()]);
        drop(job);
        assert!(!manifest.exists());

        fn thread_sleep() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
