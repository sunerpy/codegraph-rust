//! Lazy shared-daemon ownership for repositories reached through MCP
//! `projectPath`.
//!
//! The MCP crate owns only an injectable, session-bounded broker to avoid a
//! dependency cycle. This module is the concrete integration: for an existing
//! index it starts or attaches the per-project daemon, retains one passive
//! client connection for the caller's lifetime, then performs one synchronous
//! catch-up before the first tool result is allowed to return. The daemon owns
//! the sole long-lived watcher/writer capability.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use codegraph_core::IndexPaths;
use serde_json::json;

use crate::proxy::verify_daemon_hello;
use crate::session::read_daemon_hello;
use crate::transport::{Rendezvous, Stream, connect};

const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const CONNECT_BUDGET: Duration = Duration::from_secs(8);
const CONNECT_RESULT_MARGIN: Duration = Duration::from_millis(250);

/// A passive daemon client retained by one MCP session. The daemon registry
/// counts this connection, so one session closing cannot stop the watcher while
/// another session still retains its own lease. The mutex only supplies `Sync`;
/// after the initial client hello no further bytes are written through it.
#[derive(Debug)]
pub struct ProjectDaemonLease {
    _stream: Mutex<Stream>,
    project_root: PathBuf,
    /// The daemon owner recorded when the lease attached.
    daemon_pid: Option<u32>,
    /// The index database the lease attached to, and its identity then.
    db_path: PathBuf,
    db_identity: Option<codegraph_store::PathIdentity>,
}

impl ProjectDaemonLease {
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

impl codegraph_mcp::ProjectService for ProjectDaemonLease {
    /// Live while the attached daemon still owns the project and the index
    /// database is the same file it was: a daemon crash, an `uninit`, or a
    /// re-created index each make the retained connection meaningless.
    fn is_live(&self) -> bool {
        let owner_unchanged = self
            .daemon_pid
            .is_some_and(|pid| daemon_owner_pid(&self.project_root) == Some(pid));
        let index_unchanged = self.db_identity.is_some_and(|identity| {
            codegraph_store::PathIdentity::of(&self.db_path).ok() == Some(identity)
        });
        owner_unchanged && index_unchanged
    }
}

/// Build the concrete broker used by CLI, HTTP, and nested daemon sessions.
/// Each broker owns at most the MCP crate's bounded number of explicit projects.
#[must_use]
pub fn project_service_broker(no_watch: bool) -> codegraph_mcp::ProjectServiceBroker {
    codegraph_mcp::ProjectServiceBroker::new(Arc::new(move |project_root| {
        let lease = retain_project_daemon(project_root, no_watch)?;
        Ok(Arc::new(lease) as codegraph_mcp::ProjectServiceHandle)
    }))
}

/// Start/attach the existing project's shared daemon, retain a session
/// connection, and wait for one whole-project catch-up before returning.
///
/// The existing-index check precedes every process or sync action, so this path
/// never creates `.codegraph` and never turns an arbitrary `projectPath` into a
/// default project. The daemon starts first: its watcher is already subscribed
/// while the synchronous catch-up runs, closing the scan-to-watch gap. A
/// concurrent daemon startup catch-up is harmless because the permanent index
/// lease serializes writers; this final catch-up runs after whichever pass won
/// the lease and therefore observes its result before the tool query executes.
pub fn retain_project_daemon(project_root: &Path, no_watch: bool) -> Result<ProjectDaemonLease> {
    let paths = IndexPaths::resolve(project_root, std::env::var("CODEGRAPH_DIR").ok().as_deref())?;
    let project_root = paths.project().to_path_buf();
    if !paths.current_root().is_dir() || !paths.current_db().is_file() {
        bail!(
            "refusing live services for unindexed project {}; run `codegraph init {}` first",
            project_root.display(),
            project_root.display()
        );
    }
    if let Some(reason) = codegraph_watch::too_broad_root_reason(&project_root) {
        bail!(
            "refusing live services for {}: {reason}",
            project_root.display()
        );
    }

    if !daemon_owner_live(&project_root) {
        // Liveness-gated cleanup only; a live owner's rendezvous is never
        // removed. Several racing sessions may all spawn children, but the
        // daemon pid lock and writer.pid converge them onto exactly one owner.
        let _ = crate::clear_stale_daemon_socket(&project_root);
        let executable = std::env::current_exe().context("resolving codegraph executable")?;
        crate::spawn_detached_daemon_for_project_service(&executable, &project_root, no_watch)
            .context("starting shared daemon for explicit projectPath")?;
    }

    let lease = connect_project_daemon_bounded(project_root.clone())?;
    codegraph_watch::sync_project_once(&project_root).with_context(|| {
        format!(
            "catching up explicit projectPath {} before its first result",
            project_root.display()
        )
    })?;
    Ok(lease)
}

fn daemon_owner_live(project_root: &Path) -> bool {
    daemon_owner_pid(project_root).is_some()
}

/// The live daemon owner's pid recorded in the project's `daemon.pid`.
fn daemon_owner_pid(project_root: &Path) -> Option<u32> {
    let pid_path = crate::daemon_pid_path(project_root).ok()?;
    let raw = std::fs::read_to_string(pid_path).ok()?;
    crate::decode_lock_info(&raw)
        .filter(|info| info.pid > 0 && crate::is_process_alive(info.pid))
        .map(|info| info.pid)
}

/// Cross-platform bounded connect/hello. Named-pipe reads have no portable
/// socket timeout, so the whole attempt lives on a worker and the caller waits
/// on a monotonic channel deadline. A wedged peer can strand at most one worker
/// for this failed first-access attempt; it cannot wedge the MCP request thread.
fn connect_project_daemon_bounded(project_root: PathBuf) -> Result<ProjectDaemonLease> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = connect_project_daemon_until(&project_root, CONNECT_BUDGET)
            .map_err(|error| format!("{error:#}"));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(CONNECT_BUDGET + CONNECT_RESULT_MARGIN) {
        Ok(Ok(lease)) => Ok(lease),
        Ok(Err(error)) => bail!("{error}"),
        Err(mpsc::RecvTimeoutError::Timeout) => bail!(
            "shared daemon did not become ready within {:?}",
            CONNECT_BUDGET
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("shared daemon connection worker ended without a result")
        }
    }
}

