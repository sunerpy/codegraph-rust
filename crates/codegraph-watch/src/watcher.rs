use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use notify::event::{EventKind, RemoveKind};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use codegraph_core::IndexPaths;
use codegraph_core::config::Config;
use codegraph_extract::ExtensionOverrides;

use crate::policy::{WatchPolicy, watch_disabled_reason};
use crate::sync::{
    SyncCancellation, SyncOutcome, sync_changed_paths_cancellable, sync_project_once_cancellable,
};

type SyncCallback = Arc<dyn Fn(SyncOutcome) + Send + Sync>;
type SyncFn = Arc<dyn Fn(Vec<String>) -> Result<SyncOutcome> + Send + Sync>;
/// Whole-project sync, used when an event changes the effective project scope
/// or cannot be expressed as a path list (see [`RemovalHint`]).
type FullSyncFn = Arc<dyn Fn() -> Result<SyncOutcome> + Send + Sync>;
type NoticeCallback = Arc<dyn Fn(String) + Send + Sync>;

/// The OS watcher, shared between [`ProjectWatcher`] (which owns its lifetime)
/// and the event-loop thread (which registers a watch when a brand-new
/// non-ignored directory appears). Because Linux watches are NON-recursive
/// per-dir (see [`collect_watch_dirs`]), a freshly-created directory would
/// otherwise hold no watch until a server restart; the loop adds one on its
/// create event.
type SharedWatcher = Arc<Mutex<Option<RecommendedWatcher>>>;

#[derive(Debug, Clone)]
struct RuntimeWatchScope {
    policy: WatchPolicy,
    ignore_dirs: Vec<String>,
    ignore_paths: Vec<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    extensions: Arc<ExtensionOverrides>,
    debounce: Duration,
    enabled: bool,
}

impl RuntimeWatchScope {
    fn from_options(project_root: &Path, options: &WatchOptions) -> Self {
        let policy = WatchPolicy::with_config(
            project_root,
            &options.ignore_dirs,
            &options.ignore_paths,
            &options.include,
            &options.exclude,
        )
        .with_extension_overrides(Arc::clone(&options.extensions));
        Self {
            policy,
            ignore_dirs: options.ignore_dirs.clone(),
            ignore_paths: options.ignore_paths.clone(),
            include: options.include.clone(),
            exclude: options.exclude.clone(),
            extensions: Arc::clone(&options.extensions),
            debounce: options.debounce,
            enabled: !options.no_watch,
        }
    }

    fn rebuild_policy(&mut self, project_root: &Path) {
        self.policy = WatchPolicy::with_config(
            project_root,
            &self.ignore_dirs,
            &self.ignore_paths,
            &self.include,
            &self.exclude,
        )
        .with_extension_overrides(Arc::clone(&self.extensions));
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ControlChanges {
    config_toml: bool,
    codegraph_json: bool,
    root_gitignore: bool,
}

impl ControlChanges {
    fn any(self) -> bool {
        self.config_toml || self.codegraph_json || self.root_gitignore
    }
}

#[derive(Debug, Clone)]
struct ControlFiles {
    config_toml: String,
    codegraph_json: String,
    root_gitignore: String,
}

impl ControlFiles {
    fn new(project_root: &Path, paths: &IndexPaths) -> Self {
        Self {
            config_toml: relative_path(project_root, &paths.config_toml()),
            codegraph_json: relative_path(project_root, &paths.extension_config()),
            root_gitignore: ".gitignore".to_string(),
        }
    }

    fn classify(&self, relative: &str, changes: &mut ControlChanges) -> bool {
        if relative == self.config_toml {
            changes.config_toml = true;
            true
        } else if relative == self.codegraph_json {
            changes.codegraph_json = true;
            true
        } else if relative == self.root_gitignore {
            changes.root_gitignore = true;
            true
        } else {
            false
        }
    }
}

fn relative_path(project_root: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(project_root) {
        return relative.to_string_lossy().replace('\\', "/");
    }

    // `IndexPaths` keeps canonical physical paths. On Windows canonicalization
    // commonly adds the verbatim `\\?\` prefix, while a caller may still hold
    // the equivalent ordinary `C:\...` spelling. Compare their slash-normalized
    // forms after removing that prefix so project-control files remain relative.
    let root = native_path_string(project_root);
    let candidate = native_path_string(path);
    let prefix = format!("{}/", root.trim_end_matches('/'));
    candidate
        .strip_prefix(&prefix)
        .unwrap_or(&candidate)
        .to_string()
}

fn native_path_string(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = normalized.strip_prefix("//?/") {
        rest.to_string()
    } else {
        normalized
    }
}

fn reload_runtime_scope(
    project_root: &Path,
    paths: &IndexPaths,
    previous: &RuntimeWatchScope,
    changes: ControlChanges,
) -> (RuntimeWatchScope, bool, Option<String>) {
    let mut next = previous.clone();
    let mut applied = changes.root_gitignore;
    let mut error = None;

    if changes.config_toml {
        match Config::load_for_paths(None, paths) {
            Ok(config) => {
                next.ignore_dirs = config.indexing.ignore_dirs.clone();
                next.ignore_paths = config.indexing.ignore_paths.clone();
                next.include = config.indexing.include.clone();
                next.exclude = config.indexing.exclude.clone();
                next.debounce = debounce_from_env_or(config.watch.debounce_ms);
                next.enabled = config.watch.enabled;
                applied = true;
            }
            Err(err) => {
                error = Some(format!(
                    "watch config reload failed; keeping the last valid config: {err}"
                ));
            }
        }
    }
    if changes.codegraph_json {
        // JSON is deliberately tolerant: malformed content produces empty
        // overrides plus its existing warning, matching every other loader.
        next.extensions = ExtensionOverrides::load_for_paths(paths);
        applied = true;
    }
    if applied {
        next.rebuild_policy(project_root);
    }
    (next, applied, error)
}

// libc errnos used to classify a backend watch failure. Hard-coded (rather than
// pulling in `libc`) because these three values are stable across every Unix the
// project targets; on Windows `raw_os_error()` returns the Win32 code, which will
// never match these, so the error falls through to the non-degrading `Other` arm.
const EMFILE: i32 = 24; // per-process fd table exhausted
const ENFILE: i32 = 23; // system-wide file table exhausted
const ENOSPC: i32 = 28; // inotify max_user_watches exhausted (Linux)

/// Upper bound for the lock-contention retry backoff (upstream
/// `sync/watcher.ts` caps the retry sleep at 30s before degrading).
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How many CONSECUTIVE non-contention sync errors are tolerated before
/// auto-sync is disabled (upstream #1127). A repeatable persistent error (schema
/// mismatch, permission denied, corrupt DB) would otherwise retry forever every
/// debounce; after this many in a row the watcher degrades with an actionable
/// message. A single success resets the count, so a transient hiccup is
/// unaffected.
const MAX_CONSECUTIVE_SYNC_ERRORS: u32 = 5;

/// How a backend watch error is handled.
///
/// * `Degrade` — fd / file-table exhaustion (`EMFILE`/`ENFILE`): the watcher can
///   never recover on its own, so it degrades permanently and the index falls
///   back to manual sync.
/// * `Warn` — inotify watch-count exhaustion (`ENOSPC`): a soft limit the user
///   can raise; warn but keep running (#893).
/// * `Other` — any error without one of those errnos: surfaced as a
///   non-degrading sync error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchErrorClass {
    Degrade,
    Warn,
    Other,
}

/// Classify a raw `io::Error` from the watch backend into a handling decision.
///
/// Pure and total over `raw_os_error()`, so the degraded machinery can be unit
/// tested with `io::Error::from_raw_os_error(..)` without real fd exhaustion.
pub fn classify_watch_error(err: &io::Error) -> WatchErrorClass {
    match err.raw_os_error() {
        Some(EMFILE) | Some(ENFILE) => WatchErrorClass::Degrade,
        Some(ENOSPC) => WatchErrorClass::Warn,
        _ => WatchErrorClass::Other,
    }
}

/// Classify a `notify::Error` by extracting its underlying `io::Error`.
///
/// `notify` wraps OS failures in `ErrorKind::Io`; its `MaxFilesWatch` variant is
/// the cross-platform spelling of inotify exhaustion, so it maps to `Warn`. Any
/// other kind has no recoverable errno and is treated as `Other`.
fn classify_notify_error(err: &notify::Error) -> WatchErrorClass {
    match &err.kind {
        notify::ErrorKind::Io(io_err) => classify_watch_error(io_err),
        notify::ErrorKind::MaxFilesWatch => WatchErrorClass::Warn,
        _ => WatchErrorClass::Other,
    }
}

// Each production target constructs exactly one variant; unit tests exercise both.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchBackend {
    NativeRecursive,
    PerDirNonRecursive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchRegistration {
    SingleRootRecursive,
    PerDirNonRecursive,
}

#[cfg(windows)]
fn platform_watch_backend() -> WatchBackend {
    WatchBackend::NativeRecursive
}

#[cfg(target_os = "macos")]
fn platform_watch_backend() -> WatchBackend {
    WatchBackend::NativeRecursive
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_watch_backend() -> WatchBackend {
    WatchBackend::PerDirNonRecursive
}

fn watch_registration(backend: WatchBackend) -> WatchRegistration {
    match backend {
        WatchBackend::NativeRecursive => WatchRegistration::SingleRootRecursive,
        WatchBackend::PerDirNonRecursive => WatchRegistration::PerDirNonRecursive,
    }
}

/// Walk `root` and collect every directory that should be watched, PRUNING any
/// subtree the [`WatchPolicy`] excludes.
///
/// # Why this exists (inotify exhaustion)
///
/// On Linux `notify`'s `RecursiveMode::Recursive` registers one inotify watch
/// per subdirectory. A blanket recursive watch on a large root (e.g. a home dir
/// holding Python `site-packages`, `node_modules`, `.venv`, `__pycache__`,
/// `.godot`, …) registers tens of thousands of watches at startup: it exhausts
/// the kernel's `max_user_watches` limit (the "OS file watch limit reached"
/// warning) AND stalls MCP startup while every watch is registered. By pruning
/// ignored subtrees here and registering a NON-recursive watch per surviving
/// directory, we only ever hold inotify watches for source directories the index
/// actually cares about — and we never even `stat` into `site-packages`.
///
/// The walk reuses the SAME [`WatchPolicy`] already built in
/// [`ProjectWatcher::start`] (its `normalize_relative` + `should_watch_dir`), so
/// the watch-registration set matches the extract-side ignore set exactly. When a
/// directory is ignored, the walk does not descend into it at all.
///
/// Pure and deterministic: the result is sorted, always includes `root` itself,
/// and `read_dir`/metadata errors on any subdir are tolerated (that subdir is
/// skipped, the walk continues) so a transient FS error never panics startup.
fn collect_watch_dirs(root: &Path, policy: &WatchPolicy) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Explicit stack DFS (no recursion) so a deep tree can't blow the stack.
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            // Tolerate permission / transient errors: skip this dir, keep going.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Only directories add inotify watches. Use `file_type()` (no extra
            // stat syscall via DirEntry) and skip symlinks to avoid cycles.
            let is_dir = match entry.file_type() {
                Ok(ft) => ft.is_dir() && !ft.is_symlink(),
                Err(_) => continue,
            };
            if !is_dir {
                continue;
            }
            match policy.normalize_relative(&path) {
                // PRUNE: do not descend into an ignored subtree at all. This is
                // what keeps us out of node_modules/.venv/__pycache__/.git/etc.
                Some(relative) if !policy.should_watch_dir(&relative) => continue,
                // A path that doesn't normalize (escape / root) is not pushed.
                None => continue,
                Some(_) => stack.push(path),
            }
        }
    }
    dirs.sort();
    dirs
}

fn known_directory_paths(root: &Path, policy: &WatchPolicy) -> BTreeSet<String> {
    collect_watch_dirs(root, policy)
        .into_iter()
        .filter_map(|dir| policy.normalize_relative(dir))
        .collect()
}

