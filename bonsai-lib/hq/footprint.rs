//! Checkout size, dependency directories, and coding-harness presence.
//!
//! Walks run off the headquarters input thread and stream partial sizes while
//! a checkout is walked. `shared` means a symlink, a hardlinked file, or an
//! external store such as Go modules. Copy-on-write clones still report their
//! full logical size as `local`. The last complete measurement of each
//! checkout can be kept across sessions with [`save_cache`].

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::io::{BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::parallel;

/// Partial sizes are sent at most this often for each checkout being walked.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Any other `version` in the cache file loads as an empty cache.
const CACHE_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub ecosystems: Vec<String>,
    pub bytes: u64,
    pub dependencies: Vec<Dependency>,
    pub harnesses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub bytes: Option<u64>,
    pub sharing: Sharing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sharing {
    Local,
    Shared,
    None,
}

impl Footprint {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.ecosystems.is_empty() {
            parts.push(self.ecosystems.join("+"));
        }
        parts.push(format_bytes(self.bytes));
        for dependency in &self.dependencies {
            parts.push(dependency.label());
        }
        if !self.harnesses.is_empty() {
            parts.push(self.harnesses.join(" "));
        }
        parts.join(" · ")
    }

    fn unknown() -> Self {
        Self {
            ecosystems: Vec::new(),
            bytes: 0,
            dependencies: Vec::new(),
            harnesses: Vec::new(),
        }
    }
}

impl Dependency {
    pub(crate) fn label(&self) -> String {
        let word = sharing_word(self.sharing);
        match self.bytes {
            None => format!("{} {word}", self.name),
            Some(bytes) if self.sharing == Sharing::None => {
                format!("{} {}", self.name, format_bytes(bytes))
            }
            Some(bytes) => format!("{} {} {word}", self.name, format_bytes(bytes)),
        }
    }
}

fn sharing_word(sharing: Sharing) -> &'static str {
    match sharing {
        Sharing::Local => "local",
        Sharing::Shared => "shared",
        Sharing::None => "none",
    }
}

/// One size update for a checkout. While the walk runs, `complete` is false
/// and `footprint` holds the bytes counted so far (harnesses may still be
/// empty). Each measured checkout ends with exactly one complete update.
#[derive(Debug)]
pub struct Measured {
    pub generation: u64,
    pub path: PathBuf,
    pub footprint: Footprint,
    pub complete: bool,
}

/// The last complete measurement of a checkout, kept across sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cached {
    pub footprint: Footprint,
    /// Seconds since the Unix epoch.
    pub measured_at: u64,
}

pub struct MeasureJob {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Measured>,
}

