use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, thiserror::Error)]
#[error("git {} failed{}{}", args.join(" "), match status { Some(c) => format!(" (exit {c})"), None => " (killed by signal)".to_string() }, if stderr.is_empty() { String::new() } else { format!(":\n{stderr}") })]
pub struct GitError {
    pub args: Vec<String>,
    pub status: Option<i32>,
    pub stderr: String,
}

/// Thin wrapper around the system `git` binary.
#[derive(Debug, Clone, Default)]
pub struct Git {
    cwd: Option<PathBuf>,
}

impl Git {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            cwd: Some(path.as_ref().to_path_buf()),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        if let Some(cwd) = &self.cwd {
            cmd.arg("-C").arg(cwd);
        }
        cmd.args(args);
        cmd
    }

    fn error(args: &[&str], status: Option<i32>, stderr: &[u8]) -> GitError {
        GitError {
            args: args.iter().map(|s| s.to_string()).collect(),
            status,
            stderr: String::from_utf8_lossy(stderr).trim().to_string(),
        }
    }

    /// Run and return trimmed stdout.
    pub fn out(&self, args: &[&str]) -> Result<String, GitError> {
        let bytes = self.out_bytes(args)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// Run and return raw stdout bytes (for `-z` output).
    pub fn out_bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let output = self
            .command(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Self::error(args, None, e.to_string().as_bytes()))?;
        if !output.status.success() {
            return Err(Self::error(args, output.status.code(), &output.stderr));
        }
        Ok(output.stdout)
    }

    /// Run and report only success/failure (for probes like `show-ref --verify`).
    pub fn ok(&self, args: &[&str]) -> bool {
        self.command(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Run for effect; stdout is discarded, stderr is captured into the error.
    pub fn run(&self, args: &[&str]) -> Result<(), GitError> {
        self.out_bytes(args).map(|_| ())
    }

    /// Run with stdin/stderr inherited (progress bars, auth prompts). Anything
    /// the child writes to stdout is forwarded to our stderr so that wrapped
    /// stdout capture stays clean.
    pub fn interactive(&self, args: &[&str]) -> Result<(), GitError> {
        let output = self
            .command(args)
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| Self::error(args, None, e.to_string().as_bytes()))?;
        if !output.stdout.is_empty() {
            let _ = std::io::stderr().write_all(&output.stdout);
        }
        if !output.status.success() {
            return Err(Self::error(args, output.status.code(), b""));
        }
        Ok(())
    }
}