fn initial_watch_targets(
    backend: WatchBackend,
    project_root: &Path,
    policy: &WatchPolicy,
) -> Vec<(PathBuf, RecursiveMode)> {
    match watch_registration(backend) {
        WatchRegistration::SingleRootRecursive => {
            vec![(project_root.to_path_buf(), RecursiveMode::Recursive)]
        }
        WatchRegistration::PerDirNonRecursive => collect_watch_dirs(project_root, policy)
            .into_iter()
            .map(|dir| (dir, RecursiveMode::NonRecursive))
            .collect(),
    }
}

fn register_new_dirs_with(
    backend: WatchBackend,
    policy: &WatchPolicy,
    new_dir: &Path,
    mut register: impl FnMut(&Path, RecursiveMode),
) {
    if watch_registration(backend) == WatchRegistration::SingleRootRecursive {
        return;
    }
    for dir in collect_watch_dirs(new_dir, policy) {
        register(&dir, RecursiveMode::NonRecursive);
    }
}

/// Register a NonRecursive watch for a newly-created directory `new_dir` and all
/// of its non-ignored descendants (a `mkdir -p a/b/c` surfaces one create event,
/// so the subtree must be re-walked) on per-directory backends. Windows and macOS
/// use one native recursive root watch, so this is intentionally a no-op there.
fn register_new_dirs(watcher: &SharedWatcher, policy: &WatchPolicy, new_dir: &Path) {
    let backend = platform_watch_backend();
    if watch_registration(backend) == WatchRegistration::SingleRootRecursive {
        return;
    }
    let Ok(mut guard) = watcher.lock() else {
        return;
    };
    let Some(watcher) = guard.as_mut() else {
        return;
    };
    register_new_dirs_with(backend, policy, new_dir, |dir, mode| {
        let _ = watcher.watch(dir, mode);
    });
}

fn reconcile_watch_dirs(
    watcher: &SharedWatcher,
    project_root: &Path,
    policy: &WatchPolicy,
    known_dirs: &mut BTreeSet<String>,
    degraded: &Arc<DegradedState>,
    on_degraded: &Option<NoticeCallback>,
    on_sync_error: &Option<NoticeCallback>,
) -> bool {
    let desired = known_directory_paths(project_root, policy);
    if watch_registration(platform_watch_backend()) == WatchRegistration::SingleRootRecursive {
        *known_dirs = desired;
        return true;
    }

    let Ok(mut guard) = watcher.lock() else {
        if let Some(callback) = on_sync_error {
            callback("watch scope reload could not lock the watcher".to_string());
        }
        return true;
    };
    let Some(watcher) = guard.as_mut() else {
        *known_dirs = desired;
        return true;
    };

    for relative in known_dirs.difference(&desired) {
        // A removed directory often no longer exists by the time this runs;
        // unwatch is best-effort in that case because the backend has already
        // dropped the path. The desired set remains authoritative.
        let _ = watcher.unwatch(&project_root.join(relative));
    }
    for relative in desired.difference(known_dirs) {
        let path = project_root.join(relative);
        if let Err(err) = watcher.watch(&path, RecursiveMode::NonRecursive) {
            match handle_watch_error(&err, degraded, on_degraded, on_sync_error) {
                WatchErrorClass::Degrade => return false,
                WatchErrorClass::Warn | WatchErrorClass::Other => {}
            }
        }
    }
    *known_dirs = desired;
    true
}

/// Double `prev` for the next backoff step, saturating at [`MAX_BACKOFF`].
///
/// A zero/sub-ms `prev` seeds the schedule at 1ms so the doubling progresses; the
/// result is guaranteed never to exceed 30s.
pub fn next_backoff(prev: Duration) -> Duration {
    let seed = if prev.is_zero() {
        Duration::from_millis(1)
    } else {
        prev
    };
    seed.saturating_mul(2).min(MAX_BACKOFF)
}

/// Shared degraded flag + reason, readable by [`ProjectWatcher`] accessors while
/// the event loop / setup path mutate it.
#[derive(Default)]
struct DegradedState {
    degraded: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl DegradedState {
    fn mark(&self, reason: String) {
        if let Ok(mut guard) = self.reason.lock() {
            *guard = Some(reason);
        }
        self.degraded.store(true, Ordering::SeqCst);
    }

    fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }

    fn reason(&self) -> Option<String> {
        self.reason.lock().ok().and_then(|guard| guard.clone())
    }
}

#[derive(Clone)]
pub struct WatchOptions {
    pub debounce: Duration,
    pub no_watch: bool,
    pub db_path: Option<PathBuf>,
    pub inert_for_tests: bool,
    pub on_sync_complete: Option<SyncCallback>,
    /// Called ONCE when the watcher degrades permanently (fd/file-table
    /// exhaustion). The argument is a human-readable reason for STDERR.
    pub on_degraded: Option<NoticeCallback>,
    /// Called for a non-degrading watch/sync error (e.g. inotify watch-count
    /// exhaustion). May fire more than once; the watcher keeps running.
    pub on_sync_error: Option<NoticeCallback>,
    /// The addressed project's `config.toml` indexing scope. Threaded into the
    /// [`WatchPolicy`] and default sync functions so watcher scope matches scan.
    pub ignore_dirs: Vec<String>,
    pub ignore_paths: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// The addressed project's custom extension→language overrides (its
    /// current-root `codegraph.json`), so the watcher HANDLES a file the project
    /// declared as source. Empty = built-in detection only.
    pub extensions: Arc<ExtensionOverrides>,
    sync_fn: Option<SyncFn>,
    /// Override for the whole-project sync a removed directory escalates to.
    /// Defaults to [`crate::sync::sync_project_once`]; tests inject a counter.
    full_sync_fn: Option<FullSyncFn>,
    /// Cooperative cancellation shared with the default sync closures, so a
    /// shutdown can refuse queued lease loops and interrupt a running one
    /// (frozen plan lines 598-601).
    cancel: SyncCancellation,
}

impl Default for WatchOptions {
    fn default() -> Self {
        let indexing = codegraph_core::config::IndexingConfig::default();
        Self {
            // Upstream default debounce is 2000ms (`watch-policy.ts` notes and
            // `watcher.ts:86-90,220-223`); env override is clamped [100ms, 60s].
            debounce: debounce_from_env(),
            no_watch: false,
            db_path: None,
            inert_for_tests: false,
            on_sync_complete: None,
            on_degraded: None,
            on_sync_error: None,
            ignore_dirs: indexing.ignore_dirs,
            ignore_paths: indexing.ignore_paths,
            include: indexing.include,
            exclude: indexing.exclude,
            extensions: ExtensionOverrides::empty(),
            sync_fn: None,
            full_sync_fn: None,
            cancel: SyncCancellation::new(),
        }
    }
}

impl WatchOptions {
    /// Build watch options from ONE project's immutable [`Config`] and its own
    /// extension overrides.
    ///
    /// Indexing scope and the debounce window come from that project, and
    /// `watch.enabled = false` disables watching for it. An explicit
    /// `CODEGRAPH_WATCH_DEBOUNCE_MS` still wins over config.
    /// Share `cancel` with this watcher's default sync closures so a shutdown
    /// can cancel queued and running lease loops.
    #[must_use]
    pub fn with_cancellation(mut self, cancel: SyncCancellation) -> Self {
        self.cancel = cancel;
        self
    }