impl MeasureJob {
    pub fn try_recv(&self) -> Option<Measured> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for MeasureJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn measure_all(paths: &[PathBuf]) -> HashMap<PathBuf, Footprint> {
    if paths.is_empty() {
        return HashMap::new();
    }
    let cancel = AtomicBool::new(false);
    let index = HarnessIndex::load(&cancel);
    let cache = Mutex::new(HashMap::new());
    parallel::map_ordered(paths, |path| {
        let scan = Scan {
            cancel: &cancel,
            cache: &cache,
        };
        let footprint = measure(path, &index, &scan).unwrap_or_else(Footprint::unknown);
        (path.clone(), footprint)
    })
    .into_iter()
    .collect()
}

/// Measures `paths` in the background. Each checkout streams partial updates
/// while it is walked, then one complete update. Dropping the job stops it.
pub fn start(paths: Vec<PathBuf>, generation: u64) -> MeasureJob {
    start_with(paths, generation, PROGRESS_INTERVAL, HarnessIndex::load)
}

type LoadIndex = fn(&AtomicBool) -> HarnessIndex;

fn start_with(
    paths: Vec<PathBuf>,
    generation: u64,
    every: Duration,
    load_index: LoadIndex,
) -> MeasureJob {
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    if !paths.is_empty() {
        let cancel_worker = Arc::clone(&cancel);
        std::thread::spawn(move || {
            run_measure(paths, generation, every, load_index, cancel_worker, sender);
        });
    }
    MeasureJob { cancel, receiver }
}

fn run_measure(
    paths: Vec<PathBuf>,
    generation: u64,
    every: Duration,
    load_index: LoadIndex,
    cancel: Arc<AtomicBool>,
    sender: Sender<Measured>,
) {
    let index_slot = Arc::new(Mutex::new(None::<Arc<HarnessIndex>>));
    let cache = Mutex::new(HashMap::new());
    let next = AtomicUsize::new(0);
    let workers = paths.len().clamp(1, 8);
    std::thread::scope(|scope| {
        let slot = Arc::clone(&index_slot);
        let index_cancel = Arc::clone(&cancel);
        scope.spawn(move || {
            let index = Arc::new(load_index(&index_cancel));
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(index);
            }
        });
        for _ in 0..workers {
            let sender = sender.clone();
            let slot = Arc::clone(&index_slot);
            let cancel = Arc::clone(&cancel);
            let paths = &paths;
            let next = &next;
            let cache = &cache;
            scope.spawn(move || {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let current = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(current) else {
                        break;
                    };
                    let scan = Scan {
                        cancel: &cancel,
                        cache,
                    };
                    let mut harnesses = None::<Vec<String>>;
                    let mut partial = |mut footprint: Footprint| {
                        if harnesses.is_none() {
                            harnesses = loaded_index(&slot).map(|index| index.matching(path));
                        }
                        footprint.harnesses = harnesses.clone().unwrap_or_default();
                        // A closed channel means the job was dropped, which
                        // also sets the cancel flag that stops the walk.
                        let _ = sender.send(Measured {
                            generation,
                            path: path.clone(),
                            footprint,
                            complete: false,
                        });
                    };
                    let mut progress = Progress::new(every, &mut partial);
                    let Some(mut footprint) = measure_disk(path, &scan, Some(&mut progress)) else {
                        break;
                    };
                    footprint.harnesses = match harnesses {
                        Some(harnesses) => harnesses,
                        None => match wait_for_index(&slot, &cancel) {
                            Some(index) => index.matching(path),
                            None => break,
                        },
                    };
                    if sender
                        .send(Measured {
                            generation,
                            path: path.clone(),
                            footprint,
                            complete: true,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
}

fn loaded_index(slot: &Mutex<Option<Arc<HarnessIndex>>>) -> Option<Arc<HarnessIndex>> {
    slot.lock().ok().and_then(|guard| guard.clone())
}

fn wait_for_index(
    slot: &Mutex<Option<Arc<HarnessIndex>>>,
    cancel: &AtomicBool,
) -> Option<Arc<HarnessIndex>> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(index) = loaded_index(slot) {
            return Some(index);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Throttles the partial updates of one checkout walk.
struct Progress<'a> {
    every: Duration,
    due: Instant,
    send: &'a mut dyn FnMut(Footprint),
}

impl<'a> Progress<'a> {
    fn new(every: Duration, send: &'a mut dyn FnMut(Footprint)) -> Self {
        Self {
            every,
            due: Instant::now() + every,
            send,
        }
    }

    fn due(&mut self) -> bool {
        let now = Instant::now();
        if now < self.due {
            return false;
        }
        self.due = now + self.every;
        true
    }
}

struct Scan<'a> {
    cancel: &'a AtomicBool,
    cache: &'a Mutex<HashMap<PathBuf, u64>>,
}

impl Scan<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn measure(path: &Path, index: &HarnessIndex, scan: &Scan) -> Option<Footprint> {
    let mut footprint = measure_disk(path, scan, None)?;
    footprint.harnesses = index.matching(path);
    Some(footprint)
}

/// Harnesses are left empty. `progress` receives partial footprints while
/// the checkout tree is walked.
fn measure_disk(
    path: &Path,
    scan: &Scan,
    mut progress: Option<&mut Progress>,
) -> Option<Footprint> {
    if scan.cancelled() {
        return None;
    }
    let ecosystems = detect_ecosystems(path);
    let mut slots = plan(path, &ecosystems, scan)?;
    let bytes = sum_tree(path, &mut slots, scan, &mut |bytes, slots| {
        if let Some(progress) = progress.as_deref_mut()
            && progress.due()
        {
            (progress.send)(Footprint {
                ecosystems: ecosystems.clone(),
                bytes,
                dependencies: dependencies(slots),
                harnesses: Vec::new(),
            });
        }
    })?;
    Some(Footprint {
        ecosystems,
        bytes,
        dependencies: dependencies(&slots),
        harnesses: Vec::new(),
    })
}

fn detect_ecosystems(root: &Path) -> Vec<String> {
    const MARKERS: &[(&[&str], &str)] = &[
        (&["Cargo.toml"], "rust"),
        (&["package.json"], "node"),
        (
            &["pyproject.toml", "requirements.txt", "setup.py"],
            "python",
        ),
        (&["go.mod"], "go"),
        (&["Gemfile"], "ruby"),
        (&["pom.xml", "build.gradle", "build.gradle.kts"], "java"),
        (&["mix.exs"], "elixir"),
        (&["composer.json"], "php"),
        (&["Package.swift"], "swift"),
    ];
    MARKERS
        .iter()
        .filter(|(files, _)| files.iter().any(|file| root.join(file).is_file()))
        .map(|(_, name)| (*name).to_string())
        .collect()
}

/// Dependency sizes counted so far, in plan order.
fn dependencies(slots: &[Slot]) -> Vec<Dependency> {
    slots
        .iter()
        .map(|slot| match slot {
            Slot::Ready(dependency) => dependency.clone(),
            Slot::Dir(acc) => Dependency {
                name: acc.name.clone(),
                bytes: Some(acc.bytes),
                sharing: acc.sharing,
            },
        })
        .collect()
}

enum Slot {
    Ready(Dependency),
    Dir(DepAcc),
}

impl Slot {
    fn name(&self) -> &str {
        match self {
            Self::Ready(dependency) => &dependency.name,
            Self::Dir(acc) => &acc.name,
        }
    }
}

struct DepAcc {
    name: String,
    path: PathBuf,
    bytes: u64,
    sharing: Sharing,
    sampled: u8,
}

fn plan(root: &Path, ecosystems: &[String], scan: &Scan) -> Option<Vec<Slot>> {
    let has = |name: &str| ecosystems.iter().any(|item| item == name);
    let mut slots = Vec::new();
    if has("rust") {
        slots.push(plan_path(root, "target", scan)?);
    }
    if has("node") {
        slots.push(plan_path(root, "node_modules", scan)?);
    }
    if has("python") {
        slots.push(plan_path(root, ".venv", scan)?);
        if metadata_present(&root.join("venv")) {
            slots.push(plan_path(root, "venv", scan)?);
        }
    }
    if has("go") {
        if metadata_present(&root.join("vendor")) {
            slots.push(plan_path(root, "vendor", scan)?);
        } else {
            slots.push(Slot::Ready(Dependency {
                name: "modules".to_string(),
                bytes: None,
                sharing: Sharing::Shared,
            }));
        }
    }
    if has("ruby") {
        slots.push(plan_path(root, "vendor/bundle", scan)?);
    }
    if has("php") && !slots.iter().any(|slot| slot.name() == "vendor") {
        slots.push(plan_path(root, "vendor", scan)?);
    }
    if has("java") {
        if metadata_present(&root.join("build")) {
            slots.push(plan_path(root, "build", scan)?);
        }
        if metadata_present(&root.join(".gradle")) {
            slots.push(plan_path(root, ".gradle", scan)?);
        }
    }
    Some(slots)
}

fn plan_path(root: &Path, rel: &str, scan: &Scan) -> Option<Slot> {
    let path = root.join(rel);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(_) => {
            return Some(Slot::Ready(Dependency {
                name: rel.to_string(),
                bytes: None,
                sharing: Sharing::None,
            }));
        }
    };
    if meta.file_type().is_symlink() {
        let (sharing, bytes) = symlink_dependency(root, &path, scan)?;
        return Some(Slot::Ready(Dependency {
            name: rel.to_string(),
            bytes,
            sharing,
        }));
    }
    Some(Slot::Dir(DepAcc {
        name: rel.to_string(),
        path,
        bytes: 0,
        sharing: Sharing::Local,
        sampled: 0,
    }))
}

fn metadata_present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// A symlinked dependency contributes its own link length to the checkout.
/// The target is reported separately so a shared store is not added again.
fn symlink_dependency(checkout: &Path, link: &Path, scan: &Scan) -> Option<(Sharing, Option<u64>)> {
    if scan.cancelled() {
        return None;
    }
    let Ok(target) = std::fs::read_link(link) else {
        return Some((Sharing::Shared, None));
    };
    let absolute = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(checkout).join(target)
    };
    let Some(absolute) = normalize_path(&absolute) else {
        return Some((Sharing::Shared, None));
    };
    let checkout = normalize_path(checkout).unwrap_or_else(|| checkout.to_path_buf());
    let sharing = if absolute.starts_with(&checkout) {
        Sharing::Local
    } else {
        Sharing::Shared
    };
    if unsafe_target(&absolute) {
        return Some((sharing, None));
    }
    Some((sharing, Some(linked_size(&absolute, scan)?)))
}

fn unsafe_target(target: &Path) -> bool {
    if target.components().count() <= 2 {
        return true;
    }
    let Some(home) = std::env::home_dir() else {
        return false;
    };
    let home = normalize_path(&home).unwrap_or(home);
    target == home || home.starts_with(target)
}

fn linked_size(target: &Path, scan: &Scan) -> Option<u64> {
    if let Some(size) = scan
        .cache
        .lock()
        .ok()
        .and_then(|guard| guard.get(target).copied())
    {
        return Some(size);
    }
    let size = directory_size(target, scan)?;
    if let Ok(mut guard) = scan.cache.lock() {
        guard.insert(target.to_path_buf(), size);
    }
    Some(size)
}

fn directory_size(path: &Path, scan: &Scan) -> Option<u64> {
    if scan.cancelled() {
        return None;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Some(0);
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() || file_type.is_file() {
        return Some(meta.len());
    }
    if !file_type.is_dir() {
        return Some(0);
    }
    sum_tree(path, &mut [], scan, &mut |_, _| {})
}

/// `tick` sees the running total and slots after each directory.
fn sum_tree(
    root: &Path,
    slots: &mut [Slot],
    scan: &Scan,
    tick: &mut dyn FnMut(u64, &[Slot]),
) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if scan.cancelled() {
            return None;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                let len = meta.len();
                total = total.saturating_add(len);
                attribute(slots, &path, len, None);
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if file_type.is_file() {
                let len = meta.len();
                total = total.saturating_add(len);
                attribute(slots, &path, len, Some(&meta));
            }
        }
        tick(total, slots);
    }
    Some(total)
}

fn attribute(slots: &mut [Slot], path: &Path, len: u64, file: Option<&std::fs::Metadata>) {
    let mut best: Option<usize> = None;
    let mut best_len = 0usize;
    for (index, slot) in slots.iter().enumerate() {
        let Slot::Dir(acc) = slot else {
            continue;
        };
        if path.starts_with(&acc.path) {
            let length = acc.path.components().count();
            if length >= best_len {
                best = Some(index);
                best_len = length;
            }
        }
    }
    let Some(index) = best else {
        return;
    };
    let Slot::Dir(acc) = &mut slots[index] else {
        return;
    };
    acc.bytes = acc.bytes.saturating_add(len);
    if let Some(meta) = file
        && acc.sharing != Sharing::Shared
        && acc.sampled < 20
    {
        acc.sampled += 1;
        if hard_linked(meta) {
            acc.sharing = Sharing::Shared;
        }
    }
}

fn hard_linked(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Decimal SI units: one decimal below 10 (`1.5 kB`, `2 GB`), whole numbers
/// from 10 (`34 MB`).
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let bytes = u128::from(bytes);
    let mut unit = 0;
    let mut scale = 1u128;
    while unit + 1 < UNITS.len() && bytes >= scale * 1000 {
        scale *= 1000;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let tenths = (bytes * 10 + scale / 2) / scale;
    if tenths < 100 {
        let (whole, fraction) = (tenths / 10, tenths % 10);
        return if fraction == 0 {
            format!("{whole} {}", UNITS[unit])
        } else {
            format!("{whole}.{fraction} {}", UNITS[unit])
        };
    }
    let whole = (bytes + scale / 2) / scale;
    if whole >= 1000 && unit + 1 < UNITS.len() {
        return format!("1 {}", UNITS[unit + 1]);
    }
    format!("{whole} {}", UNITS[unit])
}

#[derive(Serialize)]
struct CacheFile<'a> {
    version: u64,
    checkouts: BTreeMap<&'a str, &'a Cached>,
}

/// `$XDG_STATE_HOME/bonsai/footprints.json`, else
/// `~/.local/state/bonsai/footprints.json`. `_BONSAI_HQ_FOOTPRINTS` overrides
/// the file; empty or `0` disables the cache.
pub fn cache_path() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("_BONSAI_HQ_FOOTPRINTS") {
        if value.is_empty() || value == "0" {
            return None;
        }
        return Some(PathBuf::from(value));
    }
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        let state = match std::env::var_os("XDG_STATE_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => std::env::home_dir()?.join(".local/state"),
        };
        Some(state.join("bonsai").join("footprints.json"))
    }
}

/// A missing, unreadable, corrupt, or other-version file loads as empty.
/// Entries that do not parse are skipped.
pub fn load_cache(path: &Path) -> HashMap<PathBuf, Cached> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(serde_json::Value::Object(mut document)) = serde_json::from_str(&text) else {
        return HashMap::new();
    };
    if document.get("version").and_then(serde_json::Value::as_u64) != Some(CACHE_VERSION) {
        return HashMap::new();
    }
    let Some(serde_json::Value::Object(checkouts)) = document.remove("checkouts") else {
        return HashMap::new();
    };
    checkouts
        .into_iter()
        .filter_map(|(path, entry)| {
            Some((PathBuf::from(path), serde_json::from_value(entry).ok()?))
        })
        .collect()
}

