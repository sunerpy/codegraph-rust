//! Per-project daemon lifecycle for CodeGraph.
//!
//! This crate owns the task-24 daemon mechanics: project-scoped rendezvous paths,
//! atomic pid locks, cross-platform local-socket session handling, parent/host
//! watchdogs, graceful shutdown, and stale-lock recovery. It deliberately does
//! not implement task-25 file watching.

pub mod http_registry;
mod lock;
mod paths;
mod process;
pub mod proxy;
mod session;
pub mod spawn;
mod transport;

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use interprocess::local_socket::traits::Listener as _;
pub use lock::{
    AcquireResult, DaemonLockInfo, clear_stale_daemon_lock, clear_stale_daemon_socket,
    decode_lock_info, encode_lock_info, recorded_socket_path, try_acquire_daemon_lock,
    unlock_project,
};
pub use paths::{daemon_log_path, daemon_pid_path, daemon_socket_path};
pub use process::{
    SupervisionState, current_ppid, is_process_alive, is_session_leader, supervision_lost_reason,
    terminate_pid,
};
pub use proxy::{ProxyOutcome, run_proxy, verify_daemon_hello};
pub use session::{SessionRegistry, read_daemon_hello, run_session_recv};
pub use spawn::{CODEGRAPH_HTTP_DETACH_INTERNAL, spawn_detached_daemon, spawn_detached_http};
use tracing::{debug, info, warn};

use crate::lock::{cleanup_owned_lock, rewrite_lock_socket_path};
use crate::paths::codegraph_dir;
use crate::session::serve_session;
use crate::transport::{Listener, Rendezvous, bind, connect};

const DEFAULT_WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Env var name: when set to `"1"`, the process re-invoked by the launcher IS
/// the detached daemon and must listen+serve, never re-spawn.
pub const CODEGRAPH_DAEMON_INTERNAL: &str = "CODEGRAPH_DAEMON_INTERNAL";

/// Env var name: when set to `"1"`, the daemon is opted out and `serve --mcp`
/// runs in direct (single-process) mode.
pub const CODEGRAPH_NO_DAEMON: &str = "CODEGRAPH_NO_DAEMON";

/// Env var name: milliseconds the daemon lingers after the LAST client
/// disconnects before exiting (default 300000). Mirrors colby.
pub const CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS: &str = "CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS";

/// Env var name: hard backstop in milliseconds — exit once idle this long
/// regardless of client count (default 1800000; #692 phantom-client guard).
pub const CODEGRAPH_DAEMON_MAX_IDLE_MS: &str = "CODEGRAPH_DAEMON_MAX_IDLE_MS";

/// Env var name: how often (ms) the daemon sweeps connected sessions whose
/// announced host pid is dead and force-closes them (default 30000). Mirrors
/// colby `DEFAULT_CLIENT_SWEEP_MS`.
pub const CODEGRAPH_DAEMON_CLIENT_SWEEP_MS: &str = "CODEGRAPH_DAEMON_CLIENT_SWEEP_MS";

const DEFAULT_IDLE_TIMEOUT_MS: u128 = 300_000;
const DEFAULT_MAX_IDLE_MS: u128 = 1_800_000;
const MIN_IDLE_TIMEOUT_MS: u128 = 1_000;
const MAX_IDLE_TIMEOUT_MS: u128 = 3_600_000;

const DEFAULT_CLIENT_SWEEP_MS: u128 = 30_000;
const MIN_CLIENT_SWEEP_MS: u128 = 50;
const MAX_CLIENT_SWEEP_MS: u128 = 600_000;

/// Parse + clamp the idle-timeout env var (mirror colby `resolveIdleTimeoutMs`):
/// unset/empty/invalid -> default; otherwise clamp to [MIN, MAX].
fn resolve_idle_timeout_ms() -> u128 {
    resolve_ms_env(
        CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS,
        DEFAULT_IDLE_TIMEOUT_MS,
        MIN_IDLE_TIMEOUT_MS,
        MAX_IDLE_TIMEOUT_MS,
    )
}

/// Parse + clamp the max-idle backstop env var (mirror colby `resolveMaxIdleMs`).
fn resolve_max_idle_ms() -> u128 {
    resolve_ms_env(
        CODEGRAPH_DAEMON_MAX_IDLE_MS,
        DEFAULT_MAX_IDLE_MS,
        MIN_IDLE_TIMEOUT_MS,
        MAX_IDLE_TIMEOUT_MS,
    )
}

