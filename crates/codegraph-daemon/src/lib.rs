//! Per-project daemon lifecycle for CodeGraph.
//!
//! This crate owns the task-24 daemon mechanics: project-scoped rendezvous paths,
//! atomic pid locks, cross-platform local-socket session handling, parent/host
//! watchdogs, graceful shutdown, and stale-lock recovery. It deliberately does
//! not implement task-25 file watching.

mod lock;
mod paths;
mod process;
mod session;
mod transport;

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use interprocess::local_socket::traits::Listener as _;
pub use lock::{
    clear_stale_daemon_lock, decode_lock_info, encode_lock_info, try_acquire_daemon_lock,
    unlock_project, AcquireResult, DaemonLockInfo,
};
pub use paths::{daemon_pid_path, daemon_socket_path};
pub use process::{current_ppid, is_process_alive, supervision_lost_reason, SupervisionState};
pub use session::{read_daemon_hello, SessionRegistry};
use tracing::{debug, info, warn};

use crate::lock::cleanup_owned_lock;
use crate::paths::codegraph_dir;
use crate::session::serve_session;
use crate::transport::{bind, connect, Listener, Rendezvous};

const DEFAULT_WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Env var name: when set to `"1"`, the process re-invoked by the launcher IS
/// the detached daemon and must listen+serve, never re-spawn.
pub const CODEGRAPH_DAEMON_INTERNAL: &str = "CODEGRAPH_DAEMON_INTERNAL";

/// Env var name: when set to `"1"`, the daemon is opted out and `serve --mcp`
/// runs in direct (single-process) mode.
pub const CODEGRAPH_NO_DAEMON: &str = "CODEGRAPH_NO_DAEMON";

#[derive(Clone, Debug)]
pub struct DaemonOptions {
    pub parent_pid: Option<u32>,
    pub host_pid: Option<u32>,
    pub watchdog_interval: Duration,
    pub run_mcp: bool,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            parent_pid: None,
            host_pid: None,
            watchdog_interval: DEFAULT_WATCHDOG_INTERVAL,
            run_mcp: true,
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
    let rendezvous = Rendezvous::from_socket_path(&socket_path);
    #[cfg(unix)]
    if let Some(stale) = rendezvous.cleanup_path() {
        if stale.exists() {
            fs::remove_file(stale)
                .with_context(|| format!("removing stale socket {}", stale.display()))?;
        }
    }
    let listener = bind(&rendezvous)
        .with_context(|| format!("binding daemon socket {}", socket_path.display()))?;
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
    info!(project = %project_root.display(), socket = %socket_path.display(), "daemon started");

    while !shutdown.load(Ordering::SeqCst) {
        let state = SupervisionState {
            original_ppid,
            current_ppid: current_ppid(),
            host_pid: options.host_pid,
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
