use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::policy::{WatchPolicy, watch_disabled_reason};
use crate::sync::{SyncOutcome, default_db_path, sync_changed_paths};

type SyncCallback = Arc<dyn Fn(SyncOutcome) + Send + Sync>;
type SyncFn = Arc<dyn Fn(Vec<String>) -> Result<SyncOutcome> + Send + Sync>;
type NoticeCallback = Arc<dyn Fn(String) + Send + Sync>;

/// The OS watcher, shared between [`ProjectWatcher`] (which owns its lifetime)
/// and the event-loop thread (which registers a watch when a brand-new
/// non-ignored directory appears). Because Linux watches are NON-recursive
/// per-dir (see [`collect_watch_dirs`]), a freshly-created directory would
/// otherwise hold no watch until a server restart; the loop adds one on its
/// create event.
type SharedWatcher = Arc<Mutex<Option<RecommendedWatcher>>>;

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

/// Register a NonRecursive watch for a newly-created directory `new_dir` and all
/// of its non-ignored descendants (a `mkdir -p a/b/c` surfaces one create event,
/// so the subtree must be re-walked). Reuses `collect_watch_dirs`' pruning rules
/// via the SAME policy, and silently tolerates a poisoned lock, a stopped watcher
/// (`None`), or a per-dir watch error — a best-effort top-up that must never panic
/// the event loop.
fn register_new_dirs(watcher: &SharedWatcher, policy: &WatchPolicy, new_dir: &Path) {
    let Ok(mut guard) = watcher.lock() else {
        return;
    };
    let Some(watcher) = guard.as_mut() else {
        return;
    };
    for dir in collect_watch_dirs(new_dir, policy) {
        let _ = watcher.watch(&dir, RecursiveMode::NonRecursive);
    }
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
    sync_fn: Option<SyncFn>,
}

impl Default for WatchOptions {
    fn default() -> Self {
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
            sync_fn: None,
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
}

pub fn start_serve_watcher(
    project_root: impl AsRef<Path>,
    options: WatchOptions,
) -> Result<Option<ProjectWatcher>> {
    ProjectWatcher::start(project_root, options)
}

impl ProjectWatcher {
    pub fn start(project_root: impl AsRef<Path>, options: WatchOptions) -> Result<Option<Self>> {
        let project_root = project_root.as_ref().to_path_buf();
        if watch_disabled_reason(&project_root, options.no_watch).is_some() {
            return Ok(None);
        }
        let policy = WatchPolicy::new(&project_root);
        let db_path = options
            .db_path
            .clone()
            .unwrap_or_else(|| default_db_path(&project_root));
        let sync_fn = options.sync_fn.clone().unwrap_or_else(|| {
            let project_root = project_root.clone();
            Arc::new(move |paths| sync_changed_paths(&project_root, &db_path, paths))
        });
        let (tx, rx) = mpsc::channel();
        let degraded = Arc::new(DegradedState::default());

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
                        let _ = callback_tx.send(LoopMessage::Event(event.paths));
                    }
                    Err(err) => {
                        let _ = callback_tx.send(LoopMessage::WatchError(err));
                    }
                })?;
            // Register a NON-recursive watch per surviving directory instead of one
            // blanket `RecursiveMode::Recursive` on the root. On Linux that recursive
            // mode makes notify register an inotify watch for EVERY subdirectory —
            // including node_modules/.venv/__pycache__/site-packages/.godot — which
            // exhausts the kernel `max_user_watches` limit and stalls startup on a
            // large root. `collect_watch_dirs` prunes ignored subtrees up front, so
            // we only watch source dirs.
            //
            // Platform note: notify v6 honors `NonRecursive` on Linux (inotify, one
            // watch per dir, exactly what we want). On macOS (FSEvents) and Windows
            // (ReadDirectoryChangesW) the backend is natively recursive; using
            // `NonRecursive` there yields per-dir watches that still cover the pruned
            // set, and any event from an ignored subtree (had we over-watched) is
            // dropped by the `WatchPolicy` filter in the event loop. So this single
            // strategy is correct on Linux (the platform with the bug) and safe
            // elsewhere, with no `#[cfg]` split needed.
            let dirs = collect_watch_dirs(&project_root, &policy);
            let mut watch_err: Option<notify::Error> = None;
            for dir in &dirs {
                if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
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

        let loop_policy = policy.clone();
        let on_sync_complete = options.on_sync_complete.clone();
        let on_degraded = options.on_degraded.clone();
        let on_sync_error = options.on_sync_error.clone();
        let debounce = options.debounce;
        let loop_degraded = Arc::clone(&degraded);
        let loop_watcher = Arc::clone(&watcher);
        let thread = thread::spawn(move || {
            event_loop(EventLoopCtx {
                rx,
                policy: loop_policy,
                debounce,
                sync_fn,
                on_sync_complete,
                on_degraded,
                on_sync_error,
                degraded: loop_degraded,
                watcher: loop_watcher,
            });
        });

        Ok(Some(Self {
            tx,
            thread: Some(thread),
            watcher,
            degraded,
        }))
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded.is_degraded()
    }

    pub fn degraded_reason(&self) -> Option<String> {
        self.degraded.reason()
    }

    pub fn ingest_event_for_tests(&self, relative: impl Into<PathBuf>) {
        let _ = self.tx.send(LoopMessage::Event(vec![relative.into()]));
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
        if let Ok(mut guard) = self.watcher.lock() {
            let _ = guard.take();
        }
        let _ = self.tx.send(LoopMessage::Stop);
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
    Event(Vec<PathBuf>),
    WatchError(notify::Error),
    Snapshot(Sender<Vec<PendingFile>>),
    Stop,
}