fn resolve_ms_env(name: &str, default: u128, min: u128, max: u128) -> u128 {
    match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => raw
            .parse::<u128>()
            .map_or(default, |parsed| parsed.clamp(min, max)),
        _ => default,
    }
}

fn resolve_client_sweep_ms() -> u128 {
    resolve_ms_env(
        CODEGRAPH_DAEMON_CLIENT_SWEEP_MS,
        DEFAULT_CLIENT_SWEEP_MS,
        MIN_CLIENT_SWEEP_MS,
        MAX_CLIENT_SWEEP_MS,
    )
}

#[derive(Clone, Debug)]
pub struct DaemonOptions {
    pub parent_pid: Option<u32>,
    pub host_pid: Option<u32>,
    pub watchdog_interval: Duration,
    pub run_mcp: bool,
    /// When true (default), the daemon owns ONE shared `ProjectWatcher` for the
    /// project (issue-#411: N client inotify sets collapse to 1). Honors
    /// `watch_disabled_reason` (e.g. `CODEGRAPH_NO_WATCH=1`).
    pub watch: bool,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            parent_pid: None,
            host_pid: None,
            watchdog_interval: DEFAULT_WATCHDOG_INTERVAL,
            run_mcp: true,
            watch: true,
        }
    }
}

#[derive(Debug)]
pub enum StartOrAttach {
    Started(DaemonHandle),
    Attached(DaemonClient),
}

#[derive(Debug)]
pub struct DaemonClient {
    pub socket_path: PathBuf,
    pub hello: serde_json::Value,
}

#[derive(Debug)]
pub struct DaemonHandle {
    socket_path: PathBuf,
    registry: SessionRegistry,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl DaemonHandle {
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn active_sessions(&self) -> usize {
        self.registry.active_count()
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub fn stop(mut self) -> Result<()> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .unwrap_or_else(|_| bail!("daemon thread panicked"))?;
        }
        Ok(())
    }

    pub fn wait(mut self) -> Result<()> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .unwrap_or_else(|_| bail!("daemon thread panicked"))?;
        }
        Ok(())
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn start_or_attach(
    project_root: impl AsRef<Path>,
    options: DaemonOptions,
) -> Result<StartOrAttach> {
    let project_root = project_root.as_ref().to_path_buf();
    match try_acquire_daemon_lock(&project_root)? {
        AcquireResult::Acquired { pid_path, info } => {
            let handle = start_with_lock(project_root, pid_path, info.socket_path, options)?;
            Ok(StartOrAttach::Started(handle))
        }
        AcquireResult::Taken { existing, pid_path } => {
            if let Some(info) = existing {
                if let Ok(client) = attach_to_daemon(&info.socket_path) {
                    return Ok(StartOrAttach::Attached(client));
                }
                if !clear_stale_daemon_lock(&pid_path, Some(info.pid)) {
                    bail!("daemon already running for this project (pid {})", info.pid);
                }
                return start_or_attach(project_root, options);
            }
            if !clear_stale_daemon_lock(&pid_path, None) {
                bail!(
                    "daemon lock exists but could not be cleared: {}",
                    pid_path.display()
                );
            }
            start_or_attach(project_root, options)
        }
    }
}

pub fn run_foreground(project_root: impl AsRef<Path>, options: DaemonOptions) -> Result<()> {
    match start_or_attach(project_root, options)? {
        StartOrAttach::Started(handle) => handle.wait(),
        StartOrAttach::Attached(client) => {
            bail!("daemon already running at {}", client.socket_path.display())
        }
    }
}

pub fn attach_to_daemon(socket_path: &Path) -> Result<DaemonClient> {
    let rendezvous = Rendezvous::from_socket_path(socket_path);
    let mut stream = connect(&rendezvous)
        .with_context(|| format!("connecting to daemon socket {}", socket_path.display()))?;
    let hello = read_daemon_hello(&mut stream)?;
    Ok(DaemonClient {
        socket_path: socket_path.to_path_buf(),
        hello,
    })
}

