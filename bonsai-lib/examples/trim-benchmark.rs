//! Deletion-only comparison on disposable npm-like trees. Creation and
//! compilation are excluded; each remover receives a freshly created tree.
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let packages = number(args.next(), 200)?;
    let files = number(args.next(), 100)?;
    let runs = number(args.next(), 3)?;
    anyhow::ensure!(
        packages > 0 && files > 0 && runs > 0,
        "arguments must be positive"
    );
    anyhow::ensure!(
        args.next().is_none(),
        "usage: trim-benchmark [packages] [files/package] [runs]"
    );
    let tmp = tempfile::tempdir()?;
    let one_worker = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let two_workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let four_workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let methods = [
        "path + metadata + progress",
        "std::fs::remove_dir_all",
        "bonsai + progress (1)",
        "bonsai + progress (2)",
        "bonsai + progress (4)",
        "bonsai + progress (default)",
    ];
    let mut timings: Vec<Vec<Duration>> = vec![Vec::new(); methods.len()];
    for run in 0..runs {
        // Rotate execution order so one method does not always run first.
        for offset in 0..methods.len() {
            let method = (run + offset) % methods.len();
            let tree = tmp.path().join("node_modules");
            fixture(&tree, packages, files)?;
            let removed = AtomicU64::new(0);
            let start = Instant::now();
            match method {
                0 => previous(&tree, &removed)?,
                1 => fs::remove_dir_all(&tree)?,
                2 => one_worker
                    .install(|| bonsai::ignored::remove_tree_with_progress(&tree, &removed))?,
                3 => two_workers.install(|| parallel(&tree, &removed))?,
                4 => four_workers.install(|| parallel(&tree, &removed))?,
                _ => parallel(&tree, &removed)?,
            }
            let elapsed = start.elapsed();
            anyhow::ensure!(!tree.exists(), "tree remains after {}", methods[method]);
            if method != 1 {
                anyhow::ensure!(
                    removed.load(Ordering::Relaxed) == 1 + packages as u64 * (files as u64 + 3),
                    "incomplete progress count"
                );
            }
            timings[method].push(elapsed);
            println!(
                "run {}: {:28} {:>8.3}s",
                run + 1,
                methods[method],
                elapsed.as_secs_f64()
            );
        }
    }
    println!(
        "\n{packages} packages, {files} files/package; creation excluded; median of {runs} runs:"
    );
    for (method, mut samples) in methods.into_iter().zip(timings) {
        samples.sort();
        let median: Duration = samples[samples.len() / 2];
        println!("{method:28} {:>8.3}s", median.as_secs_f64());
    }
    Ok(())
}

fn parallel(tree: &Path, removed: &AtomicU64) -> io::Result<()> {
    let children = fs::read_dir(tree)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    let results: Vec<_> = children
        .par_iter()
        .map(|child| bonsai::ignored::remove_tree_with_progress(child, removed))
        .collect();
    results.into_iter().collect::<io::Result<()>>()?;
    fs::remove_dir(tree)?;
    removed.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn number(raw: Option<String>, fallback: usize) -> Result<usize, std::num::ParseIntError> {
    raw.map_or(Ok(fallback), |value| value.parse())
}

fn fixture(tree: &Path, packages: usize, files: usize) -> io::Result<()> {
    for package in 0..packages {
        let path = tree.join(format!("pkg-{package}/dist/lib"));
        fs::create_dir_all(&path)?;
        for file in 0..files {
            fs::write(
                path.join(format!("module-{file}.js")),
                b"export default 1;\n",
            )?;
        }
    }
    Ok(())
}

/// The former counted deletion: a full path and stat for each entry, with
/// rayon fan-out at every level. Kept here only as a measurement baseline.
fn previous(path: &Path, removed: &AtomicU64) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let children = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        let results: Vec<_> = children
            .par_iter()
            .map(|child| previous(child, removed))
            .collect();
        results.into_iter().collect::<io::Result<()>>()?;
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    removed.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