#[derive(Debug, Clone)]
struct PendingInfo {
    first_seen_ms: u128,
    last_seen_ms: u128,
}

struct EventLoopCtx {
    rx: mpsc::Receiver<LoopMessage>,
    policy: WatchPolicy,
    debounce: Duration,
    sync_fn: SyncFn,
    on_sync_complete: Option<SyncCallback>,
    on_degraded: Option<NoticeCallback>,
    on_sync_error: Option<NoticeCallback>,
    degraded: Arc<DegradedState>,
    watcher: SharedWatcher,
}

fn event_loop(ctx: EventLoopCtx) {
    let EventLoopCtx {
        rx,
        policy,
        debounce,
        sync_fn,
        on_sync_complete,
        on_degraded,
        on_sync_error,
        degraded,
        watcher,
    } = ctx;
    let mut pending = BTreeMap::<String, PendingInfo>::new();
    let mut deadline = None::<Instant>;
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
            Some(LoopMessage::Event(paths)) => {
                for path in paths {
                    if let Some(relative) = policy.normalize_relative(&path) {
                        // A brand-new non-ignored directory holds no inotify watch
                        // yet (Linux watches are per-dir NonRecursive — see
                        // `collect_watch_dirs`). Register it (and any non-ignored
                        // descendants created in the same burst, e.g. `mkdir -p`) so
                        // edits inside it are seen without a server restart.
                        if path.is_dir() && policy.should_watch_dir(&relative) {
                            register_new_dirs(&watcher, &policy, &path);
                        }
                        if policy.should_handle_file(&relative)
                            || (policy.allows_file_path(&relative)
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
                }
                if !pending.is_empty() {
                    // Resetting the timer on every event ports the upstream exactly-once
                    // burst semantics (`upstream sync/watcher.ts:529-540`).
                    deadline = Some(Instant::now() + debounce);
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
            Some(LoopMessage::Stop) => break,
            None => {
                let paths = pending.keys().cloned().collect::<Vec<_>>();
                pending.clear();
                deadline = None;
                match run_sync_with_backoff(&sync_fn, paths) {
                    SyncAttempt::Done(outcome) => {
                        if let Some(callback) = &on_sync_complete {
                            callback(outcome);
                        }
                    }
                    SyncAttempt::Degraded(reason) => {
                        if !degraded.is_degraded() {
                            degraded.mark(reason.clone());
                            if let Some(cb) = &on_degraded {
                                cb(reason);
                            }
                        }
                        break;
                    }
                    SyncAttempt::Error(reason) => {
                        if let Some(cb) = &on_sync_error {
                            cb(reason);
                        }
                    }
                }
            }
        }
    }
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

/// Run `sync_fn`, retrying on write-lock contention with bounded exponential
/// backoff capped at [`MAX_BACKOFF`]. Once the cumulative sleep budget is spent
/// the watcher degrades; any non-contention error is surfaced as a sync error.
fn run_sync_with_backoff(sync_fn: &SyncFn, paths: Vec<String>) -> SyncAttempt {
    run_sync_with_backoff_inner(sync_fn, paths, MAX_BACKOFF, thread::sleep)
}

/// Inner retry loop with an injectable budget and sleeper so the cap can be
/// unit-tested without sleeping a real 30 seconds.
fn run_sync_with_backoff_inner(
    sync_fn: &SyncFn,
    paths: Vec<String>,
    budget: Duration,
    mut sleeper: impl FnMut(Duration),
) -> SyncAttempt {
    let mut backoff = Duration::ZERO;
    let mut slept = Duration::ZERO;
    loop {
        match sync_fn(paths.clone()) {
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
    let millis = std::env::var("CODEGRAPH_WATCH_DEBOUNCE_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(2_000)
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

    #[test]
    fn rapid_save_burst_triggers_exactly_one_reindex() {
        let dir = crate::sync::tests::TestDir::new("watch-debounce");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let db = crate::sync::default_db_path(dir.path());
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
    fn ignored_directory_event_does_not_schedule_reindex() {
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
    fn fresh_watcher_is_not_degraded_and_has_no_reason() {
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
        // Given: an inert watcher with a long debounce so events stay pending.
        let dir = crate::sync::tests::TestDir::new("watch-pending");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let db = crate::sync::default_db_path(dir.path());
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

    #[test]
    fn event_loop_reports_non_degrading_sync_error_through_callback() {
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
        // `.godot/` is not in DEFAULT_IGNORE_DIRS, but a real Godot project lists
        // it in .gitignore, which WatchPolicy::new merges. This proves the watch
        // set is pruned by the SAME merged policy (not a second hardcoded list),
        // so project-specific ignores keep us out of generated trees too.
        let dir = crate::sync::tests::TestDir::new("watch-collect-gitignore");
        let root = dir.path();
        fs::write(root.join(".gitignore"), ".godot/\n").unwrap();
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join(".godot/imported")).unwrap();

        let policy = WatchPolicy::new(root);
        let dirs = collect_watch_dirs(root, &policy);
        let got: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.strip_prefix(root).ok().map(normalize_relative_for_test))
            .collect();

        assert!(got.contains(&"scenes".to_string()), "source dir kept");
        assert!(
            !got.iter().any(|p| p.starts_with(".godot")),
            ".godot subtree must be pruned via .gitignore, got {got:?}"
        );
    }

    #[test]
    fn editing_a_real_source_file_still_triggers_sync() {
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