/// Replaces the file atomically, entries sorted by path. Paths that are not
/// UTF-8 cannot be JSON keys and are skipped.
pub fn save_cache(path: &Path, entries: &HashMap<PathBuf, Cached>) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let document = CacheFile {
        version: CACHE_VERSION,
        checkouts: entries
            .iter()
            .filter_map(|(path, cached)| Some((path.to_str()?, cached)))
            .collect(),
    };
    let mut text = serde_json::to_string_pretty(&document).map_err(std::io::Error::other)?;
    text.push('\n');
    // A file torn by a crash loads as empty and is measured again: no fsync.
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    temporary.write_all(text.as_bytes())?;
    temporary.persist(path)?;
    Ok(())
}

#[derive(Default)]
struct HarnessRoots {
    claude_projects: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    codex_sqlite: Option<PathBuf>,
    grok_sessions: Option<PathBuf>,
    opencode_db: Option<PathBuf>,
}

#[derive(Default)]
struct HarnessIndex {
    claude: Vec<String>,
    codex: Vec<PathBuf>,
    grok: Vec<PathBuf>,
    opencode: Vec<PathBuf>,
}

impl HarnessIndex {
    fn load(cancel: &AtomicBool) -> Self {
        // Integration tests set this so `hq --json` does not read developer stores.
        if cancel.load(Ordering::Relaxed) || std::env::var_os("_BONSAI_HQ_SKIP_HARNESS").is_some() {
            return Self::default();
        }
        Self::from_roots(&HarnessRoots::discover(), cancel)
    }