    #[must_use]
    pub fn for_project(config: &Config, extensions: Arc<ExtensionOverrides>) -> Self {
        Self {
            debounce: debounce_from_env_or(config.watch.debounce_ms),
            no_watch: !config.watch.enabled,
            ignore_dirs: config.indexing.ignore_dirs.clone(),
            ignore_paths: config.indexing.ignore_paths.clone(),
            include: config.indexing.include.clone(),
            exclude: config.indexing.exclude.clone(),
            extensions,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFile {
    pub path: String,
    pub first_seen_ms: u128,
    pub last_seen_ms: u128,
}

pub struct ProjectWatcher {
    tx: Sender<LoopMessage>,
    thread: Option<JoinHandle<()>>,
    watcher: SharedWatcher,
    degraded: Arc<DegradedState>,
    cancel: SyncCancellation,
    /// Set by the event-loop thread as its LAST action. Lets a caller observe
    /// completion without joining, so a bounded shutdown never blocks on a
    /// still-running sync (see [`Self::begin_shutdown`]).
    finished: Arc<AtomicBool>,
}

pub fn start_serve_watcher(
    project_root: impl AsRef<Path>,
    options: WatchOptions,
) -> Result<Option<ProjectWatcher>> {
    ProjectWatcher::start(project_root, options)
}

/// Build [`WatchOptions`] from the ADDRESSED project's own immutable config.
///
/// Loads `<current_root>/config.toml` and `<current_root>/codegraph.json` through
/// the resolved [`IndexPaths`], so a caller that serves several projects in one
/// process (a shared daemon, a global HTTP server) gives each watcher that
/// project's include/exclude, debounce, enable flag, and extension overrides —
/// never another project's and never a process-global value.
pub fn watch_options_for_project(project_root: impl AsRef<Path>) -> Result<WatchOptions> {
    let paths = IndexPaths::resolve(
        project_root.as_ref(),
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )?;
    let config = Config::load_for_paths(None, &paths)?;
    let extensions = ExtensionOverrides::load_for_paths(&paths);
    Ok(WatchOptions::for_project(&config, extensions))
}

impl ProjectWatcher {
    pub fn start(project_root: impl AsRef<Path>, options: WatchOptions) -> Result<Option<Self>> {
        let requested_root = project_root.as_ref().to_path_buf();
        if watch_disabled_reason(&requested_root, options.no_watch).is_some() {
            return Ok(None);
        }
        let index_paths = Arc::new(IndexPaths::resolve(
            &requested_root,
            std::env::var("CODEGRAPH_DIR").ok().as_deref(),
        )?);
        // Keep every watcher path in the same physical namespace as IndexPaths.
        // This matters on Windows, where canonical paths carry a `\\?\` prefix:
        // mixing that spelling with the requested root makes absolute control
        // events fail `WatchPolicy::normalize_relative` before classification.
        let project_root = index_paths.project().to_path_buf();
        let control_files = ControlFiles::new(&project_root, &index_paths);
        let runtime_scope = RuntimeWatchScope::from_options(&project_root, &options);
        let policy = runtime_scope.policy.clone();
        let db_path = match options.db_path.clone() {
            Some(db) => db,
            None => index_paths.current_db(),
        };
        let cancel = options.cancel.clone();
        let sync_fn = options.sync_fn.clone().unwrap_or_else(|| {
            let project_root = project_root.clone();
            let db_path = db_path.clone();
            let cancel = cancel.clone();
            Arc::new(move |paths| {
                sync_changed_paths_cancellable(&project_root, &db_path, paths, &cancel)
            })
        });
        // A removed directory cannot be expressed as a path list (its tracked
        // descendants are only discoverable by diffing the whole index against
        // disk), so it escalates to the SAME full sync `codegraph sync` runs.
        let full_sync_fn = options.full_sync_fn.clone().unwrap_or_else(|| {
            let project_root = project_root.clone();
            let cancel = cancel.clone();
            Arc::new(move || sync_project_once_cancellable(&project_root, &cancel))
        });
        let (tx, rx) = mpsc::channel();
        let degraded = Arc::new(DegradedState::default());
        // Capture directory identity before registering the backend. Windows emits
        // `RemoveKind::Any` after deletion, so this pre-event snapshot is the only
        // deterministic way to distinguish a known directory from an extensionless
        // file without inspecting a path that no longer exists.
        let known_dirs = known_directory_paths(&project_root, &policy);

        // Build the OS watcher and register the pruned watch set BEFORE spawning
        // the loop, so its create-event handler can share the same watcher to add
        // watches for newly-created directories (Linux NonRecursive — see below).
        let watcher: SharedWatcher = if options.inert_for_tests {
            Arc::new(Mutex::new(None))
        } else {
            let callback_tx = tx.clone();
            let mut watcher =
                notify::recommended_watcher(move |event: notify::Result<Event>| match event {
                    Ok(event) => {
                        let _ = callback_tx.send(LoopMessage::Event(WatchEventBatch::from_event(
                            &event.kind,
                            event.paths,
                        )));
                    }
                    Err(err) => {
                        let _ = callback_tx.send(LoopMessage::WatchError(err));
                    }
                })?;
            // Windows ReadDirectoryChangesW and macOS FSEvents cover descendants
            // with one recursive root watch. Registering every directory separately
            // is both redundant and expensive on Windows (one 16 KiB buffer plus OS
            // handles per registration in notify v6). Other targets retain the
            // pruned per-directory NonRecursive strategy that prevents Linux
            // inotify exhaustion.
            let backend = platform_watch_backend();
            let mut targets = initial_watch_targets(backend, &project_root, &policy);
            // The index root is structurally ignored source, but its two
            // project-control files must still be observed. Per-directory
            // backends therefore add one explicit non-recursive control watch.
            if watch_registration(backend) == WatchRegistration::PerDirNonRecursive
                && index_paths.current_root().is_dir()
                && !targets
                    .iter()
                    .any(|(dir, _)| dir == index_paths.current_root())
            {
                targets.push((
                    index_paths.current_root().to_path_buf(),
                    RecursiveMode::NonRecursive,
                ));
            }
            let mut watch_err: Option<notify::Error> = None;
            for (dir, mode) in &targets {
                if let Err(err) = watcher.watch(dir, *mode) {
                    let is_root = dir == &project_root;
                    match classify_notify_error(&err) {
                        // fd/file-table exhaustion can never recover: degrade once.
                        // A failure on the root is fatal to coverage; on a subdir we
                        // still degrade (the watch is incomplete and won't recover).
                        WatchErrorClass::Degrade => {
                            let reason = format!("watch {} failed: {err}", dir.display());
                            if !degraded.is_degraded() {
                                degraded.mark(reason.clone());
                                if let Some(cb) = &options.on_degraded {
                                    cb(reason);
                                }
                            }
                        }
                        // inotify watch-count exhaustion: a soft, user-raisable limit.
                        // Warn and keep the watches we did manage to register.
                        WatchErrorClass::Warn => {
                            if let Some(cb) = &options.on_sync_error {
                                cb(format!("watch {} warning: {err}", dir.display()));
                            }
                        }
                        // A non-recoverable error on the ROOT means we have no watch at
                        // all: surface it. On a subdir, remember the first one but keep
                        // going so a single bad dir doesn't sink the whole watcher.
                        WatchErrorClass::Other => {
                            if is_root {
                                return Err(anyhow::Error::new(err)
                                    .context(format!("watch {}", dir.display())));
                            }
                            watch_err.get_or_insert(err);
                        }
                    }
                }
            }
            if let (Some(err), Some(cb)) = (&watch_err, &options.on_sync_error) {
                cb(format!("watch (partial) warning: {err}"));
            }
            Arc::new(Mutex::new(Some(watcher)))
        };

        let on_sync_complete = options.on_sync_complete.clone();
        let on_degraded = options.on_degraded.clone();
        let on_sync_error = options.on_sync_error.clone();
        let loop_degraded = Arc::clone(&degraded);
        let loop_watcher = Arc::clone(&watcher);
        let finished = Arc::new(AtomicBool::new(false));
        let loop_finished = Arc::clone(&finished);
        let thread = thread::spawn(move || {
            event_loop(EventLoopCtx {
                rx,
                project_root,
                index_paths,
                control_files,
                runtime_scope,
                sync_fn,
                full_sync_fn,
                on_sync_complete,
                on_degraded,
                on_sync_error,
                degraded: loop_degraded,
                watcher: loop_watcher,
                known_dirs,
            });
            loop_finished.store(true, Ordering::SeqCst);
        });

        Ok(Some(Self {
            tx,
            thread: Some(thread),
            watcher,
            degraded,
            cancel,
            finished,
        }))
    }

    /// Begin shutdown WITHOUT joining: stop delivering new OS events, refuse
    /// queued lease loops, interrupt a running one, and ask the event loop to
    /// exit. Idempotent and never blocks.
    ///
    /// The join is deliberately separated from this signal. A running sync can
    /// hold the event-loop thread for the whole extraction pass, so joining here
    /// (as `stop`/`Drop` do) would block past any caller-side deadline and make a
    /// bounded drain unbounded. Callers poll [`Self::is_finished`] against their
    /// own budget and then either [`Self::stop`] (an instant join) or
    /// [`Self::detach`].
    pub fn begin_shutdown(&self) {
        self.cancel.cancel();
        if let Ok(mut guard) = self.watcher.lock() {
            let _ = guard.take();
        }
        let _ = self.tx.send(LoopMessage::Stop);
    }

    /// Whether the event-loop thread has run to completion.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Give up ownership of the event-loop thread without joining it.
    ///
    /// Used only after a bounded shutdown deadline elapsed: the loop is already
    /// cancelled and will exit on its own, and the caller has already reported an
    /// INCOMPLETE drain, so nothing destructive proceeds behind it. Dropping the
    /// `JoinHandle` detaches the thread, and the subsequent `Drop` finds no handle
    /// to join.
    pub fn detach(mut self) {
        let _ = self.thread.take();
    }

    /// The watcher's cooperative cancellation handle. A shutdown cancels it so
    /// queued lease loops refuse immediately and a running one returns a typed
    /// error, then waits for [`SyncCancellation::active_syncs`] to reach zero
    /// before considering the watcher drained.
    #[must_use]
    pub fn cancellation(&self) -> SyncCancellation {
        self.cancel.clone()
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded.is_degraded()
    }

    pub fn degraded_reason(&self) -> Option<String> {
        self.degraded.reason()
    }

    pub fn ingest_event_for_tests(&self, relative: impl Into<PathBuf>) {
        let _ = self.tx.send(LoopMessage::Event(WatchEventBatch::paths(vec![
            relative.into(),
        ])));
    }

    /// Feed a REMOVED-DIRECTORY event, the notify `Remove(RemoveKind::Folder)`
    /// shape, without needing a real OS watcher.
    pub fn ingest_removed_dir_for_tests(&self, relative: impl Into<PathBuf>) {
        let _ = self.tx.send(LoopMessage::Event(WatchEventBatch::from_event(
            &EventKind::Remove(RemoveKind::Folder),
            vec![relative.into()],
        )));
    }

    /// Feed the ambiguous removal shape emitted by Windows
    /// `ReadDirectoryChangesW`, without requiring a native Windows runner.
    pub fn ingest_ambiguous_remove_for_tests(&self, relative: impl Into<PathBuf>) {
        let _ = self.tx.send(LoopMessage::Event(WatchEventBatch::from_event(
            &EventKind::Remove(RemoveKind::Any),
            vec![relative.into()],
        )));
    }

    #[cfg(test)]
    fn flush_for_tests(&self) {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(LoopMessage::Flush(tx));
        rx.recv_timeout(Duration::from_secs(1))
            .expect("event loop accepted deterministic flush");
    }

    pub fn pending_files(&self) -> Vec<PendingFile> {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(LoopMessage::Snapshot(tx));
        rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default()
    }

    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        // Signal first (never join without cancelling: the event-loop thread may be
        // inside a bounded lease acquisition), then join.
        self.begin_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ProjectWatcher {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

enum LoopMessage {
    Event(WatchEventBatch),
    WatchError(notify::Error),
    Snapshot(Sender<Vec<PendingFile>>),
    #[cfg(test)]
    Flush(Sender<()>),
    Stop,
}

/// One notify event, reduced to exactly what the debounce loop needs: the paths,
/// plus the backend's removal classification.
///
/// The distinction has to travel with the event because it cannot be recovered
/// later: a removed directory is already gone from disk, so `path.is_dir()` is
/// false and its extensionless name is indistinguishable from a deleted
/// extensionless file. Carrying the notify `EventKind` forward keeps the
/// escalation deterministic instead of guessing from a missing path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WatchEventBatch {
    paths: Vec<PathBuf>,
    removal: RemovalHint,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RemovalHint {
    #[default]
    None,
    Directory,
    Ambiguous,
}

impl WatchEventBatch {
    fn paths(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            removal: RemovalHint::None,
        }
    }

    fn from_event(kind: &EventKind, paths: Vec<PathBuf>) -> Self {
        let removal = match kind {
            EventKind::Remove(RemoveKind::Folder) => RemovalHint::Directory,
            EventKind::Remove(RemoveKind::Any) => RemovalHint::Ambiguous,
            _ => RemovalHint::None,
        };
        Self { paths, removal }
    }
}

fn classify_removed_directory(
    hint: RemovalHint,
    relative: &str,
    known_dirs: &mut BTreeSet<String>,
) -> bool {
    let removed_dir = match hint {
        RemovalHint::Directory => true,
        RemovalHint::Ambiguous => known_dirs.contains(relative),
        RemovalHint::None => false,
    };
    if removed_dir {
        let descendant_prefix = format!("{relative}/");
        known_dirs.retain(|known| known != relative && !known.starts_with(&descendant_prefix));
    }
    removed_dir
}

#[derive(Debug, Clone)]
struct PendingInfo {
    first_seen_ms: u128,
    last_seen_ms: u128,
}

struct EventLoopCtx {
    rx: mpsc::Receiver<LoopMessage>,
    project_root: PathBuf,
    index_paths: Arc<IndexPaths>,
    control_files: ControlFiles,
    runtime_scope: RuntimeWatchScope,
    sync_fn: SyncFn,
    full_sync_fn: FullSyncFn,
    on_sync_complete: Option<SyncCallback>,
    on_degraded: Option<NoticeCallback>,
    on_sync_error: Option<NoticeCallback>,
    degraded: Arc<DegradedState>,
    watcher: SharedWatcher,
    known_dirs: BTreeSet<String>,
}

fn event_loop(ctx: EventLoopCtx) {
    let EventLoopCtx {
        rx,
        project_root,
        index_paths,
        control_files,
        mut runtime_scope,
        sync_fn,
        full_sync_fn,
        on_sync_complete,
        on_degraded,
        on_sync_error,
        degraded,
        watcher,
        mut known_dirs,
    } = ctx;
    let mut pending = BTreeMap::<String, PendingInfo>::new();
    let mut deadline = None::<Instant>;
    let mut consecutive_sync_errors = 0u32;
    // Set by a removed-directory event in the current burst. It DOMINATES the
    // per-path list (one full sync instead of N incremental ones) yet still
    // flushes exactly once, on the same debounce deadline.
    let mut full_sync_pending = false;
    loop {
        let message = match deadline {
            Some(when) => match rx.recv_timeout(when.saturating_duration_since(Instant::now())) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };

        match message {
            Some(LoopMessage::Event(batch)) => {
                let WatchEventBatch { paths, removal } = batch;
                let mut control_changes = ControlChanges::default();
                let mut normalized = Vec::new();
                for path in paths {
                    if let Some(relative) = runtime_scope.policy.normalize_relative(&path) {
                        let is_control = control_files.classify(&relative, &mut control_changes);
                        normalized.push((path, relative, is_control));
                    }
                }

                // Project control files are recognized BEFORE ordinary
                // include/exclude filtering. A successful reload swaps the
                // complete runtime scope at once, reconciles the OS watch set,
                // and makes one full scan dominate every queued path delta.
                if control_changes.any() {
                    let (next, applied, reload_error) = reload_runtime_scope(
                        &project_root,
                        &index_paths,
                        &runtime_scope,
                        control_changes,
                    );
                    if let Some(reason) = reload_error
                        && let Some(callback) = &on_sync_error
                    {
                        callback(reason);
                    }
                    if applied {
                        if !reconcile_watch_dirs(
                            &watcher,
                            &project_root,
                            &next.policy,
                            &mut known_dirs,
                            &degraded,
                            &on_degraded,
                            &on_sync_error,
                        ) {
                            break;
                        }
                        runtime_scope = next;
                        full_sync_pending = true;
                        let now = epoch_millis();
                        for (_, relative, is_control) in &normalized {
                            if *is_control {
                                pending
                                    .entry(relative.clone())
                                    .and_modify(|info| info.last_seen_ms = now)
                                    .or_insert(PendingInfo {
                                        first_seen_ms: now,
                                        last_seen_ms: now,
                                    });
                            }
                        }
                    }
                }

                for (path, relative, is_control) in normalized {
                    if is_control || !runtime_scope.enabled {
                        continue;
                    }

                    // A removed DIRECTORY bypasses extension filtering: it has
                    // no source extension, so the file gate below would drop it
                    // and every tracked descendant would linger in the index
                    // forever. The watch policy still applies (an ignored dir is
                    // still ignored), and the removal escalates the burst to one
                    // full sync — the only pass that can find those descendants.
                    if classify_removed_directory(removal, &relative, &mut known_dirs) {
                        if runtime_scope.policy.should_watch_dir(&relative) {
                            full_sync_pending = true;
                            let now = epoch_millis();
                            pending
                                .entry(relative)
                                .and_modify(|info| info.last_seen_ms = now)
                                .or_insert(PendingInfo {
                                    first_seen_ms: now,
                                    last_seen_ms: now,
                                });
                        }
                        continue;
                    }
                    // A brand-new non-ignored directory holds no inotify watch
                    // yet (Linux watches are per-dir NonRecursive — see
                    // `collect_watch_dirs`). Register it (and any non-ignored
                    // descendants created in the same burst, e.g. `mkdir -p`) so
                    // edits inside it are seen without a server restart.
                    if path.is_dir() && runtime_scope.policy.should_watch_dir(&relative) {
                        register_new_dirs(&watcher, &runtime_scope.policy, &path);
                        known_dirs.extend(known_directory_paths(&path, &runtime_scope.policy));
                    }
                    if runtime_scope.policy.should_handle_file(&relative)
                        || (runtime_scope.policy.allows_file_path(&relative)
                            && maybe_deleted_source(&relative))
                    {
                        let now = epoch_millis();
                        pending
                            .entry(relative)
                            .and_modify(|info| info.last_seen_ms = now)
                            .or_insert(PendingInfo {
                                first_seen_ms: now,
                                last_seen_ms: now,
                            });
                    }
                }
                if !pending.is_empty() {
                    // Resetting the timer on every event ports the upstream exactly-once
                    // burst semantics (`upstream sync/watcher.ts:529-540`).
                    deadline = Some(Instant::now() + runtime_scope.debounce);
                }
            }
            Some(LoopMessage::WatchError(err)) => {
                match handle_watch_error(&err, &degraded, &on_degraded, &on_sync_error) {
                    WatchErrorClass::Degrade => break,
                    WatchErrorClass::Warn | WatchErrorClass::Other => {}
                }
            }
            Some(LoopMessage::Snapshot(reply)) => {
                let _ = reply.send(snapshot(&pending));
            }
            #[cfg(test)]
            Some(LoopMessage::Flush(reply)) => {
                if !pending.is_empty() {
                    deadline = Some(Instant::now());
                }
                let _ = reply.send(());
            }
            Some(LoopMessage::Stop) => break,
            None => {
                let paths = pending.keys().cloned().collect::<Vec<_>>();
                pending.clear();
                deadline = None;
                let full_sync = std::mem::take(&mut full_sync_pending);
                let attempt = if full_sync {
                    run_full_sync_with_backoff(&full_sync_fn)
                } else {
                    run_sync_with_backoff(&sync_fn, paths.clone())
                };
                let decision = classify_persistent_failure(&attempt, &mut consecutive_sync_errors);
                match attempt {
                    SyncAttempt::Done(outcome) => {
                        // Preserve the event batch independently of actual DB
                        // mutations. Startup catch-up can win the writer lease and
                        // index the same file first; the watcher still handled this
                        // event and its callback must be able to report that fact.
                        let outcome = with_trigger_paths(outcome, paths);
                        if let Some(callback) = &on_sync_complete {
                            callback(outcome);
                        }
                    }
                    SyncAttempt::Error(reason) => {
                        if let Some(cb) = &on_sync_error {
                            cb(reason);
                        }
                    }
                    SyncAttempt::Degraded(_) => {}
                }
                if let PersistentFailure::Degrade(reason) | PersistentFailure::Disable(reason) =
                    decision
                {
                    if !degraded.is_degraded() {
                        degraded.mark(reason.clone());
                        if let Some(cb) = &on_degraded {
                            cb(reason);
                        }
                    }
                    break;
                }
            }
        }
    }
}

fn with_trigger_paths(mut outcome: SyncOutcome, trigger_paths: Vec<String>) -> SyncOutcome {
    outcome.trigger_paths = trigger_paths;
    outcome
}

/// Apply the EMFILE/ENFILE → degrade-once, ENOSPC → warn classification to a
/// backend watch error. Returns the class so the event loop can stop the watch
/// on `Degrade`. `on_degraded` fires at most once across the watcher's life.
fn handle_watch_error(
    err: &notify::Error,
    degraded: &Arc<DegradedState>,
    on_degraded: &Option<NoticeCallback>,
    on_sync_error: &Option<NoticeCallback>,
) -> WatchErrorClass {
    let class = classify_notify_error(err);
    match class {
        WatchErrorClass::Degrade => {
            if !degraded.is_degraded() {
                let reason = format!("file watcher backend error: {err}");
                degraded.mark(reason.clone());
                if let Some(cb) = on_degraded {
                    cb(reason);
                }
            }
        }
        WatchErrorClass::Warn | WatchErrorClass::Other => {
            if let Some(cb) = on_sync_error {
                cb(format!("file watcher warning: {err}"));
            }
        }
    }
    class
}

enum SyncAttempt {
    Done(SyncOutcome),
    Degraded(String),
    Error(String),
}

/// What the event loop should do with a completed [`SyncAttempt`], after the
/// consecutive-error counter has been folded in (upstream #1127).
#[derive(Debug)]
enum PersistentFailure {
    /// Keep running: report the outcome via the usual callbacks, if any.
    Surface,
    /// Lock-contention degrade — pass the reason straight through unchanged.
    Degrade(String),
    /// Auto-sync has failed persistently; degrade with this actionable message.
    Disable(String),
}

/// Fold a [`SyncAttempt`] into the consecutive-error counter and decide the
/// event loop's next move (upstream #1127). A `Done` resets the counter; an
/// `Error` increments it and, once [`MAX_CONSECUTIVE_SYNC_ERRORS`] in a row is
/// reached, escalates to [`PersistentFailure::Disable`] with a message that
/// names the underlying error and points at `codegraph sync`. Contention
/// (`Degraded`) is passed through untouched and never counts as a failure.
fn classify_persistent_failure(
    attempt: &SyncAttempt,
    consecutive_errors: &mut u32,
) -> PersistentFailure {
    match attempt {
        SyncAttempt::Done(_) => {
            *consecutive_errors = 0;
            PersistentFailure::Surface
        }
        SyncAttempt::Degraded(reason) => PersistentFailure::Degrade(reason.clone()),
        SyncAttempt::Error(reason) => {
            *consecutive_errors += 1;
            if *consecutive_errors >= MAX_CONSECUTIVE_SYNC_ERRORS {
                PersistentFailure::Disable(format!(
                    "auto-sync disabled after {MAX_CONSECUTIVE_SYNC_ERRORS} consecutive failures \
                     ({reason}); run `codegraph sync` to reindex once the cause is fixed"
                ))
            } else {
                PersistentFailure::Surface
            }
        }
    }
}

/// Run `sync_fn`, retrying on write-lock contention with bounded exponential
/// backoff capped at [`MAX_BACKOFF`]. Once the cumulative sleep budget is spent
/// the watcher degrades; any non-contention error is surfaced as a sync error.
fn run_sync_with_backoff(sync_fn: &SyncFn, paths: Vec<String>) -> SyncAttempt {
    run_sync_with_backoff_inner(sync_fn, paths, MAX_BACKOFF, thread::sleep)
}

/// Same bounded-backoff contract as [`run_sync_with_backoff`], for the
/// whole-project sync a removed directory escalates to.
fn run_full_sync_with_backoff(full_sync_fn: &FullSyncFn) -> SyncAttempt {
    run_with_backoff(MAX_BACKOFF, thread::sleep, || full_sync_fn())
}

/// Inner retry loop with an injectable budget and sleeper so the cap can be
/// unit-tested without sleeping a real 30 seconds.
fn run_sync_with_backoff_inner(
    sync_fn: &SyncFn,
    paths: Vec<String>,
    budget: Duration,
    sleeper: impl FnMut(Duration),
) -> SyncAttempt {
    run_with_backoff(budget, sleeper, || sync_fn(paths.clone()))
}

/// The shared retry policy: retry on write-lock contention with bounded
/// exponential backoff capped at [`MAX_BACKOFF`], degrade once the cumulative
/// sleep budget is spent, and surface any non-contention error immediately.
fn run_with_backoff(
    budget: Duration,
    mut sleeper: impl FnMut(Duration),
    mut run: impl FnMut() -> Result<SyncOutcome>,
) -> SyncAttempt {
    let mut backoff = Duration::ZERO;
    let mut slept = Duration::ZERO;
    loop {
        match run() {
            Ok(outcome) => return SyncAttempt::Done(outcome),
            Err(err) => {
                if !is_lock_contention(&err) {
                    return SyncAttempt::Error(format!("sync failed: {err}"));
                }
                if slept >= budget {
                    return SyncAttempt::Degraded(format!(
                        "sync write-lock contention exceeded {}s budget: {err}",
                        MAX_BACKOFF.as_secs()
                    ));
                }
                backoff = next_backoff(backoff);
                sleeper(backoff);
                slept = slept.saturating_add(backoff);
            }
        }
    }
}

/// A sync error is "lock contention" iff its chain mentions a busy/locked DB,
/// which is the only error worth retrying with backoff.
fn is_lock_contention(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}").to_ascii_lowercase();
    text.contains("locked") || text.contains("busy")
}

fn snapshot(pending: &BTreeMap<String, PendingInfo>) -> Vec<PendingFile> {
    pending
        .iter()
        .map(|(path, info)| PendingFile {
            path: path.clone(),
            first_seen_ms: info.first_seen_ms,
            last_seen_ms: info.last_seen_ms,
        })
        .collect()
}

fn maybe_deleted_source(relative: &str) -> bool {
    relative.rsplit_once('.').is_some_and(|(_, ext)| {
        codegraph_extract::engine::builtin_language_for_ext(&ext.to_ascii_lowercase()).is_some()
    })
}

fn debounce_from_env() -> Duration {
    debounce_from_env_or(2_000)
}

/// The debounce window: `CODEGRAPH_WATCH_DEBOUNCE_MS` when set (the documented
/// env escape hatch), else `fallback_ms` (a project's `watch.debounce_ms`, or the
/// upstream 2000ms default). Always clamped to [100ms, 60s].
fn debounce_from_env_or(fallback_ms: u64) -> Duration {
    let millis = std::env::var("CODEGRAPH_WATCH_DEBOUNCE_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(fallback_ms)
        .clamp(100, 60_000);
    Duration::from_millis(millis)
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    fn write_current_config(project_root: &Path, contents: &str) -> IndexPaths {
        let paths = IndexPaths::resolve(project_root, None).unwrap();
        fs::create_dir_all(paths.current_root()).unwrap();
        fs::write(paths.config_toml(), contents).unwrap();
        paths
    }

    fn live_project_options(project_root: &Path) -> WatchOptions {
        let paths = IndexPaths::resolve(project_root, None).unwrap();
        let config = Config::load_for_paths(None, &paths).unwrap();
        let extensions = ExtensionOverrides::load_for_paths(&paths);
        WatchOptions::for_project(&config, extensions)
    }

    #[test]
    fn noop_watcher_completion_preserves_trigger_paths() {
        let outcome = with_trigger_paths(
            SyncOutcome::default(),
            vec!["src/brand_new_symbol.ts".to_string()],
        );

        assert_eq!(outcome.files_reindexed, 0);
        assert!(outcome.changed_paths.is_empty());
        assert_eq!(
            outcome.trigger_paths,
            vec!["src/brand_new_symbol.ts".to_string()]
        );
    }

    #[test]
    fn relative_path_equates_windows_verbatim_and_regular_drive_paths() {
        assert_eq!(
            relative_path(
                Path::new("C:/Users/test/project"),
                Path::new("//?/C:/Users/test/project/.codegraph/config.toml"),
            ),
            ".codegraph/config.toml"
        );
        assert_eq!(
            native_path_string(Path::new("//?/UNC/server/share/project")),
            "//server/share/project"
        );
    }

    #[test]
    fn rapid_save_burst_triggers_exactly_one_reindex() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-debounce");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let db = crate::sync::default_db_path(dir.path()).unwrap();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&outcomes);
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                inert_for_tests: true,
                db_path: Some(db),
                on_sync_complete: Some(Arc::new(move |outcome| {
                    seen.lock().unwrap().push(outcome);
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        fs::write(
            dir.path().join("src/app.ts.__tmp"),
            "export function one() { return 1; }\n",
        )
        .unwrap();
        fs::rename(
            dir.path().join("src/app.ts.__tmp"),
            dir.path().join("src/app.ts"),
        )
        .unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function one() { return 1; }\n",
        )
        .unwrap();
        watcher.ingest_event_for_tests("src/app.ts.__tmp");
        watcher.ingest_event_for_tests("src/app.ts");
        watcher.ingest_event_for_tests("src/app.ts");
        std::thread::sleep(Duration::from_millis(220));
        watcher.stop();

        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].files_reindexed, 1);
        assert_eq!(outcomes[0].files_checked, 1);
    }

    #[test]
    fn deleted_directory_event_schedules_one_full_sync() {
        let _env = crate::test_env::env_guard();
        // A removed DIRECTORY has no source extension, so extension-based event
        // filtering drops it and every tracked descendant lingers in the index
        // forever. The watcher must escalate the removal to exactly ONE
        // full-project sync — the only pass that discovers every absent tracked
        // descendant — instead of an incremental path-scoped sync.
        let dir = crate::sync::tests::TestDir::new("watch-deleted-dir");
        fs::write(dir.path().join(".gitignore"), "Tools/\n").unwrap();
        let index_paths = IndexPaths::resolve(dir.path(), None).unwrap();
        fs::write(
            index_paths.config_toml(),
            "[app]\nname = \"watch-deleted-dir\"\n\n[indexing]\ninclude = [\"Tools/\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("Tools/feature")).unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Tools/feature/alpha.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Tools/feature/beta.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Tools/keep.ts"),
            "export function includedBefore() { return 3; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/excluded.ts"),
            "export function excludedBefore() { return 4; }\n",
        )
        .unwrap();
        let db = crate::sync::default_db_path(dir.path()).unwrap();
        let indexed = crate::sync::sync_changed_paths(
            dir.path(),
            &db,
            [
                "Tools/feature/alpha.ts",
                "Tools/feature/beta.ts",
                "Tools/keep.ts",
                "src/excluded.ts",
            ],
        )
        .unwrap();
        assert_eq!(
            indexed.files_reindexed, 4,
            "all fixture files indexed first"
        );

        // Both surviving files change before the directory removal so the
        // resulting stored symbols prove the full sync reloaded the project's
        // authoritative current config rather than a frozen startup snapshot.
        fs::write(
            dir.path().join("Tools/keep.ts"),
            "export function includedAfter() { return 30; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/excluded.ts"),
            "export function excludedAfter() { return 40; }\n",
        )
        .unwrap();

        let (outcome_tx, outcome_rx) = mpsc::channel();
        // An injected INCREMENTAL sync_fn that must never run: a directory
        // removal is not a per-path event.
        let incremental_calls = Arc::new(AtomicUsize::new(0));
        let incremental = Arc::clone(&incremental_calls);
        let sync_fn: SyncFn = Arc::new(move |_paths| {
            incremental.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome::default())
        });
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_secs(30),
                inert_for_tests: true,
                db_path: Some(db.clone()),
                sync_fn: Some(sync_fn),
                include: vec!["Tools/".to_string()],
                on_sync_complete: Some(Arc::new(move |outcome| {
                    outcome_tx.send(outcome).unwrap();
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        // When: the directory disappears and the backend reports the ambiguous
        // Windows removal shape. The watcher must recover directory identity from
        // its startup registration state rather than inspecting the missing path.
        fs::remove_dir_all(dir.path().join("Tools/feature")).unwrap();
        watcher.ingest_ambiguous_remove_for_tests("Tools/feature");
        watcher.flush_for_tests();
        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("removed-directory sync completion");
        watcher.stop();

        // Then: exactly one sync fired, it was the FULL sync (not the injected
        // incremental one), it removed both tracked descendants, and the removed
        // directory is reported as the trigger path.
        assert!(outcome_rx.try_recv().is_err(), "only one sync may complete");
        assert_eq!(
            incremental_calls.load(AtomicOrdering::SeqCst),
            0,
            "a directory removal must NOT take the incremental per-path sync"
        );
        assert_eq!(
            outcome.files_removed, 2,
            "the full sync must drop every tracked descendant of the missing dir"
        );
        assert_eq!(outcome.trigger_paths, vec!["Tools/feature".to_string()]);
        let store = codegraph_store::Store::open(&db).unwrap();
        for gone in ["Tools/feature/alpha.ts", "Tools/feature/beta.ts"] {
            assert!(
                store.file_by_path(gone).unwrap().is_none(),
                "{gone} must be gone from the store after the directory removal"
            );
        }
        let included_names = store
            .nodes_by_file_path("Tools/keep.ts")
            .unwrap()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(
            included_names.iter().any(|name| name == "includedAfter"),
            "watcher-local include must let full sync refresh Tools/keep.ts: {included_names:?}"
        );
        let excluded_names = store
            .nodes_by_file_path("src/excluded.ts")
            .unwrap()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(
            excluded_names.iter().any(|name| name == "excludedAfter"),
            "the current config must let full sync refresh src/excluded.ts: {excluded_names:?}"
        );
    }

    #[test]
    fn config_reload_reconciles_scope_and_dominates_queued_paths() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-config-reload");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("generated")).unwrap();
        fs::write(
            dir.path().join("src/keep.ts"),
            "export function keep() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("generated/drop.ts"),
            "export function drop() { return 2; }\n",
        )
        .unwrap();
        let paths = write_current_config(dir.path(), "[app]\nname = \"watch-config\"\n");
        let initial = crate::sync::sync_project_once(dir.path()).unwrap();
        assert_eq!(initial.files_reindexed, 2);

        let (outcome_tx, outcome_rx) = mpsc::channel();
        let mut options = live_project_options(dir.path());
        options.debounce = Duration::from_secs(30);
        options.inert_for_tests = true;
        options.on_sync_complete = Some(Arc::new(move |outcome| {
            outcome_tx.send(outcome).unwrap();
        }));
        let watcher = ProjectWatcher::start(dir.path(), options).unwrap().unwrap();
        let control = relative_path(dir.path(), &paths.config_toml());

        fs::write(
            paths.config_toml(),
            "[app]\nname = \"watch-config\"\n\n[indexing]\nexclude = [\"generated/\"]\n",
        )
        .unwrap();
        watcher.ingest_event_for_tests(&control);
        watcher.ingest_event_for_tests("generated/drop.ts");
        watcher.flush_for_tests();
        let excluded = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("scope-removal full sync");
        assert_eq!(excluded.files_removed, 1);
        assert_eq!(excluded.trigger_paths, vec![control.clone()]);
        assert!(
            codegraph_store::Store::open(&paths.current_db())
                .unwrap()
                .file_by_path("generated/drop.ts")
                .unwrap()
                .is_none(),
            "the full reconcile must remove a file excluded by the new scope"
        );

        fs::write(paths.config_toml(), "[app]\nname = \"watch-config\"\n").unwrap();
        watcher.ingest_event_for_tests(&control);
        watcher.flush_for_tests();
        let readmitted = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("scope-readmission full sync");
        assert_eq!(readmitted.files_reindexed, 1);
        assert!(
            codegraph_store::Store::open(&paths.current_db())
                .unwrap()
                .file_by_path("generated/drop.ts")
                .unwrap()
                .is_some(),
            "removing the exclusion must readmit the existing source file"
        );
        watcher.stop();
    }

    #[test]
    fn malformed_toml_keeps_last_valid_scope_and_reports_error() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-config-invalid");
        fs::create_dir_all(dir.path().join("generated")).unwrap();
        fs::write(
            dir.path().join("generated/drop.ts"),
            "export function drop() { return 1; }\n",
        )
        .unwrap();
        let paths = write_current_config(
            dir.path(),
            "[app]\nname = \"watch-config\"\n\n[indexing]\nexclude = [\"generated/\"]\n",
        );
        let control = relative_path(dir.path(), &paths.config_toml());
        let incremental_calls = Arc::new(AtomicUsize::new(0));
        let incremental_counter = Arc::clone(&incremental_calls);
        let full_calls = Arc::new(AtomicUsize::new(0));
        let full_counter = Arc::clone(&full_calls);
        let (error_tx, error_rx) = mpsc::channel();
        let mut options = live_project_options(dir.path());
        options.debounce = Duration::from_secs(30);
        options.inert_for_tests = true;
        options.sync_fn = Some(Arc::new(move |_| {
            incremental_counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome::default())
        }));
        options.full_sync_fn = Some(Arc::new(move || {
            full_counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome::default())
        }));
        options.on_sync_error = Some(Arc::new(move |error| {
            error_tx.send(error).unwrap();
        }));
        let watcher = ProjectWatcher::start(dir.path(), options).unwrap().unwrap();

        fs::write(paths.config_toml(), "[app\nthis is not toml").unwrap();
        watcher.ingest_event_for_tests(&control);
        watcher.flush_for_tests();
        let error = error_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("malformed config warning");
        assert!(error.contains("keeping the last valid config"), "{error}");

        watcher.ingest_event_for_tests("generated/drop.ts");
        watcher.flush_for_tests();
        assert!(watcher.pending_files().is_empty());
        watcher.stop();
        assert_eq!(incremental_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(full_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn extension_config_reload_handles_and_then_drops_custom_source() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-extension-reload");
        fs::write(
            dir.path().join("plugin.zz"),
            "local function plugin()\n  return 1\nend\n",
        )
        .unwrap();
        let paths = IndexPaths::resolve(dir.path(), None).unwrap();
        let control = relative_path(dir.path(), &paths.extension_config());
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let mut options = live_project_options(dir.path());
        options.debounce = Duration::from_secs(30);
        options.inert_for_tests = true;
        options.on_sync_complete = Some(Arc::new(move |outcome| {
            outcome_tx.send(outcome).unwrap();
        }));
        let watcher = ProjectWatcher::start(dir.path(), options).unwrap().unwrap();

        fs::write(paths.extension_config(), r#"{"extensions":{".zz":"lua"}}"#).unwrap();
        watcher.ingest_event_for_tests(&control);
        watcher.ingest_event_for_tests("plugin.zz");
        watcher.flush_for_tests();
        let added = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("extension-add full sync");
        assert_eq!(added.files_reindexed, 1);
        assert_eq!(
            added.trigger_paths,
            vec![control.clone(), "plugin.zz".to_string()]
        );
        assert!(
            codegraph_store::Store::open(&paths.current_db())
                .unwrap()
                .file_by_path("plugin.zz")
                .unwrap()
                .is_some()
        );

        fs::write(paths.extension_config(), "{ malformed").unwrap();
        watcher.ingest_event_for_tests(&control);
        watcher.flush_for_tests();
        let removed = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("tolerant extension reset full sync");
        assert_eq!(removed.files_removed, 1);
        assert!(
            codegraph_store::Store::open(&paths.current_db())
                .unwrap()
                .file_by_path("plugin.zz")
                .unwrap()
                .is_none(),
            "malformed JSON keeps the existing tolerant empty-override semantics"
        );
        watcher.stop();
    }

    #[test]
    fn root_gitignore_reload_reconciles_and_readmits_sources() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-gitignore-reload");
        fs::create_dir_all(dir.path().join("generated")).unwrap();
        fs::write(
            dir.path().join("generated/drop.ts"),
            "export function drop() { return 1; }\n",
        )
        .unwrap();
        let paths = IndexPaths::resolve(dir.path(), None).unwrap();
        let initial = crate::sync::sync_project_once(dir.path()).unwrap();
        assert_eq!(initial.files_reindexed, 1);
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let mut options = live_project_options(dir.path());
        options.debounce = Duration::from_secs(30);
        options.inert_for_tests = true;
        options.on_sync_complete = Some(Arc::new(move |outcome| {
            outcome_tx.send(outcome).unwrap();
        }));
        let watcher = ProjectWatcher::start(dir.path(), options).unwrap().unwrap();

        fs::write(dir.path().join(".gitignore"), "generated/\n").unwrap();
        watcher.ingest_event_for_tests(".gitignore");
        watcher.ingest_event_for_tests("generated/drop.ts");
        watcher.flush_for_tests();
        let removed = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("gitignore-removal full sync");
        assert_eq!(removed.files_removed, 1);
        assert_eq!(removed.trigger_paths, vec![".gitignore".to_string()]);

        fs::write(dir.path().join(".gitignore"), "").unwrap();
        watcher.ingest_event_for_tests(".gitignore");
        watcher.flush_for_tests();
        let readmitted = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("gitignore-readmission full sync");
        assert_eq!(readmitted.files_reindexed, 1);
        assert!(
            codegraph_store::Store::open(&paths.current_db())
                .unwrap()
                .file_by_path("generated/drop.ts")
                .unwrap()
                .is_some()
        );
        watcher.stop();
    }

    #[test]
    fn ambiguous_remove_of_known_directory_keeps_directory_semantics() {
        // Windows ReadDirectoryChangesW reports directory deletion as Any. The
        // current dirty implementation loses that identity unconditionally;
        // the corrected classifier must corroborate it from the watcher's known
        // directory state rather than from extension or the now-missing path.
        let dir = crate::sync::tests::TestDir::new("watch-ambiguous-remove");
        fs::create_dir_all(dir.path().join("src/feature")).unwrap();
        fs::write(dir.path().join("src/README"), "extensionless file\n").unwrap();
        let policy = WatchPolicy::new(dir.path());
        let mut known_dirs = known_directory_paths(dir.path(), &policy);
        fs::remove_dir_all(dir.path().join("src/feature")).unwrap();
        fs::remove_file(dir.path().join("src/README")).unwrap();

        let directory_batch = WatchEventBatch::from_event(
            &EventKind::Remove(RemoveKind::Any),
            vec![PathBuf::from("src/feature")],
        );
        assert!(
            classify_removed_directory(directory_batch.removal, "src/feature", &mut known_dirs,),
            "an ambiguous removal of a watcher-known directory must remain a directory removal"
        );

        let file_batch = WatchEventBatch::from_event(
            &EventKind::Remove(RemoveKind::Any),
            vec![PathBuf::from("src/README")],
        );
        assert!(
            !classify_removed_directory(file_batch.removal, "src/README", &mut known_dirs),
            "an extensionless deleted file must not be guessed to be a directory"
        );
    }

    #[test]
    fn real_watcher_classifies_a_removed_directory_as_a_full_sync() {
        let _env = crate::test_env::env_guard();
        // End-to-end with a REAL notify watcher: proves the OS actually delivers
        // either an explicit Folder removal or Windows' ambiguous Any removal,
        // and that the watcher escalates the known directory to a full sync.
        let dir = crate::sync::tests::TestDir::new("watch-real-deleted-dir");
        fs::create_dir_all(dir.path().join("src/feature")).unwrap();
        fs::write(
            dir.path().join("src/feature/mod.ts"),
            "export const x = 1;\n",
        )
        .unwrap();

        let full_calls = Arc::new(AtomicUsize::new(0));
        let full_counter = Arc::clone(&full_calls);
        let full_sync_fn: FullSyncFn = Arc::new(move || {
            full_counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome::default())
        });
        let sync_fn: SyncFn = Arc::new(|_paths| Ok(SyncOutcome::default()));
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                sync_fn: Some(sync_fn),
                full_sync_fn: Some(full_sync_fn),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        std::thread::sleep(Duration::from_millis(150));
        fs::remove_dir_all(dir.path().join("src/feature")).unwrap();

        let mut saw_full = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(50));
            if full_calls.load(AtomicOrdering::SeqCst) > 0 {
                saw_full = true;
                break;
            }
        }
        watcher.stop();
        assert!(
            saw_full,
            "removing a watched directory must escalate to the full sync"
        );
    }

    #[test]
    fn directory_removal_burst_deduplicates_to_one_full_sync() {
        let _env = crate::test_env::env_guard();
        // A `rm -rf` burst surfaces the folder removal AND each child removal.
        // The full-sync escalation must dominate the per-file paths yet still
        // collapse to ONE sync, with every event path retained as a trigger.
        let dir = crate::sync::tests::TestDir::new("watch-deleted-dir-burst");
        let full_calls = Arc::new(AtomicUsize::new(0));
        let full_counter = Arc::clone(&full_calls);
        let full_sync_fn: FullSyncFn = Arc::new(move || {
            full_counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome {
                files_removed: 2,
                ..Default::default()
            })
        });
        let incremental_calls = Arc::new(AtomicUsize::new(0));
        let incremental = Arc::clone(&incremental_calls);
        let sync_fn: SyncFn = Arc::new(move |_paths| {
            incremental.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome::default())
        });
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_secs(30),
                inert_for_tests: true,
                sync_fn: Some(sync_fn),
                full_sync_fn: Some(full_sync_fn),
                on_sync_complete: Some(Arc::new(move |outcome| {
                    outcome_tx.send(outcome).unwrap();
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        watcher.ingest_event_for_tests("src/feature/beta.ts");
        watcher.ingest_removed_dir_for_tests("src/feature");
        watcher.ingest_event_for_tests("src/feature/alpha.ts");
        watcher.ingest_event_for_tests("src/feature/beta.ts");
        watcher.flush_for_tests();
        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("burst sync completion");
        assert!(watcher.pending_files().is_empty());
        watcher.stop();

        assert!(
            outcome_rx.try_recv().is_err(),
            "the burst must collapse to one sync"
        );
        assert_eq!(
            full_calls.load(AtomicOrdering::SeqCst),
            1,
            "the full sync must run exactly once for the burst"
        );
        assert_eq!(
            incremental_calls.load(AtomicOrdering::SeqCst),
            0,
            "a full-sync burst must not also run the incremental sync"
        );
        assert_eq!(
            outcome.trigger_paths,
            vec![
                "src/feature".to_string(),
                "src/feature/alpha.ts".to_string(),
                "src/feature/beta.ts".to_string(),
            ],
            "every event path in the burst stays a sorted, deduped trigger path"
        );
    }

    #[test]
    fn removed_ignored_directory_does_not_schedule_a_full_sync() {
        let _env = crate::test_env::env_guard();
        // The escalation honors the watch policy: an ignored directory removal
        // is still dropped, so no sync is scheduled at all.
        let dir = crate::sync::tests::TestDir::new("watch-deleted-dir-ignored");
        let full_calls = Arc::new(AtomicUsize::new(0));
        let full_counter = Arc::clone(&full_calls);
        let full_sync_fn: FullSyncFn = Arc::new(move || {
            full_counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SyncOutcome::default())
        });
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_secs(30),
                inert_for_tests: true,
                full_sync_fn: Some(full_sync_fn),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        watcher.ingest_removed_dir_for_tests("node_modules/pkg");
        watcher.flush_for_tests();
        watcher.stop();
        assert_eq!(full_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn ignored_directory_event_does_not_schedule_reindex() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-ignore");
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(
            dir.path().join("node_modules/pkg/index.ts"),
            "export const ignored = 1;\n",
        )
        .unwrap();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&outcomes);
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                inert_for_tests: true,
                on_sync_complete: Some(Arc::new(move |outcome| {
                    seen.lock().unwrap().push(outcome);
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        watcher.ingest_event_for_tests("node_modules/pkg/index.ts");
        std::thread::sleep(Duration::from_millis(150));
        watcher.stop();
        assert!(outcomes.lock().unwrap().is_empty());
    }

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn notify_io(errno: i32) -> notify::Error {
        notify::Error::io(io::Error::from_raw_os_error(errno))
    }

    #[test]
    fn classify_maps_errnos_to_handling_classes() {
        assert_eq!(
            classify_watch_error(&io::Error::from_raw_os_error(EMFILE)),
            WatchErrorClass::Degrade
        );
        assert_eq!(
            classify_watch_error(&io::Error::from_raw_os_error(ENFILE)),
            WatchErrorClass::Degrade
        );
        assert_eq!(
            classify_watch_error(&io::Error::from_raw_os_error(ENOSPC)),
            WatchErrorClass::Warn
        );
        assert_eq!(
            classify_watch_error(&io::Error::from_raw_os_error(2)),
            WatchErrorClass::Other
        );
    }

    #[test]
    fn classify_notify_error_maps_kinds() {
        assert_eq!(
            classify_notify_error(&notify_io(EMFILE)),
            WatchErrorClass::Degrade
        );
        assert_eq!(
            classify_notify_error(&notify::Error::new(notify::ErrorKind::MaxFilesWatch)),
            WatchErrorClass::Warn
        );
        assert_eq!(
            classify_notify_error(&notify::Error::new(notify::ErrorKind::WatchNotFound)),
            WatchErrorClass::Other
        );
    }

    #[test]
    fn start_returns_none_when_watch_disabled_by_flag() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-nowatch-flag");
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                no_watch: true,
                inert_for_tests: true,
                ..WatchOptions::default()
            },
        )
        .unwrap();
        assert!(
            watcher.is_none(),
            "no_watch=true must disable the watcher (start returns None)"
        );
    }

    #[test]
    fn emfile_degrades_and_fires_on_degraded_exactly_once() {
        let state = Arc::new(DegradedState::default());
        let degraded_calls = Arc::new(AtomicUsize::new(0));
        let sync_err_calls = Arc::new(AtomicUsize::new(0));
        let dc = Arc::clone(&degraded_calls);
        let sc = Arc::clone(&sync_err_calls);
        let on_degraded: Option<NoticeCallback> = Some(Arc::new(move |_| {
            dc.fetch_add(1, AtomicOrdering::SeqCst);
        }));
        let on_sync_error: Option<NoticeCallback> = Some(Arc::new(move |_| {
            sc.fetch_add(1, AtomicOrdering::SeqCst);
        }));

        for _ in 0..3 {
            let class =
                handle_watch_error(&notify_io(EMFILE), &state, &on_degraded, &on_sync_error);
            assert_eq!(class, WatchErrorClass::Degrade);
        }

        assert!(state.is_degraded());
        assert!(state.reason().is_some());
        assert_eq!(degraded_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sync_err_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn enospc_warns_but_does_not_degrade() {
        let state = Arc::new(DegradedState::default());
        let degraded_calls = Arc::new(AtomicUsize::new(0));
        let sync_err_calls = Arc::new(AtomicUsize::new(0));
        let dc = Arc::clone(&degraded_calls);
        let sc = Arc::clone(&sync_err_calls);
        let on_degraded: Option<NoticeCallback> = Some(Arc::new(move |_| {
            dc.fetch_add(1, AtomicOrdering::SeqCst);
        }));
        let on_sync_error: Option<NoticeCallback> = Some(Arc::new(move |_| {
            sc.fetch_add(1, AtomicOrdering::SeqCst);
        }));

        let class = handle_watch_error(&notify_io(ENOSPC), &state, &on_degraded, &on_sync_error);

        assert_eq!(class, WatchErrorClass::Warn);
        assert!(!state.is_degraded());
        assert_eq!(degraded_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(sync_err_calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn backoff_doubles_and_caps_at_thirty_seconds() {
        let mut backoff = Duration::ZERO;
        let mut last = Duration::ZERO;
        for _ in 0..64 {
            backoff = next_backoff(backoff);
            assert!(backoff <= MAX_BACKOFF, "backoff {backoff:?} exceeded cap");
            assert!(backoff >= last || backoff == MAX_BACKOFF);
            last = backoff;
        }
        assert_eq!(backoff, MAX_BACKOFF);
    }

    #[test]
    fn lock_contention_retries_then_degrades_after_budget() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let sync_fn: SyncFn = Arc::new(move |_paths: Vec<String>| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Err(anyhow::anyhow!("database is locked"))
        });
        let slept = Arc::new(Mutex::new(Vec::<Duration>::new()));
        let recorder = Arc::clone(&slept);
        let outcome = run_sync_with_backoff_inner(
            &sync_fn,
            vec!["src/app.ts".to_string()],
            Duration::from_millis(10),
            move |d| recorder.lock().unwrap().push(d),
        );
        match outcome {
            SyncAttempt::Degraded(reason) => assert!(reason.contains("contention")),
            SyncAttempt::Done(_) => panic!("expected degrade, got Done"),
            SyncAttempt::Error(reason) => panic!("expected degrade, got Error: {reason}"),
        }
        assert!(attempts.load(AtomicOrdering::SeqCst) >= 2);
        assert!(slept.lock().unwrap().iter().all(|d| *d <= MAX_BACKOFF));
    }

    #[test]
    fn non_contention_sync_error_surfaces_without_degrading() {
        let sync_fn: SyncFn =
            Arc::new(|_paths: Vec<String>| Err(anyhow::anyhow!("parse error in file")));
        let outcome = run_sync_with_backoff_inner(
            &sync_fn,
            vec!["src/app.ts".to_string()],
            Duration::from_millis(10),
            |_| {},
        );
        match outcome {
            SyncAttempt::Error(reason) => assert!(reason.contains("parse error")),
            SyncAttempt::Done(_) => panic!("expected non-degrading Error, got Done"),
            SyncAttempt::Degraded(_) => panic!("expected non-degrading Error, got Degraded"),
        }
    }

    #[test]
    fn persistent_errors_disable_auto_sync_after_threshold() {
        // #1127: a repeatable non-contention error retries a bounded number of
        // times, then escalates to a disabled state with an actionable message.
        let mut consecutive = 0u32;
        let outcome = SyncAttempt::Error("sync failed: schema mismatch".to_string());
        for _ in 1..MAX_CONSECUTIVE_SYNC_ERRORS {
            match classify_persistent_failure(&outcome, &mut consecutive) {
                PersistentFailure::Surface => {}
                PersistentFailure::Disable(_) => {
                    panic!("must not disable before {MAX_CONSECUTIVE_SYNC_ERRORS} errors")
                }
                PersistentFailure::Degrade(_) => {
                    panic!("non-contention must not report contention")
                }
            }
        }
        match classify_persistent_failure(&outcome, &mut consecutive) {
            PersistentFailure::Disable(message) => {
                assert!(
                    message.contains("schema mismatch"),
                    "message must name the underlying error: {message}"
                );
                assert!(
                    message.contains("codegraph sync"),
                    "message must point at the manual recovery command: {message}"
                );
            }
            other => panic!("expected Disable at the threshold, got {other:?}"),
        }
    }

    #[test]
    fn a_single_success_resets_the_error_counter() {
        // #1127: a transient hiccup must not accumulate toward disable — one
        // successful sync resets the consecutive-error count.
        let mut consecutive = 0u32;
        let err = SyncAttempt::Error("sync failed: transient".to_string());
        for _ in 0..(MAX_CONSECUTIVE_SYNC_ERRORS - 1) {
            classify_persistent_failure(&err, &mut consecutive);
        }
        assert_eq!(consecutive, MAX_CONSECUTIVE_SYNC_ERRORS - 1);

        let outcome = SyncOutcome::default();
        let done = SyncAttempt::Done(outcome);
        assert!(matches!(
            classify_persistent_failure(&done, &mut consecutive),
            PersistentFailure::Surface
        ));
        assert_eq!(consecutive, 0, "one success must reset the counter");

        // After the reset the very next error is surfaced, NOT disabled.
        assert!(matches!(
            classify_persistent_failure(&err, &mut consecutive),
            PersistentFailure::Surface
        ));
    }

    #[test]
    fn contention_degrade_path_is_unchanged_by_persistent_failure() {
        // #1127 must not touch the lock-contention degrade path: a Degraded
        // outcome passes straight through and never counts as a persistent error.
        let mut consecutive = 7u32;
        let outcome = SyncAttempt::Degraded("sync write-lock contention exceeded".to_string());
        match classify_persistent_failure(&outcome, &mut consecutive) {
            PersistentFailure::Degrade(reason) => assert!(reason.contains("contention")),
            other => panic!("contention must map to Degrade, got {other:?}"),
        }
        assert_eq!(
            consecutive, 7,
            "the contention path must not disturb the error counter"
        );
    }

    #[test]
    fn fresh_watcher_is_not_degraded_and_has_no_reason() {
        let _env = crate::test_env::env_guard();
        // Given: an inert watcher that never hits a backend error.
        let dir = crate::sync::tests::TestDir::new("watch-not-degraded");
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                inert_for_tests: true,
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        // Then: the degraded accessors report a healthy watcher.
        assert!(!watcher.is_degraded());
        assert!(watcher.degraded_reason().is_none());
        watcher.stop();
    }

    #[test]
    fn start_serve_watcher_returns_none_when_watching_is_disabled() {
        let _env = crate::test_env::env_guard();
        // Given: a normal project but the no_watch flag forced on.
        let dir = crate::sync::tests::TestDir::new("watch-serve-disabled");
        // Then: the public wrapper returns Ok(None) (no watcher started).
        let watcher = start_serve_watcher(
            dir.path(),
            WatchOptions {
                no_watch: true,
                inert_for_tests: true,
                ..WatchOptions::default()
            },
        )
        .unwrap();
        assert!(
            watcher.is_none(),
            "start_serve_watcher must not start a watcher when disabled"
        );
    }

    #[test]
    fn start_serve_watcher_starts_an_inert_watcher_for_a_normal_project() {
        let _env = crate::test_env::env_guard();
        // Given: a normal project directory.
        let dir = crate::sync::tests::TestDir::new("watch-serve-start");
        // Then: the public wrapper starts and returns an inert watcher.
        let watcher = start_serve_watcher(
            dir.path(),
            WatchOptions {
                inert_for_tests: true,
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .expect("watcher should start for a normal project");
        assert!(!watcher.is_degraded());
        watcher.stop();
    }

    #[test]
    fn pending_files_snapshot_reflects_ingested_events_before_debounce() {
        let _env = crate::test_env::env_guard();
        // Given: an inert watcher with a long debounce so events stay pending.
        let dir = crate::sync::tests::TestDir::new("watch-pending");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let db = crate::sync::default_db_path(dir.path()).unwrap();
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_secs(30),
                inert_for_tests: true,
                db_path: Some(db),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        // When: two distinct source events are ingested.
        watcher.ingest_event_for_tests("src/alpha.ts");
        watcher.ingest_event_for_tests("src/beta.ts");
        // Then: poll the snapshot until both land (the loop processes async).
        let mut paths = Vec::new();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(25));
            paths = watcher
                .pending_files()
                .into_iter()
                .map(|p| p.path)
                .collect::<Vec<_>>();
            if paths.len() == 2 {
                break;
            }
        }
        assert_eq!(
            paths,
            vec!["src/alpha.ts".to_string(), "src/beta.ts".to_string()]
        );
        for entry in watcher.pending_files() {
            assert!(entry.first_seen_ms <= entry.last_seen_ms);
        }
        watcher.stop();
    }

    /// A watcher whose event loop is INSIDE a sync must not be forced to a join:
    /// `begin_shutdown` returns immediately, `is_finished` truthfully stays false
    /// while the sync runs, and `detach` releases the handle without blocking. This
    /// is what lets the daemon's bounded drain report an INCOMPLETE result instead
    /// of blocking past its budget inside `Drop`. The sync blocks on a barrier, so
    /// "still running" is deterministic rather than timed.
    #[test]
    fn begin_shutdown_and_detach_never_block_on_a_running_sync() {
        let _env = crate::test_env::env_guard();
        let dir = crate::sync::tests::TestDir::new("watch-running-sync");
        let release = Arc::new(std::sync::Barrier::new(2));
        let entered = Arc::new(AtomicBool::new(false));
        let sync_release = Arc::clone(&release);
        let sync_entered = Arc::clone(&entered);
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(1),
                inert_for_tests: true,
                sync_fn: Some(Arc::new(move |_paths| {
                    sync_entered.store(true, AtomicOrdering::SeqCst);
                    sync_release.wait();
                    Ok(SyncOutcome::default())
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        watcher.ingest_event_for_tests("src/app.ts");
        while !entered.load(AtomicOrdering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }

        let started = Instant::now();
        watcher.begin_shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "begin_shutdown must not join a running sync"
        );
        assert!(
            !watcher.is_finished(),
            "a watcher inside a sync must report itself unfinished"
        );

        let started = Instant::now();
        watcher.detach();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "detach must not join a running sync"
        );
        release.wait();
    }

    #[test]
    fn begin_shutdown_is_nonblocking_and_detach_skips_the_join() {
        let _env = crate::test_env::env_guard();
        // Given: an inert watcher whose event loop is idle.
        let dir = crate::sync::tests::TestDir::new("watch-begin-shutdown");
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                inert_for_tests: true,
                sync_fn: Some(Arc::new(|_paths| Ok(SyncOutcome::default()))),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        // When: shutdown begins.
        let cancel = watcher.cancellation();
        watcher.begin_shutdown();

        // Then: cancellation is observable immediately and the call did not join.
        assert!(cancel.is_cancelled());
        watcher.begin_shutdown();
        for _ in 0..200 {
            if watcher.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            watcher.is_finished(),
            "the event loop must exit after begin_shutdown"
        );
        watcher.detach();
    }

    #[test]
    fn event_loop_reports_non_degrading_sync_error_through_callback() {
        let _env = crate::test_env::env_guard();
        // Given: an inert watcher whose injected sync_fn always fails with a
        // non-contention error, plus recorders for both notice callbacks.
        let dir = crate::sync::tests::TestDir::new("watch-loop-error");
        let sync_errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let degraded_calls = Arc::new(AtomicUsize::new(0));
        let se = Arc::clone(&sync_errors);
        let dc = Arc::clone(&degraded_calls);
        let sync_fn: SyncFn =
            Arc::new(|_paths| Err(anyhow::anyhow!("parse error while re-extracting")));
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(40),
                inert_for_tests: true,
                sync_fn: Some(sync_fn),
                on_sync_error: Some(Arc::new(move |msg| se.lock().unwrap().push(msg))),
                on_degraded: Some(Arc::new(move |_| {
                    dc.fetch_add(1, AtomicOrdering::SeqCst);
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        // When: a source event flushes through the debounce into the failing sync.
        watcher.ingest_event_for_tests("src/app.ts");
        let mut saw_error = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(25));
            if !sync_errors.lock().unwrap().is_empty() {
                saw_error = true;
                break;
            }
        }
        watcher.stop();

        // Then: the sync-error callback fired, the watcher did NOT degrade, and
        // the error text is surfaced.
        assert!(
            saw_error,
            "on_sync_error must fire for a non-contention error"
        );
        assert_eq!(degraded_calls.load(AtomicOrdering::SeqCst), 0);
        assert!(
            sync_errors.lock().unwrap()[0].contains("parse error"),
            "the surfaced message must carry the underlying error"
        );
    }

    #[test]
    fn event_loop_disables_auto_sync_after_persistent_errors() {
        let _env = crate::test_env::env_guard();
        // #1127: an injected sync_fn that always fails with the SAME
        // non-contention error must, after MAX_CONSECUTIVE_SYNC_ERRORS flushes,
        // degrade the watcher with an actionable message and stop retrying.
        let dir = crate::sync::tests::TestDir::new("watch-loop-disable");
        let degrade_msgs = Arc::new(Mutex::new(Vec::<String>::new()));
        let dm = Arc::clone(&degrade_msgs);
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let sync_fn: SyncFn = Arc::new(move |_paths| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Err(anyhow::anyhow!("schema version mismatch"))
        });
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(20),
                inert_for_tests: true,
                sync_fn: Some(sync_fn),
                on_degraded: Some(Arc::new(move |msg| dm.lock().unwrap().push(msg))),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        for i in 0..MAX_CONSECUTIVE_SYNC_ERRORS {
            watcher.ingest_event_for_tests(format!("src/app{i}.ts"));
            std::thread::sleep(Duration::from_millis(60));
        }
        let mut degraded = false;
        for _ in 0..40 {
            if watcher.is_degraded() {
                degraded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        watcher.stop();

        assert!(degraded, "watcher must degrade after persistent failures");
        let msgs = degrade_msgs.lock().unwrap();
        assert_eq!(msgs.len(), 1, "on_degraded must fire exactly once");
        assert!(
            msgs[0].contains("schema version mismatch") && msgs[0].contains("codegraph sync"),
            "disable message must name the error and point at the fix: {}",
            msgs[0]
        );
    }

    #[test]
    fn maybe_deleted_source_tracks_builtin_language_table() {
        // A deleted file is "source" iff its extension maps to a builtin
        // language in `builtin_language_for_ext` (the single source of truth).
        // GDScript (`gd`) regression: the prior hardcoded SOURCE_EXTENSIONS list
        // omitted it, so a deleted `.gd` file was wrongly skipped on cleanup.
        assert!(
            maybe_deleted_source("foo.gd"),
            "gd is a builtin source language"
        );
        assert!(
            maybe_deleted_source("foo.ts"),
            "ts is a builtin source language"
        );
        assert!(
            !maybe_deleted_source("foo.unknownxyz"),
            "unknown extension is not a source language"
        );
        assert!(
            !maybe_deleted_source("README.md"),
            "md is not a builtin source language"
        );
    }

    #[test]
    fn collect_watch_dirs_prunes_ignored_subtrees() {
        let dir = crate::sync::tests::TestDir::new("watch-collect-dirs");
        let root = dir.path();
        // A realistic tree: source dirs to keep, and one TOP-LEVEL dir per ignore
        // family the WatchPolicy recognizes (node_modules / .venv / __pycache__ /
        // .git / .codegraph), each with a descendant that must also be excluded.
        // The policy's ignore rules anchor at the relative-path root, so a
        // top-level ignored dir prunes its whole subtree — that is exactly the
        // existing semantics we must honor (not extend) at watch-registration time.
        for keep in ["src", "src/inner", "lib"] {
            fs::create_dir_all(root.join(keep)).unwrap();
        }
        for ignored in [
            "node_modules/pkg",
            ".venv/lib/site-packages",
            "__pycache__/sub",
            ".git/objects",
            ".codegraph/cache",
        ] {
            fs::create_dir_all(root.join(ignored)).unwrap();
        }

        let policy = WatchPolicy::new(root);
        let dirs = collect_watch_dirs(root, &policy);

        let rels = |dirs: &[PathBuf]| -> Vec<String> {
            dirs.iter()
                .map(|d| {
                    d.strip_prefix(root)
                        .map(normalize_relative_for_test)
                        .unwrap_or_default()
                })
                .collect()
        };
        let got = rels(&dirs);

        // Root and every real source dir are watched.
        assert!(dirs.iter().any(|d| d == root), "root must be watched");
        for keep in ["src", "src/inner", "lib"] {
            assert!(got.contains(&keep.to_string()), "missing source dir {keep}");
        }
        // No ignored dir OR descendant survives — the walk never descended.
        for bad in [
            "node_modules",
            "node_modules/pkg",
            ".venv",
            ".venv/lib",
            ".venv/lib/site-packages",
            "__pycache__",
            "__pycache__/sub",
            ".git",
            ".git/objects",
            ".codegraph",
            ".codegraph/cache",
        ] {
            assert!(
                !got.contains(&bad.to_string()),
                "ignored subtree {bad} must be pruned, got {got:?}"
            );
        }
        // Deterministic: output is sorted.
        let mut sorted = dirs.clone();
        sorted.sort();
        assert_eq!(dirs, sorted, "collect_watch_dirs must be sorted");
    }

    #[test]
    fn native_recursive_backend_registers_only_the_root() {
        // Given: a backend whose OS watcher covers descendants natively.
        let dir = crate::sync::tests::TestDir::new("watch-native-targets");
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        let policy = WatchPolicy::new(dir.path());

        // When: the initial watch targets are calculated.
        let targets = initial_watch_targets(WatchBackend::NativeRecursive, dir.path(), &policy);

        // Then: exactly one recursive root registration is selected.
        assert_eq!(
            targets,
            vec![(dir.path().to_path_buf(), RecursiveMode::Recursive)]
        );
    }

    #[test]
    fn per_dir_backend_registers_the_pruned_non_recursive_set() {
        // Given: retained source directories and an ignored subtree.
        let dir = crate::sync::tests::TestDir::new("watch-per-dir-targets");
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        let policy = WatchPolicy::new(dir.path());

        // When: the initial watch targets are calculated.
        let targets = initial_watch_targets(WatchBackend::PerDirNonRecursive, dir.path(), &policy);

        // Then: every retained directory is non-recursive and ignored dirs are absent.
        assert_eq!(
            targets,
            vec![
                (dir.path().to_path_buf(), RecursiveMode::NonRecursive),
                (dir.path().join("src"), RecursiveMode::NonRecursive),
                (dir.path().join("src/nested"), RecursiveMode::NonRecursive),
            ]
        );
    }

    #[test]
    fn native_recursive_registration_does_not_top_up_new_directories() {
        // Given: a new directory under a native-recursive watch and an injected
        // registration callback that records every attempted `.watch()` call.
        let dir = crate::sync::tests::TestDir::new("watch-native-new-dir");
        let new_dir = dir.path().join("feature");
        fs::create_dir_all(new_dir.join("nested")).unwrap();
        let policy = WatchPolicy::new(dir.path());
        let mut registrations = Vec::new();

        // When: the create-event top-up path runs for the new directory.
        register_new_dirs_with(
            WatchBackend::NativeRecursive,
            &policy,
            &new_dir,
            |path, mode| registrations.push((path.to_path_buf(), mode)),
        );

        // Then: the native root watch grows by zero registrations.
        assert!(registrations.is_empty());
    }

    fn normalize_relative_for_test(relative: &Path) -> String {
        relative.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn collect_watch_dirs_prunes_nested_ignored_subtrees() {
        let dir = crate::sync::tests::TestDir::new("watch-collect-nested");
        let root = dir.path();
        // A project nested 3+ levels under root, with a `.pnpm`-style nested
        // node_modules tree. The whole nested ignore subtree (and its
        // descendants) must be pruned while real source dirs are kept.
        for keep in [
            "workspace/team/app/src",
            "workspace/team/app/src/components",
            "workspace/team/lib",
        ] {
            fs::create_dir_all(root.join(keep)).unwrap();
        }
        for ignored in [
            "workspace/team/app/node_modules/pkg",
            "workspace/team/app/node_modules/.pnpm/vue-demi@1/node_modules/vue-demi/bin",
            "workspace/team/app/src/.venv/lib",
        ] {
            fs::create_dir_all(root.join(ignored)).unwrap();
        }

        let policy = WatchPolicy::new(root);
        let dirs = collect_watch_dirs(root, &policy);
        let got: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.strip_prefix(root).ok().map(normalize_relative_for_test))
            .collect();

        for keep in [
            "workspace/team/app/src",
            "workspace/team/app/src/components",
            "workspace/team/lib",
        ] {
            assert!(got.contains(&keep.to_string()), "missing source dir {keep}");
        }
        for bad in [
            "workspace/team/app/node_modules",
            "workspace/team/app/node_modules/pkg",
            "workspace/team/app/node_modules/.pnpm",
            "workspace/team/app/src/.venv",
            "workspace/team/app/src/.venv/lib",
        ] {
            assert!(
                !got.iter()
                    .any(|p| p == bad || p.starts_with(&format!("{bad}/"))),
                "nested ignored subtree {bad} must be pruned, got {got:?}"
            );
        }
    }

    #[test]
    fn collect_watch_dirs_honors_gitignore_pruning() {
        // `.generated/` is deliberately absent from the default ignores. This
        // proves the watch set is pruned by the SAME merged policy (not a second
        // hardcoded list), so project-specific ignores keep us out of generated
        // trees too.
        let dir = crate::sync::tests::TestDir::new("watch-collect-gitignore");
        let root = dir.path();
        fs::write(root.join(".gitignore"), ".generated/\n").unwrap();
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join(".generated/cache")).unwrap();

        let policy = WatchPolicy::new(root);
        let dirs = collect_watch_dirs(root, &policy);
        let got: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.strip_prefix(root).ok().map(normalize_relative_for_test))
            .collect();

        assert!(got.contains(&"scenes".to_string()), "source dir kept");
        assert!(
            !got.iter().any(|p| p.starts_with(".generated")),
            ".generated subtree must be pruned via .gitignore, got {got:?}"
        );
    }

    #[test]
    fn editing_a_real_source_file_still_triggers_sync() {
        let _env = crate::test_env::env_guard();
        // End-to-end with a REAL notify watcher (inert_for_tests = false), proving
        // the per-dir NonRecursive registration still delivers source edits to the
        // sync pipeline. `sync_fn` is injected so no real index is needed.
        let dir = crate::sync::tests::TestDir::new("watch-real-sync");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function one() { return 1; }\n",
        )
        .unwrap();

        let synced = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let recorder = Arc::clone(&synced);
        let sync_fn: SyncFn = Arc::new(move |paths| {
            recorder.lock().unwrap().push(paths.clone());
            Ok(SyncOutcome {
                files_checked: paths.len(),
                files_reindexed: paths.len(),
                ..Default::default()
            })
        });

        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                sync_fn: Some(sync_fn),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        // Give the OS watch registration a moment, then edit the source file.
        std::thread::sleep(Duration::from_millis(150));
        fs::write(
            dir.path().join("src/app.ts"),
            "export function two() { return 2; }\n",
        )
        .unwrap();

        // Poll for the sync to land (debounce + OS delivery latency).
        let mut seen = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(50));
            if synced
                .lock()
                .unwrap()
                .iter()
                .any(|paths| paths.iter().any(|p| p == "src/app.ts"))
            {
                seen = true;
                break;
            }
        }
        watcher.stop();
        assert!(
            seen,
            "editing src/app.ts should trigger a sync of that path"
        );
    }

    #[test]
    fn newly_created_source_dir_is_watched_after_start() {
        let _env = crate::test_env::env_guard();
        // A directory created AFTER start must be picked up (Linux NonRecursive:
        // the event loop registers a watch on the create event) so edits inside
        // it sync without a server restart.
        let dir = crate::sync::tests::TestDir::new("watch-new-dir");
        fs::create_dir_all(dir.path().join("src")).unwrap();

        let synced = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let recorder = Arc::clone(&synced);
        let sync_fn: SyncFn = Arc::new(move |paths| {
            recorder.lock().unwrap().push(paths.clone());
            Ok(SyncOutcome {
                files_checked: paths.len(),
                files_reindexed: paths.len(),
                ..Default::default()
            })
        });

        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                sync_fn: Some(sync_fn),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        std::thread::sleep(Duration::from_millis(150));
        // Create a brand-new dir, let the create event register its watch, then
        // edit a file inside it.
        fs::create_dir_all(dir.path().join("feature")).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        fs::write(dir.path().join("feature/mod.ts"), "export const x = 1;\n").unwrap();

        let mut seen = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(50));
            if synced
                .lock()
                .unwrap()
                .iter()
                .any(|paths| paths.iter().any(|p| p == "feature/mod.ts"))
            {
                seen = true;
                break;
            }
        }
        watcher.stop();
        assert!(
            seen,
            "editing a file in a dir created after start should trigger a sync"
        );
    }
}
