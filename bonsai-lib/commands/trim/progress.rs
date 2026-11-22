use std::io::{self, Write};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) struct Progress {
    pub removed: Arc<AtomicU64>,
    pub finished: Arc<AtomicU64>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn new(enabled: bool, total: u64, paths: usize) -> Self {
        let removed = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicU64::new(0));
        let mut progress = Self {
            removed,
            finished,
            stop: None,
            thread: None,
        };
        if enabled {
            let (stop, receiver) = mpsc::channel();
            progress.stop = Some(stop);
            let removed = Arc::clone(&progress.removed);
            let finished = Arc::clone(&progress.finished);
            progress.thread = Some(thread::spawn(move || {
                let start = Instant::now();
                loop {
                    let line = render(
                        removed.load(Ordering::Relaxed),
                        total,
                        finished.load(Ordering::Relaxed),
                        paths,
                        start.elapsed(),
                    );
                    let width =
                        crossterm::terminal::size().map_or(88, |(width, _)| usize::from(width));
                    let mut stderr = io::stderr().lock();
                    let _ = write!(
                        stderr,
                        "\r\x1b[2K{}",
                        super::fit(&line, width.saturating_sub(1))
                    );
                    let _ = stderr.flush();
                    drop(stderr);
                    if receiver
                        .recv_timeout(Duration::from_millis(100))
                        .is_err_and(|error| error == mpsc::RecvTimeoutError::Disconnected)
                    {
                        break;
                    }
                }
                let _ = write!(io::stderr(), "\r\x1b[2K");
            }));
        }
        progress
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn render(removed: u64, total: u64, finished: u64, paths: usize, elapsed: Duration) -> String {
    let percent = if total == 0 {
        0
    } else {
        removed
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(0)
            .min(100)
    };
    let bars = usize::try_from(percent / 5).unwrap_or(20);
    let rate = removed as f64 / elapsed.as_secs_f64().max(0.001);
    format!(
        "bonsai: [{}{}] {percent:>3}%  {removed}/{total} entries  {rate:.0}/s  {finished}/{paths} paths",
        "=".repeat(bars),
        " ".repeat(20 - bars)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_handles_empty_and_changing_trees() {
        assert!(render(0, 0, 0, 1, Duration::ZERO).contains("  0%"));
        let halfway = render(50, 100, 1, 2, Duration::from_secs(2));
        assert!(halfway.contains(" 50%"));
        assert!(halfway.contains("25/s"));
        assert!(halfway.contains("1/2 paths"));
        assert!(render(110, 100, 2, 2, Duration::from_secs(2)).contains("100%"));
    }
}
