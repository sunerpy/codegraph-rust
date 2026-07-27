//! Versioned, project-identity-bound daemon CONTROL frames.
//!
//! Frozen plan lines 593-612. `uninit --force` holds the ONE outer exclusive
//! index lease for its whole lifecycle, so the frame that asks a live daemon to
//! shut down must NOT travel the ordinary data-request path — an ordinary request
//! takes its own SHARED lease and would deadlock against that exclusive holder.
//!
//! This module owns both ends of that narrow channel:
//!
//! * the wire form — a versioned JSON line carrying the FULL physical project
//!   identity, so a frame can never drain a daemon serving another project;
//! * [`request_daemon_shutdown`], the caller side used by `uninit`, which
//!   connects to the daemon's recorded rendezvous, sends the frame, and waits a
//!   bounded time for the ACK the daemon writes only AFTER it has stopped
//!   accepting, cancelled its watcher lease loops, drained, and removed its own
//!   pid/socket. No PID is ever signalled: an unresponsive daemon returns
//!   [`ShutdownOutcome::Unresponsive`] and the caller fails closed.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::transport::{Rendezvous, connect};

/// Wire version of the control protocol. A daemon refuses any other value rather
/// than guessing a peer's semantics.
pub const CONTROL_PROTOCOL: u8 = 1;

/// The only control action this version defines.
const SHUTDOWN_ACTION: &str = "shutdown";

/// Bounded wall-clock budget for the whole send-and-ACK exchange.
const SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Slack added to the caller-side wait so a daemon that answers right at its own
/// drain budget is still read, while the wait stays finite everywhere.
const SHUTDOWN_WAIT_MARGIN: Duration = Duration::from_secs(5);

/// A control frame sent on a fresh rendezvous connection, BEFORE any JSON-RPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlFrame {
    /// Discriminator that separates a control frame from a client hello and from
    /// a JSON-RPC request.
    pub codegraph_control: u8,
    /// The requested action.
    pub action: String,
    /// The FULL lowercase SHA-256 physical project identity the sender addresses.
    pub project_identity: String,
}

impl ControlFrame {
    #[must_use]
    pub fn shutdown(project_identity: &str) -> Self {
        Self {
            codegraph_control: CONTROL_PROTOCOL,
            action: SHUTDOWN_ACTION.to_string(),
            project_identity: project_identity.to_string(),
        }
    }

    /// Whether this frame is a shutdown request this build understands, bound to
    /// `project_identity`. A version, action, or identity mismatch is refused.
    #[must_use]
    pub fn authorizes_shutdown_of(&self, project_identity: &str) -> bool {
        self.codegraph_control == CONTROL_PROTOCOL
            && self.action == SHUTDOWN_ACTION
            && self.project_identity == project_identity
    }
}

/// The daemon's reply, written only after the drain completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlAck {
    pub codegraph_control: u8,
    pub action: String,
    /// `true` only when the daemon fully drained and removed its own rendezvous.
    pub drained: bool,
}

impl ControlAck {
    #[must_use]
    pub fn drained() -> Self {
        Self::for_drain(true)
    }

    /// The ack for a completed (`true`) or INCOMPLETE (`false`) drain. An
    /// incomplete drain must never be reported as success: the caller treats it as
    /// unresponsive and fails closed.
    #[must_use]
    pub fn for_drain(drained: bool) -> Self {
        Self {
            codegraph_control: CONTROL_PROTOCOL,
            action: SHUTDOWN_ACTION.to_string(),
            drained,
        }
    }

    #[must_use]
    pub fn accepted(&self) -> bool {
        self.codegraph_control == CONTROL_PROTOCOL && self.action == SHUTDOWN_ACTION && self.drained
    }
}

/// Parse one already-read line as a control frame. Anything else is `None`, and
/// the caller must treat the bytes as ordinary session input.
///
/// EVERY line that deserializes as a `ControlFrame` — including
/// `codegraph_control: 0` and any future version — is recognized here, so it is
/// routed to explicit authorization and refused. Rejecting a foreign version at
/// parse time instead would push those bytes into the JSON-RPC executor, which is
/// exactly the data path a control frame must never reach.
#[must_use]
pub fn parse_control_frame(line: &str) -> Option<ControlFrame> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<ControlFrame>(trimmed).ok()
}

/// Serialize a frame as one newline-terminated wire line.
#[must_use]
pub fn encode_control_line<T: Serialize>(value: &T) -> String {
    format!(
        "{}\n",
        serde_json::to_string(value).unwrap_or_else(|_| String::from("{}"))
    )
}

