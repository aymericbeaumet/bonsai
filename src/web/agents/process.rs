use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

struct OwnedCommand(Child);

impl Drop for OwnedCommand {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            // Descendants may still own stdout after the direct child exits.
            let _ = killpg(Pid::from_raw(self.0.id() as i32), Signal::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
pub(crate) fn command_output(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    use std::io::ErrorKind;
    use std::os::unix::process::CommandExt;

    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let started = Instant::now();
    let mut child = OwnedCommand(
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start provider command")?,
    );
    let mut stdout = child.0.stdout.take().context("missing command output")?;
    let flags = OFlag::from_bits_truncate(fcntl(&stdout, FcntlArg::F_GETFL)?);
    fcntl(&stdout, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;

    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut eof = false;
    let mut status = None;
    loop {
        ensure!(started.elapsed() < timeout, "provider command timed out");
        let mut received = false;
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => {
                    ensure!(
                        bytes.len() + count <= OUTPUT_LIMIT,
                        "provider output exceeded four MiB"
                    );
                    bytes.extend_from_slice(&buffer[..count]);
                    received = true;
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(error) => return Err(error).context("read provider output"),
            }
        }
        if status.is_none() {
            status = child.0.try_wait().context("wait for provider command")?;
        }
        if let Some(status) = status {
            ensure!(status.success(), "provider command exited with {status}");
            if eof {
                return Ok(bytes);
            }
        }
        if !received {
            wait_tick(started, timeout)?;
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn command_output(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};

    // Regular-file reads do not wait for EOF from inherited pipe handles.
    let mut output = tempfile::tempfile().context("create provider output capture")?;
    let started = Instant::now();
    let mut child = OwnedCommand(
        command
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(Stdio::null())
            .spawn()
            .context("start provider command")?,
    );
    loop {
        ensure!(
            output.metadata()?.len() <= OUTPUT_LIMIT as u64,
            "provider output exceeded four MiB"
        );
        if let Some(status) = child.0.try_wait().context("wait for provider command")? {
            ensure!(status.success(), "provider command exited with {status}");
            output.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            output
                .take(OUTPUT_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= OUTPUT_LIMIT,
                "provider output exceeded four MiB"
            );
            return Ok(bytes);
        }
        wait_tick(started, timeout)?;
    }
}

fn wait_tick(started: Instant, timeout: Duration) -> Result<()> {
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        bail!("provider command timed out");
    }
    std::thread::sleep(POLL_INTERVAL.min(remaining));
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    fn assert_stopped(pid_file: &Path) {
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let output = Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&output.stdout);
            let status = status.trim();
            if status.is_empty() || status.starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "process {pid} survived cleanup: {status}"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn collects_output_and_rejects_unsuccessful_commands() {
        assert_eq!(
            command_output(
                &mut shell("printf 'provider output\\n'"),
                Duration::from_secs(2)
            )
            .unwrap(),
            b"provider output\n"
        );
        assert!(
            command_output(&mut shell("exit 7"), Duration::from_secs(2))
                .unwrap_err()
                .to_string()
                .contains("exited with")
        );
    }

    #[test]
    fn permits_exactly_four_mib_and_rejects_more() {
        let output = command_output(
            &mut shell("head -c 4194304 /dev/zero"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.len(), OUTPUT_LIMIT);
        assert!(
            command_output(
                &mut shell("head -c 4194305 /dev/zero"),
                Duration::from_secs(5)
            )
            .unwrap_err()
            .to_string()
            .contains("exceeded four MiB")
        );
    }

    #[test]
    fn timeout_kills_and_reaps_the_direct_child() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut command = shell("printf '%s' \"$$\" > \"$1\"; exec sleep 30");
        command.arg("provider").arg(&pid_file);
        let started = Instant::now();
        assert!(
            command_output(&mut command, Duration::from_millis(200))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_stopped(&pid_file);
    }

    #[test]
    fn timeout_covers_descendant_stdout_after_parent_exit_and_kills_the_group() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut command = shell("sleep 30 & printf '%s' \"$!\" > \"$1\"; exit 0");
        command.arg("provider").arg(&pid_file);
        let started = Instant::now();
        assert!(
            command_output(&mut command, Duration::from_millis(200))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_stopped(&pid_file);
    }

    #[test]
    fn excessive_output_also_stops_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut command =
            shell("sleep 30 & printf '%s' \"$!\" > \"$1\"; head -c 4194305 /dev/zero; wait");
        command.arg("provider").arg(&pid_file);
        assert!(
            command_output(&mut command, Duration::from_secs(5))
                .unwrap_err()
                .to_string()
                .contains("exceeded four MiB")
        );
        assert_stopped(&pid_file);
    }
}