    fn from_roots(roots: &HarnessRoots, cancel: &AtomicBool) -> Self {
        if cancel.load(Ordering::Relaxed) {
            return Self::default();
        }
        let claude = roots
            .claude_projects
            .as_deref()
            .map(directory_names)
            .unwrap_or_default();
        let mut codex = Vec::new();
        if let Some(sqlite_root) = &roots.codex_sqlite {
            for database in state_databases(sqlite_root) {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                codex.extend(
                    sqlite_texts(&database, "SELECT DISTINCT cwd FROM threads")
                        .into_iter()
                        .filter_map(|cwd| normalize_path(Path::new(&cwd))),
                );
            }
        }
        if let Some(home) = &roots.codex_home {
            codex.extend(jsonl_cwds(&home.join("sessions"), cancel));
        }
        let grok = roots
            .grok_sessions
            .as_deref()
            .map(grok_paths)
            .unwrap_or_default();
        let opencode = roots
            .opencode_db
            .as_deref()
            .map(opencode_directories)
            .unwrap_or_default();
        Self {
            claude,
            codex: dedup_paths(codex),
            grok,
            opencode: dedup_paths(opencode),
        }
    }

    fn matching(&self, worktree: &Path) -> Vec<String> {
        let normalized = normalize_path(worktree).unwrap_or_else(|| worktree.to_path_buf());
        let mut labels = Vec::new();
        let encoded = [claude_encode(worktree), claude_encode(&normalized)];
        if self.claude.iter().any(|stored| {
            encoded
                .iter()
                .any(|candidate| claude_related(stored, candidate))
        }) {
            labels.push("claude".to_string());
        }
        if self
            .codex
            .iter()
            .any(|path| paths_related(path, &normalized))
        {
            labels.push("codex".to_string());
        }
        if self
            .grok
            .iter()
            .any(|path| paths_related(path, &normalized))
        {
            labels.push("grok".to_string());
        }
        if self
            .opencode
            .iter()
            .any(|path| paths_related(path, &normalized))
        {
            labels.push("opencode".to_string());
        }
        labels
    }
}