fn start_with_lock(
    project_root: PathBuf,
    pid_path: PathBuf,
    socket_path: PathBuf,
    options: DaemonOptions,
) -> Result<DaemonHandle> {
    fs::create_dir_all(codegraph_dir(&project_root))
        .with_context(|| format!("creating {}", codegraph_dir(&project_root).display()))?;
    let (listener, socket_path) = bind_with_fallback(&project_root, socket_path)?;
    // The bind-fallback may have selected a candidate other than the one
    // recorded at acquire time; persist the CHOSEN socket so a client reading
    // the lock attaches to the socket actually bound (f83a1ec / D-Daemon).
    if let Err(err) = rewrite_lock_socket_path(&pid_path, &socket_path) {
        debug!(error = %err, "could not persist chosen daemon socket into lock");
    }
    let registry = SessionRegistry::default();
    let shutdown = Arc::new(AtomicBool::new(false));
    let thread_registry = registry.clone();
    let thread_shutdown = Arc::clone(&shutdown);
    let thread_project = project_root.clone();
    let thread_socket = socket_path.clone();
    let thread_pid_path = pid_path.clone();

    let thread = thread::spawn(move || {
        run_accept_loop(
            listener,
            thread_project,
            thread_socket,
            thread_pid_path,
            thread_registry,
            thread_shutdown,
            options,
        )
    });

    Ok(DaemonHandle {
        socket_path,
        registry,
        shutdown,
        thread: Some(thread),
    })
}