/// What the shutdown exchange established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// No live owner: nothing to drain.
    NoDaemon,
    /// The daemon ACKed after draining and removing its own pid/socket.
    Drained { pid: u32 },
    /// A live owner was recorded but never ACKed within the bounded budget. The
    /// caller MUST fail closed; the pid is reported, never signalled.
    Unresponsive { pid: u32, detail: String },
}

/// Ask the daemon recorded for `project_root` to shut down, and wait a bounded
/// time for its post-drain ACK.
///
/// `project_identity` is the full physical identity from `IndexPaths`, so the
/// frame authorizes draining ONLY the daemon serving this exact namespace.
pub fn request_daemon_shutdown(
    project_root: &Path,
    project_identity: &str,
) -> Result<ShutdownOutcome> {
    let pid_path = crate::paths::daemon_pid_path(project_root)?;
    let Some(info) = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|raw| crate::lock::decode_lock_info(&raw))
        .filter(|info| info.pid > 0)
    else {
        return Ok(ShutdownOutcome::NoDaemon);
    };
    if !crate::process::is_process_alive(info.pid) {
        return Ok(ShutdownOutcome::NoDaemon);
    }
    let socket_path = if info.socket_path.as_os_str().is_empty() {
        crate::paths::daemon_socket_path(project_root)?
    } else {
        info.socket_path.clone()
    };

    match bounded_exchange_shutdown(socket_path, project_identity.to_string()) {
        Ok(()) => Ok(ShutdownOutcome::Drained { pid: info.pid }),
        Err(detail) => Ok(ShutdownOutcome::Unresponsive {
            pid: info.pid,
            detail,
        }),
    }
}

/// Run the exchange on a worker thread and wait for it with a monotonic channel
/// deadline.
///
/// The per-socket send/recv timeouts `interprocess` exposes are Unix-only, so a
/// wedged Windows named pipe would otherwise block forever. Bounding the WAIT
/// itself (not the socket) is finite on every supported platform; the abandoned
/// worker owns only its own connection and never touches index bytes.
fn bounded_exchange_shutdown(socket_path: PathBuf, project_identity: String) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result =
            exchange_shutdown(&socket_path, &project_identity).map_err(|e| format!("{e:#}"));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(SHUTDOWN_ACK_TIMEOUT + SHUTDOWN_WAIT_MARGIN) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "no drain acknowledgement within {:?}",
            SHUTDOWN_ACK_TIMEOUT + SHUTDOWN_WAIT_MARGIN
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("the shutdown exchange ended without a result".to_string())
        }
    }
}