impl HarnessRoots {
    fn discover() -> Self {
        let home = std::env::home_dir();
        let claude =
            env_path("CLAUDE_CONFIG_DIR").or_else(|| home.as_ref().map(|dir| dir.join(".claude")));
        let codex_home =
            env_path("CODEX_HOME").or_else(|| home.as_ref().map(|dir| dir.join(".codex")));
        let codex_sqlite = codex_home.as_ref().map(|dir| codex_sqlite_root(dir));
        Self {
            claude_projects: claude.map(|dir| dir.join("projects")),
            codex_home,
            codex_sqlite,
            grok_sessions: home.as_ref().map(|dir| dir.join(".grok/sessions")),
            opencode_db: Some(opencode_database()),
        }
    }
}

fn directory_names(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn state_databases(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("state_") && name.ends_with(".sqlite")
        })
        .map(|entry| entry.path())
        .collect()
}

fn sqlite_texts(path: &Path, sql: &str) -> Vec<String> {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let _ = connection.busy_timeout(Duration::from_secs(1));
    let mut statement = match connection.prepare(sql) {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    let rows = match statement.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    rows.filter_map(Result::ok).collect()
}

fn jsonl_cwds(root: &Path, cancel: &AtomicBool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if file_type.is_file()
                && path.extension() == Some(OsStr::new("jsonl"))
                && let Some(cwd) = first_cwd(&path)
            {
                found.push(cwd);
            }
        }
    }
    found
}

fn first_cwd(path: &Path) -> Option<PathBuf> {
    let line = first_line(path)?;
    let value: serde_json::Value = serde_json::from_str(&line).ok()?;
    let cwd = value
        .pointer("/payload/cwd")
        .and_then(|value| value.as_str())
        .or_else(|| value.get("cwd").and_then(|value| value.as_str()))?;
    normalize_path(Path::new(cwd))
}

fn first_line(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut limited = std::io::BufReader::new(file).take(64 * 1024);
    let mut buffer = Vec::new();
    limited.read_until(b'\n', &mut buffer).ok()?;
    if buffer.last() == Some(&b'\n') {
        buffer.pop();
    } else if buffer.len() >= 64 * 1024 {
        return None;
    }
    let line = std::str::from_utf8(&buffer).ok()?.trim();
    Some(line.to_string())
}

fn grok_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(decoded) = percent_decode(name) else {
            continue;
        };
        let Some(path) = normalize_path(Path::new(&decoded)) else {
            continue;
        };
        paths.push(path);
    }
    dedup_paths(paths)
}