fn connect_project_daemon_until(
    project_root: &Path,
    budget: Duration,
) -> Result<ProjectDaemonLease> {
    let deadline = Instant::now() + budget;
    let mut last_error = None;
    while Instant::now() < deadline {
        let socket_path = match crate::recorded_socket_path(project_root) {
            Ok(path) => path,
            Err(error) => {
                last_error = Some(format!("resolving daemon rendezvous: {error:#}"));
                std::thread::sleep(CONNECT_RETRY_INTERVAL);
                continue;
            }
        };
        let rendezvous = Rendezvous::from_socket_path(&socket_path);
        match connect(&rendezvous) {
            Ok(mut stream) => {
                let hello = read_daemon_hello(&mut stream).with_context(|| {
                    format!("reading shared daemon hello at {}", socket_path.display())
                })?;
                if verify_daemon_hello(&hello).is_some() {
                    bail!(
                        "shared daemon at {} has an incompatible version/protocol: {}",
                        socket_path.display(),
                        hello
                    );
                }
                let client_hello = json!({ "hostPid": std::process::id() }).to_string();
                writeln!(&mut stream, "{client_hello}")
                    .context("sending retained project-service client hello")?;
                stream
                    .flush()
                    .context("flushing retained project-service client hello")?;
                let db_path = IndexPaths::resolve(
                    project_root,
                    std::env::var("CODEGRAPH_DIR").ok().as_deref(),
                )
                .map(|paths| paths.current_db())
                .unwrap_or_default();
                return Ok(ProjectDaemonLease {
                    _stream: Mutex::new(stream),
                    project_root: project_root.to_path_buf(),
                    daemon_pid: daemon_owner_pid(project_root),
                    db_identity: codegraph_store::PathIdentity::of(&db_path).ok(),
                    db_path,
                });
            }
            Err(error) => {
                last_error = Some(format!(
                    "connecting to daemon socket {}: {error:#}",
                    socket_path.display()
                ));
                std::thread::sleep(CONNECT_RETRY_INTERVAL);
            }
        }
    }
    bail!(
        "shared daemon was not reachable within {budget:?}: {}",
        last_error.unwrap_or_else(|| "no rendezvous became available".to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unindexed_project_is_rejected_without_creating_state() {
        let root = std::env::temp_dir().join(format!(
            "codegraph-project-service-unindexed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let error = retain_project_daemon(&root, false).expect_err("unindexed must fail");
        assert!(error.to_string().contains("unindexed project"));
        assert!(!root.join(".codegraph").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