/// Ordered bind candidates: the lock-recorded `preferred` socket first, then the
/// remaining unix fallback candidates (`f83a1ec`). On non-unix there is a single
/// namespaced pipe name, so the chain is just `[preferred]`.
#[cfg(unix)]
fn socket_candidate_chain(project_root: &Path, preferred: PathBuf) -> Vec<PathBuf> {
    let mut candidates = vec![preferred];
    for candidate in paths::daemon_socket_candidates(project_root) {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

#[cfg(not(unix))]
fn socket_candidate_chain(_project_root: &Path, preferred: PathBuf) -> Vec<PathBuf> {
    vec![preferred]
}

/// Bind the daemon listener, falling through the deterministic socket-candidate
/// chain on `bind()` failure (`f83a1ec`). `preferred` is the socket the lock
/// recorded; it is tried first, then any remaining
/// [`daemon_socket_candidates`] in order. Returns the listener plus the socket
/// that actually bound. Errors only when EVERY candidate fails.
fn bind_with_fallback(project_root: &Path, preferred: PathBuf) -> Result<(Listener, PathBuf)> {
    let candidates = socket_candidate_chain(project_root, preferred);

    let mut last_err = None;
    for socket_path in candidates {
        let rendezvous = Rendezvous::from_socket_path(&socket_path);
        #[cfg(unix)]
        if let Some(stale) = rendezvous.cleanup_path()
            && stale.exists()
        {
            let _ = fs::remove_file(stale);
        }
        match bind(&rendezvous) {
            Ok(listener) => return Ok((listener, socket_path)),
            Err(err) => {
                debug!(socket = %socket_path.display(), error = %err, "daemon socket bind failed; trying next candidate");
                last_err = Some((socket_path, err));
            }
        }
    }
    let (socket_path, err) = last_err.expect("candidate list is never empty");
    Err(err).with_context(|| format!("binding daemon socket {}", socket_path.display()))
}

#[allow(clippy::too_many_arguments)]
fn run_accept_loop(
    listener: Listener,
    project_root: PathBuf,
    socket_path: PathBuf,
    pid_path: PathBuf,
    registry: SessionRegistry,
    shutdown: Arc<AtomicBool>,
    options: DaemonOptions,
) -> Result<()> {
    let original_ppid = options.parent_pid.unwrap_or_else(current_ppid);
    let socket_display = socket_path.to_string_lossy().to_string();
    let idle_timeout_ms = resolve_idle_timeout_ms();
    let max_idle_ms = resolve_max_idle_ms();
    let client_sweep_ms = resolve_client_sweep_ms();
    let mut last_sweep = std::time::Instant::now();
    info!(project = %project_root.display(), socket = %socket_path.display(), "daemon started");

    // ONE shared watcher per daemon process (issue-#411). Bound to a local so
    // its `Drop` stops the watch thread on shutdown. NEVER move this into
    // `serve_session`: per-connection would spawn N watchers.
    let _watcher = start_project_watcher(&project_root, &options);

    let _catch_up_done = spawn_catch_up(&project_root);

    while !shutdown.load(Ordering::SeqCst) {
        let state = SupervisionState {
            original_ppid,
            current_ppid: current_ppid(),
            host_pid: options.host_pid,
            session_leader: is_session_leader(),
        };
        if let Some(reason) = supervision_lost_reason(&state, is_process_alive) {
            warn!(reason, "daemon watchdog stopping after supervisor loss");
            break;
        }

        match listener.accept() {
            Ok(stream) => {
                let session_project = project_root.clone();
                let session_socket = socket_display.clone();
                let session_registry = registry.clone();
                let run_mcp = options.run_mcp;
                thread::spawn(move || {
                    if let Err(err) = serve_session(
                        stream,
                        session_project,
                        session_socket,
                        session_registry,
                        run_mcp,
                    ) {
                        debug!(error = %err, "daemon session ended with error");
                    }
                });
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if last_sweep.elapsed().as_millis() >= client_sweep_ms {
                    sweep_dead_clients(&registry);
                    last_sweep = std::time::Instant::now();
                }
                let idle_ms = registry.millis_since_active();
                if idle_ms > max_idle_ms {
                    info!(idle_ms, max_idle_ms, "daemon exiting on max-idle backstop");
                    break;
                }
                if registry.active_count() == 0 && idle_ms > idle_timeout_ms {
                    info!(idle_ms, idle_timeout_ms, "daemon idle-exit: no clients");
                    break;
                }
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(err) => return Err(err).context("accepting daemon connection"),
        }
    }

    cleanup_owned_lock(&pid_path, std::process::id());
    #[cfg(unix)]
    if let Some(stale) = Rendezvous::from_socket_path(&socket_path).cleanup_path() {
        let _ = fs::remove_file(stale);
    }
    info!(project = %project_root.display(), "daemon stopped");
    Ok(())
}

fn sweep_dead_clients(registry: &SessionRegistry) {
    for id in registry.dead_session_ids(is_process_alive) {
        debug!(session = id, "sweeping session whose host pid is dead");
        registry.shutdown_session(id);
    }
}

fn start_project_watcher(
    project_root: &Path,
    options: &DaemonOptions,
) -> Option<codegraph_watch::ProjectWatcher> {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut watch_options = codegraph_watch::WatchOptions::default();
    watch_options.no_watch = !options.watch;
    watch_options.on_sync_complete =
        Some(Arc::new(move |outcome: codegraph_watch::SyncOutcome| {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let ts = local_timestamp();
            let tail = changed_paths_tail(&outcome.changed_paths);
            eprintln!(
                "[{ts}] [watcher] sync #{n}: {} file(s) reindexed, {} removed{tail}",
                outcome.files_reindexed, outcome.files_removed,
            );
        }));
    watch_options.on_degraded = Some(Arc::new(|reason: String| {
        eprintln!("[CodeGraph MCP] File watcher degraded — {reason}");
    }));
    watch_options.on_sync_error = Some(Arc::new(|reason: String| {
        eprintln!("[CodeGraph MCP] File watcher warning — {reason}");
    }));
    match codegraph_watch::start_serve_watcher(project_root, watch_options) {
        Ok(watcher) => watcher,
        Err(err) => {
            warn!(error = %err, "daemon failed to start project watcher");
            None
        }
    }
}

/// RFC 3339 local timestamp, falling back local -> UTC -> empty so logging
/// never panics on a missing TZ database.
fn local_timestamp() -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Bounded inline file list ` — a, b`: first 10 paths, then ` (+N more)`, so a
/// large batch can never produce a multi-kilobyte log line. Empty when no paths.
fn changed_paths_tail(paths: &[String]) -> String {
    const MAX: usize = 10;
    if paths.is_empty() {
        return String::new();
    }
    let shown = paths
        .iter()
        .take(MAX)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match paths.len().checked_sub(MAX) {
        Some(extra) if extra > 0 => format!(" — {shown} (+{extra} more)"),
        _ => format!(" — {shown}"),
    }
}

/// Spawn a ONE-SHOT background catch-up sync absorbing edits made while the
/// daemon was down (#905). Returns an `Arc<AtomicBool>` flipped `true` on
/// completion. Runs on a detached `std::thread`; the accept loop is never
/// blocked on it, so the first client's first tool call does not wait.
fn spawn_catch_up(project_root: &Path) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let root = project_root.to_path_buf();
    thread::spawn(move || {
        match codegraph_watch::sync_project_once(&root) {
            Ok(outcome) => {
                let changed = outcome.files_reindexed + outcome.files_removed;
                if changed > 0 {
                    let ts = local_timestamp();
                    eprintln!(
                        "[{ts}] [CodeGraph MCP] Caught up {changed} file(s) changed since last run"
                    );
                }
            }
            Err(err) => warn!(error = %err, "daemon catch-up sync failed"),
        }
        thread_done.store(true, Ordering::SeqCst);
    });
    done
}