fn opencode_directories(path: &Path) -> Vec<PathBuf> {
    dedup_paths(
        sqlite_texts(path, "SELECT DISTINCT directory FROM session")
            .into_iter()
            .filter_map(|directory| normalize_path(Path::new(&directory)))
            .collect(),
    )
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Claude's project directory replaces every non-alphanumeric byte with `-`.
/// `/repo/child` and `/repo-other` can therefore look alike; real paths use
/// component boundaries instead.
fn claude_encode(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn claude_related(stored: &str, encoded: &str) -> bool {
    fn boundary(longer: &str, shorter: &str) -> bool {
        longer
            .strip_prefix(shorter)
            .is_some_and(|rest| rest.starts_with('-'))
    }
    stored == encoded || boundary(stored, encoded) || boundary(encoded, stored)
}

fn paths_related(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn dedup_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

fn normalize_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Some(crate::paths::canonicalize_or_self(&normalized))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn codex_sqlite_root(codex_home: &Path) -> PathBuf {
    if let Some(path) = env_path("CODEX_SQLITE_HOME") {
        return path;
    }
    std::fs::read_to_string(codex_home.join("config.toml"))
        .ok()
        .and_then(|contents| contents.parse::<toml::Value>().ok())
        .and_then(|config| config.get("sqlite_home")?.as_str().map(str::to_string))
        .map(|path| crate::config::expand_tilde(&path))
        .unwrap_or_else(|| codex_home.to_path_buf())
}

fn opencode_database() -> PathBuf {
    let data = env_path("XDG_DATA_HOME").unwrap_or_else(|| {
        std::env::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share")
    });
    match env_path("OPENCODE_DB") {
        Some(path) if path.is_absolute() => path,
        Some(path) => data.join("opencode").join(path),
        None => data.join("opencode/opencode.db"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn inspect(path: &Path) -> Footprint {
        let cancel = AtomicBool::new(false);
        let cache = Mutex::new(HashMap::new());
        let scan = Scan {
            cancel: &cancel,
            cache: &cache,
        };
        measure(path, &HarnessIndex::default(), &scan).unwrap()
    }

    fn canonical(path: &Path) -> PathBuf {
        crate::paths::canonicalize_or_self(path)
    }

    fn without_harnesses(_: &AtomicBool) -> HarnessIndex {
        HarnessIndex::default()
    }

    fn harnesses_after_cancel(cancel: &AtomicBool) -> HarnessIndex {
        while !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
        HarnessIndex::default()
    }

    /// Every update until the job's workers have all exited.
    fn drain(job: &MeasureJob) -> Vec<Measured> {
        let mut updates = Vec::new();
        loop {
            match job.receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(update) => updates.push(update),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return updates,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("measurement did not finish: {updates:?}")
                }
            }
        }
    }

    fn rust_checkout(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("target/debug/deps/a"), vec![0_u8; 300]).unwrap();
        std::fs::write(root.join("target/debug/b"), vec![0_u8; 200]).unwrap();
    }

    fn cached(bytes: u64, measured_at: u64) -> Cached {
        Cached {
            footprint: Footprint {
                ecosystems: vec!["rust".to_string()],
                bytes,
                dependencies: vec![
                    Dependency {
                        name: "target".to_string(),
                        bytes: Some(bytes / 2),
                        sharing: Sharing::Local,
                    },
                    Dependency {
                        name: "modules".to_string(),
                        bytes: None,
                        sharing: Sharing::Shared,
                    },
                ],
                harnesses: vec!["claude".to_string()],
            },
            measured_at,
        }
    }

    #[test]
    fn formats_decimal_sizes() {
        for (bytes, expected) in [
            (0, "0 B"),
            (512, "512 B"),
            (999, "999 B"),
            (1_000, "1 kB"),
            (1_500, "1.5 kB"),
            (1_536, "1.5 kB"),
            (9_949, "9.9 kB"),
            (9_950, "10 kB"),
            (999_499, "999 kB"),
            (999_500, "1 MB"),
            (34_000_000, "34 MB"),
            (2_000_000_000, "2 GB"),
            (2_040_000_000, "2 GB"),
            (5_600_000_000, "5.6 GB"),
            (67_000_000_000, "67 GB"),
            (1_200_000_000_000, "1.2 TB"),
            (247_000_000_000_000, "247 TB"),
            (u64::MAX, "18446744 TB"),
        ] {
            assert_eq!(format_bytes(bytes), expected, "{bytes}");
        }
    }

    #[test]
    fn labels_use_the_decimal_format() {
        let footprint = cached(3_000_000, 0).footprint;
        assert_eq!(
            footprint.summary(),
            "rust · 3 MB · target 1.5 MB local · modules shared · claude"
        );
    }

    #[test]
    fn footprints_round_trip_through_json() {
        let footprint = cached(42, 0).footprint;
        let value = serde_json::to_value(&footprint).unwrap();
        assert_eq!(value["dependencies"][1]["sharing"], "shared");
        assert_eq!(
            serde_json::from_value::<Footprint>(value).unwrap(),
            footprint
        );
    }

    #[test]
    fn start_sends_one_complete_update_per_checkout() {
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        rust_checkout(&first);
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("file"), "hello").unwrap();
        let job = start_with(
            vec![first.clone(), second.clone()],
            3,
            PROGRESS_INTERVAL,
            without_harnesses,
        );
        let updates = drain(&job);
        for (path, expected) in [(&first, inspect(&first)), (&second, inspect(&second))] {
            let complete: Vec<_> = updates
                .iter()
                .filter(|update| &update.path == path && update.complete)
                .collect();
            assert_eq!(complete.len(), 1, "{updates:?}");
            assert_eq!(complete[0].generation, 3);
            assert_eq!(complete[0].footprint, expected);
            let last = updates.iter().rfind(|update| &update.path == path).unwrap();
            assert!(last.complete, "the complete update comes last: {updates:?}");
        }
        assert_eq!(
            updates
                .iter()
                .find(|update| update.path == second && update.complete)
                .unwrap()
                .footprint
                .bytes,
            5
        );
    }

    #[test]
    fn start_streams_partial_sizes_while_a_walk_is_slow() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout");
        rust_checkout(&root);
        // A zero interval makes every directory of the walk report progress.
        let job = start_with(vec![root.clone()], 9, Duration::ZERO, without_harnesses);
        let updates = drain(&job);
        let (last, partial) = updates.split_last().unwrap();
        assert!(last.complete);
        assert_eq!(last.footprint, inspect(&root));
        assert!(!partial.is_empty(), "{updates:?}");
        for update in partial {
            assert!(!update.complete);
            assert_eq!(update.generation, 9);
            assert_eq!(update.path, root);
            assert_eq!(update.footprint.ecosystems, vec!["rust".to_string()]);
            assert_eq!(update.footprint.dependencies.len(), 1);
            assert!(update.footprint.bytes <= last.footprint.bytes);
        }
        assert!(
            partial
                .windows(2)
                .all(|pair| pair[0].footprint.bytes <= pair[1].footprint.bytes)
        );
        assert!(partial[0].footprint.bytes < last.footprint.bytes);
        let target = |update: &Measured| update.footprint.dependencies[0].bytes.unwrap();
        assert!(partial.iter().all(|update| target(update) <= 500));
        assert_eq!(target(last), 500);
    }

    #[test]
    fn cancelling_a_job_stops_it_without_a_complete_update() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout");
        rust_checkout(&root);
        // The harness index only loads once cancelled, so the walk finishes
        // and the worker waits until the cancel flag releases it.
        let job = start_with(vec![root], 1, Duration::ZERO, harnesses_after_cancel);
        let first = job.receiver.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(!first.complete);
        job.cancel.store(true, Ordering::Relaxed);
        assert!(drain(&job).iter().all(|update| !update.complete));
    }

    #[test]
    fn the_cache_round_trips_and_replaces_an_existing_file() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state/bonsai/footprints.json");
        let mut entries = HashMap::new();
        entries.insert(PathBuf::from("/trees/b"), cached(2_000, 10));
        entries.insert(PathBuf::from("/trees/a"), cached(1_000, 20));
        save_cache(&path, &entries).unwrap();
        assert_eq!(load_cache(&path), entries);
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["checkouts"]["/trees/a"]["measured_at"], 20);
        assert!(text.find("/trees/a").unwrap() < text.find("/trees/b").unwrap());
        save_cache(&path, &entries).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        let mut replaced = HashMap::new();
        replaced.insert(PathBuf::from("/trees/c"), cached(3_000, 30));
        save_cache(&path, &replaced).unwrap();
        assert_eq!(load_cache(&path), replaced);
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("footprints.json")]);
    }

    #[test]
    fn tests_use_no_cache_file_unless_one_is_named() {
        if std::env::var_os("_BONSAI_HQ_FOOTPRINTS").is_none() {
            assert_eq!(cache_path(), None);
        }
    }

    #[test]
    fn a_missing_corrupt_or_other_version_cache_loads_empty() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("footprints.json");
        assert!(load_cache(&path).is_empty());
        for text in [
            "",
            "not json",
            "[]",
            r#"{"checkouts": {}}"#,
            r#"{"version": 2, "checkouts": {"/trees/a": {"footprint": {"ecosystems": [], "bytes": 1, "dependencies": [], "harnesses": []}, "measured_at": 1}}}"#,
            r#"{"version": 1, "checkouts": []}"#,
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(load_cache(&path).is_empty(), "{text}");
        }
        std::fs::create_dir(temporary.path().join("directory")).unwrap();
        assert!(load_cache(&temporary.path().join("directory")).is_empty());
    }

    #[test]
    fn a_cache_entry_that_does_not_parse_is_skipped() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("footprints.json");
        let good = cached(7, 70);
        let document = serde_json::json!({
            "version": 1,
            "checkouts": {
                "/trees/good": good,
                "/trees/bad": {"footprint": {"bytes": "many"}, "measured_at": 1},
            },
        });
        std::fs::write(&path, document.to_string()).unwrap();
        let loaded = load_cache(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(Path::new("/trees/good")), Some(&good));
    }

    #[test]
    fn detects_ecosystems_in_a_fixed_order_and_missing_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(root.join("package.json"), "{}\n").unwrap();
        std::fs::write(root.join("go.mod"), "module example\n").unwrap();
        let footprint = inspect(root);
        assert_eq!(
            footprint.ecosystems,
            vec!["rust".to_string(), "node".to_string(), "go".to_string()]
        );
        assert_eq!(footprint.dependencies[0].name, "target");
        assert_eq!(footprint.dependencies[0].sharing, Sharing::None);
        assert_eq!(footprint.dependencies[1].name, "node_modules");
        assert_eq!(footprint.dependencies[1].sharing, Sharing::None);
        assert_eq!(footprint.dependencies[2].name, "modules");
        assert_eq!(footprint.dependencies[2].bytes, None);
        assert_eq!(footprint.dependencies[2].sharing, Sharing::Shared);
        assert!(footprint.summary().contains("rust+node+go"));
        assert!(footprint.summary().contains("target none"));
        assert!(footprint.summary().contains("node_modules none"));
        assert!(footprint.summary().contains("modules shared"));
    }

    #[test]
    fn python_reports_a_missing_venv_and_a_present_one() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::write(root.join("requirements.txt"), "flask\n").unwrap();
        let footprint = inspect(root);
        assert_eq!(footprint.ecosystems, vec!["python".to_string()]);
        assert_eq!(footprint.dependencies.len(), 1);
        assert_eq!(footprint.dependencies[0].name, ".venv");
        assert_eq!(footprint.dependencies[0].sharing, Sharing::None);
        std::fs::create_dir(root.join("venv")).unwrap();
        std::fs::write(root.join("venv/py"), "hi").unwrap();
        let footprint = inspect(root);
        assert_eq!(footprint.dependencies[1].name, "venv");
        assert_eq!(footprint.dependencies[1].bytes, Some(2));
        assert_eq!(footprint.dependencies[1].sharing, Sharing::Local);
    }

    #[test]
    fn sums_file_lengths_including_git() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "abc").unwrap();
        std::fs::write(root.join("file"), "hello").unwrap();
        let footprint = inspect(root);
        assert_eq!(footprint.bytes, 8);
        assert!(footprint.ecosystems.is_empty());
        assert!(footprint.dependencies.is_empty());
    }

    #[test]
    fn java_reports_build_output_only_when_present() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::write(root.join("pom.xml"), "<project/>").unwrap();
        let footprint = inspect(root);
        assert_eq!(footprint.ecosystems, vec!["java".to_string()]);
        assert!(footprint.dependencies.is_empty());
        std::fs::create_dir(root.join("build")).unwrap();
        std::fs::write(root.join("build/out"), "xyz").unwrap();
        let footprint = inspect(root);
        assert_eq!(footprint.dependencies[0].name, "build");
        assert_eq!(footprint.dependencies[0].bytes, Some(3));
        assert_eq!(footprint.dependencies[0].sharing, Sharing::Local);
    }

    #[test]
    fn a_cancelled_scan_returns_nothing() {
        let temporary = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let cache = Mutex::new(HashMap::new());
        let scan = Scan {
            cancel: &cancel,
            cache: &cache,
        };
        assert!(measure(temporary.path(), &HarnessIndex::default(), &scan).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_dependency_is_shared_and_stays_out_of_the_checkout_total() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout");
        let store = temporary.path().join("store");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::write(store.join("pkg"), vec![0_u8; 1000]).unwrap();
        std::os::unix::fs::symlink(&store, root.join("node_modules")).unwrap();
        let footprint = inspect(&root);
        let dependency = footprint
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "node_modules")
            .unwrap();
        assert_eq!(dependency.sharing, Sharing::Shared);
        assert_eq!(dependency.bytes, Some(1000));
        assert!(footprint.bytes < 1000);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_dependency_files_are_shared() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::create_dir(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules/a"), "hello").unwrap();
        std::fs::hard_link(root.join("node_modules/a"), root.join("node_modules/b")).unwrap();
        let footprint = inspect(root);
        let dependency = &footprint.dependencies[0];
        assert_eq!(dependency.name, "node_modules");
        assert_eq!(dependency.sharing, Sharing::Shared);
        assert_eq!(dependency.bytes, Some(10));
    }

    #[test]
    fn go_vendor_replaces_the_external_module_label() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::write(root.join("go.mod"), "module example\n").unwrap();
        std::fs::create_dir(root.join("vendor")).unwrap();
        std::fs::write(root.join("vendor/lib.go"), "package a\n").unwrap();
        let footprint = inspect(root);
        assert_eq!(footprint.dependencies.len(), 1);
        assert_eq!(footprint.dependencies[0].name, "vendor");
        assert_eq!(footprint.dependencies[0].sharing, Sharing::Local);
        assert_eq!(
            footprint.dependencies[0].bytes,
            Some("package a\n".len() as u64)
        );
    }

    #[test]
    fn grok_percent_encoded_sessions_match_ancestors_and_not_neighbors() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        let child = repo.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let neighbor = temporary.path().join("repo-other");
        std::fs::create_dir_all(&neighbor).unwrap();
        let repo = canonical(&repo);
        let child = canonical(&child);
        let neighbor = canonical(&neighbor);
        let sessions = temporary.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        std::fs::create_dir(sessions.join(percent_encode(&child))).unwrap();
        std::fs::write(sessions.join("session_search.sqlite"), "skip").unwrap();
        let index = HarnessIndex::from_roots(
            &HarnessRoots {
                grok_sessions: Some(sessions),
                ..HarnessRoots::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(index.matching(&repo), vec!["grok".to_string()]);
        assert_eq!(index.matching(&child), vec!["grok".to_string()]);
        assert!(index.matching(&neighbor).is_empty());
    }

    #[test]
    fn claude_dash_encoding_matches_a_project_and_its_child() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        let child = repo.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let unrelated = temporary.path().join("other");
        std::fs::create_dir_all(&unrelated).unwrap();
        let repo = canonical(&repo);
        let child = canonical(&child);
        let unrelated = canonical(&unrelated);
        let projects = temporary.path().join("projects");
        std::fs::create_dir_all(projects.join(claude_encode(&repo))).unwrap();
        let index = HarnessIndex::from_roots(
            &HarnessRoots {
                claude_projects: Some(projects),
                ..HarnessRoots::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(index.matching(&repo), vec!["claude".to_string()]);
        assert_eq!(index.matching(&child), vec!["claude".to_string()]);
        assert!(index.matching(&unrelated).is_empty());
    }

    #[test]
    fn codex_reads_sqlite_and_session_cwd_but_not_history() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("codex");
        let repo = canonical(&temporary.path().join("repo"));
        std::fs::create_dir_all(&repo).unwrap();
        let repo = canonical(&repo);
        std::fs::create_dir_all(home.join("sessions/2026")).unwrap();
        std::fs::write(
            home.join("history.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"session_id": "x", "text": temporary.path().join("decoy"), "ts": 1})
            ),
        )
        .unwrap();
        std::fs::write(
            home.join("sessions/2026/rollout.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"type": "session_meta", "payload": {"cwd": repo}})
            ),
        )
        .unwrap();
        let database = home.join("state_5.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        let sqlite_repo = canonical(&temporary.path().join("sqlite-repo"));
        std::fs::create_dir_all(&sqlite_repo).unwrap();
        let sqlite_repo = canonical(&sqlite_repo);
        connection
            .execute("CREATE TABLE threads (id TEXT, cwd TEXT)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, cwd) VALUES (?1, ?2)",
                rusqlite::params!["a", sqlite_repo.to_string_lossy().as_ref()],
            )
            .unwrap();
        drop(connection);
        let index = HarnessIndex::from_roots(
            &HarnessRoots {
                codex_home: Some(home.clone()),
                codex_sqlite: Some(home),
                ..HarnessRoots::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(index.matching(&repo), vec!["codex".to_string()]);
        assert_eq!(index.matching(&sqlite_repo), vec!["codex".to_string()]);
        assert!(index.matching(&temporary.path().join("decoy")).is_empty());
    }

    #[test]
    fn opencode_reads_session_directories_and_ignores_a_broken_database() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = canonical(&temporary.path().join("repo"));
        std::fs::create_dir_all(&repo).unwrap();
        let repo = canonical(&repo);
        let database = temporary.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute("CREATE TABLE session (directory TEXT)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO session (directory) VALUES (?1)",
                rusqlite::params![repo.to_string_lossy().as_ref()],
            )
            .unwrap();
        drop(connection);
        let index = HarnessIndex::from_roots(
            &HarnessRoots {
                opencode_db: Some(database),
                ..HarnessRoots::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(index.matching(&repo), vec!["opencode".to_string()]);
        assert_eq!(
            index.matching(&repo.join("nested")),
            vec!["opencode".to_string()]
        );

        let broken = temporary.path().join("broken.db");
        std::fs::write(&broken, "not a database").unwrap();
        let index = HarnessIndex::from_roots(
            &HarnessRoots {
                opencode_db: Some(broken),
                ..HarnessRoots::default()
            },
            &AtomicBool::new(false),
        );
        assert!(index.matching(&repo).is_empty());
    }

    fn percent_encode(path: &Path) -> String {
        let mut encoded = String::new();
        for byte in path.to_string_lossy().bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        encoded
    }
}
