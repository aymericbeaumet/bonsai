use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[derive(Debug, thiserror::Error)]
#[error("provisioning interrupted by signal {0}; the worktree has been kept")]
pub struct Interrupted(pub i32);

pub struct Cancellation {
    signal: Arc<AtomicUsize>,
    #[cfg(unix)]
    registrations: Vec<signal_hook::SigId>,
}

impl Cancellation {
    pub fn new() -> Result<Self> {
        #[allow(unused_mut)]
        let mut cancellation = Self {
            signal: Arc::new(AtomicUsize::new(0)),
            #[cfg(unix)]
            registrations: Vec::new(),
        };
        #[cfg(unix)]
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            cancellation
                .registrations
                .push(signal_hook::flag::register_usize(
                    signal,
                    Arc::clone(&cancellation.signal),
                    signal as usize,
                )?);
        }
        Ok(cancellation)
    }

    pub fn check(&self) -> Result<()> {
        let signal = self.signal.load(Ordering::Relaxed);
        if signal != 0 {
            return Err(Interrupted(signal as i32).into());
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for Cancellation {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

/// Stream both pipes with fixed-size buffers. Installers never consume the
/// caller's stdin, which may belong to a shell substitution or another protocol.
pub fn run(command: &mut Command, label: &str, cancellation: &Cancellation) -> Result<ExitStatus> {
    cancellation.check()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not start {label}"))?;
    let stdout = child.stdout.take().context("missing child stdout")?;
    let stderr = child.stderr.take().context("missing child stderr")?;
    let streams_done = AtomicUsize::new(0);
    let result = std::thread::scope(|scope| -> Result<ExitStatus> {
        let done = &streams_done;
        scope.spawn(move || {
            forward(stdout, label);
            done.fetch_add(1, Ordering::Release);
        });
        scope.spawn(move || {
            forward(stderr, label);
            done.fetch_add(1, Ordering::Release);
        });
        let mut interrupted_at = None;
        let mut status = None;
        loop {
            let signal = cancellation.signal.load(Ordering::Relaxed) as i32;
            if signal != 0 && interrupted_at.is_none() {
                #[cfg(unix)]
                // The child owns a new process group; include its install scripts.
                unsafe {
                    libc::kill(-(child.id() as i32), signal);
                }
                #[cfg(not(unix))]
                let _ = child.kill();
                interrupted_at = Some(Instant::now());
            }
            if interrupted_at.is_some_and(|start| start.elapsed() >= Duration::from_secs(1)) {
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(found) => status = found,
                    Err(error) => {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error.into());
                    }
                }
            }
            // Descendants can retain pipes after their parent exits. Continue
            // servicing cancellation until both readers have reached EOF.
            if let Some(status) = status
                && streams_done.load(Ordering::Acquire) == 2
            {
                if interrupted_at.is_some() {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    cancellation.check()?;
    result
}

fn forward(mut input: impl Read, label: &str) {
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::with_capacity(buffer.len());
    loop {
        let count = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for byte in &buffer[..count] {
            pending.push(*byte);
            if *byte == b'\n' || pending.len() == buffer.len() {
                emit(label, &pending);
                pending.clear();
            }
        }
    }
    if !pending.is_empty() {
        emit(label, &pending);
    }
}

fn emit(label: &str, bytes: &[u8]) {
    let mut stderr = std::io::stderr().lock();
    let _ = write!(stderr, "  [{label}] ");
    let _ = stderr.write_all(bytes);
    if !bytes.ends_with(b"\n") {
        let _ = stderr.write_all(b"\n");
    }
    let _ = stderr.flush();
}