fn exchange_shutdown(socket_path: &Path, project_identity: &str) -> Result<()> {
    let rendezvous = Rendezvous::from_socket_path(socket_path);
    let stream = connect(&rendezvous)
        .with_context(|| format!("connecting to daemon socket {}", socket_path.display()))?;
    #[cfg(unix)]
    {
        use interprocess::local_socket::traits::Stream as _;
        stream
            .set_recv_timeout(Some(SHUTDOWN_ACK_TIMEOUT))
            .context("bounding the daemon shutdown ACK wait")?;
        stream
            .set_send_timeout(Some(SHUTDOWN_ACK_TIMEOUT))
            .context("bounding the daemon shutdown send")?;
    }

    // The daemon writes its versioned hello on accept; consume that line first so
    // the ACK read below cannot mistake it for the reply.
    let mut reader = BufReader::new(&stream);
    let mut hello = String::new();
    reader
        .read_line(&mut hello)
        .context("reading the daemon hello before sending a control frame")?;

    let frame = ControlFrame::shutdown(project_identity);
    (&stream)
        .write_all(encode_control_line(&frame).as_bytes())
        .context("sending the shutdown control frame")?;
    (&stream).flush().context("flushing the control frame")?;

    let mut ack_line = String::new();
    reader
        .read_line(&mut ack_line)
        .context("waiting for the daemon drain ACK")?;
    let ack: ControlAck = serde_json::from_str(ack_line.trim())
        .with_context(|| format!("decoding the daemon drain ACK {:?}", ack_line.trim()))?;
    anyhow::ensure!(
        ack.accepted(),
        "daemon reported an incomplete drain (drained={})",
        ack.drained
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &str = "a1b2c3";

    #[test]
    fn shutdown_frame_round_trips_and_binds_the_project_identity() {
        let frame = ControlFrame::shutdown(IDENTITY);
        let line = encode_control_line(&frame);
        assert!(line.ends_with('\n'));
        let parsed = parse_control_frame(&line).expect("a control frame parses");
        assert_eq!(parsed, frame);
        assert!(parsed.authorizes_shutdown_of(IDENTITY));
        assert!(
            !parsed.authorizes_shutdown_of("another-project"),
            "a frame must never authorize draining another project's daemon"
        );
    }

    #[test]
    fn a_client_hello_or_jsonrpc_line_is_not_a_control_frame() {
        assert!(parse_control_frame("").is_none());
        assert!(parse_control_frame("   \n").is_none());
        assert!(parse_control_frame("{\"hostPid\":4242}").is_none());
        assert!(parse_control_frame("{\"jsonrpc\":\"2.0\",\"id\":1}").is_none());
        assert!(parse_control_frame("not json").is_none());
    }

    #[test]
    fn a_foreign_protocol_version_is_never_authorized() {
        for version in [0, CONTROL_PROTOCOL + 1, u8::MAX] {
            let frame = ControlFrame {
                codegraph_control: version,
                action: SHUTDOWN_ACTION.to_string(),
                project_identity: IDENTITY.to_string(),
            };
            // Recognized as a control frame (so it is refused explicitly) and
            // never authorized.
            let parsed = parse_control_frame(&encode_control_line(&frame))
                .expect("every control-shaped line is recognized, not routed to JSON-RPC");
            assert_eq!(parsed.codegraph_control, version);
            assert!(!parsed.authorizes_shutdown_of(IDENTITY));
        }
        let frame = ControlFrame {
            codegraph_control: CONTROL_PROTOCOL + 1,
            action: SHUTDOWN_ACTION.to_string(),
            project_identity: IDENTITY.to_string(),
        };
        assert!(!frame.authorizes_shutdown_of(IDENTITY));
        let other_action = ControlFrame {
            codegraph_control: CONTROL_PROTOCOL,
            action: "reindex".to_string(),
            project_identity: IDENTITY.to_string(),
        };
        assert!(!other_action.authorizes_shutdown_of(IDENTITY));
    }

    #[test]
    fn only_a_drained_matching_ack_is_accepted() {
        assert!(ControlAck::drained().accepted());
        assert!(ControlAck::for_drain(true).accepted());
        let undrained = ControlAck::for_drain(false);
        assert!(
            !undrained.accepted(),
            "an incomplete drain must never be accepted as success"
        );
        assert_eq!(
            undrained,
            ControlAck {
                codegraph_control: CONTROL_PROTOCOL,
                action: SHUTDOWN_ACTION.to_string(),
                drained: false,
            }
        );
        let wrong_version = ControlAck {
            codegraph_control: CONTROL_PROTOCOL + 1,
            action: SHUTDOWN_ACTION.to_string(),
            drained: true,
        };
        assert!(!wrong_version.accepted());
    }

    #[test]
    fn no_recorded_owner_means_there_is_nothing_to_drain() {
        let project = std::env::temp_dir().join(format!(
            "cg-control-none-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let outcome = request_daemon_shutdown(&project, IDENTITY).expect("probe an idle project");
        assert_eq!(outcome, ShutdownOutcome::NoDaemon);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn a_live_owner_with_an_unreachable_socket_is_unresponsive_and_unsignalled() {
        let project = std::env::temp_dir().join(format!(
            "cg-control-unresponsive-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let paths = crate::paths::index_paths(&project).expect("resolve v2 paths");
        std::fs::create_dir_all(paths.current_root()).unwrap();
        // This process is the live "owner": a real live pid whose rendezvous
        // socket was never bound.
        let info = crate::lock::DaemonLockInfo {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            socket_path: paths.daemon_socket(),
            started_at: 1,
        };
        std::fs::write(
            paths.daemon_pid(),
            crate::lock::encode_lock_info(&info).unwrap(),
        )
        .unwrap();

        let outcome =
            request_daemon_shutdown(&project, IDENTITY).expect("an unreachable socket is data");
        match outcome {
            ShutdownOutcome::Unresponsive { pid, .. } => assert_eq!(pid, std::process::id()),
            other => panic!("expected an unresponsive outcome, got {other:?}"),
        }
        assert!(
            crate::process::is_process_alive(std::process::id()),
            "the recorded owner is never signalled"
        );
        let _ = std::fs::remove_dir_all(&project);
    }
}
