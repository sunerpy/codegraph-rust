use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use codegraph_mcp::McpServer;
use interprocess::local_socket::traits::Stream as _;
use serde::Serialize;

use crate::transport::Stream;

/// Tracks live client sessions plus a shared last-active instant bumped on BOTH
/// connect and disconnect, so the accept loop can measure idle time.
#[derive(Clone, Debug)]
pub struct SessionRegistry {
    active: Arc<AtomicUsize>,
    last_active: Arc<Mutex<Instant>>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            last_active: Arc::new(Mutex::new(Instant::now())),
        }
    }
}

impl SessionRegistry {
    pub fn active_count(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Milliseconds since the last connect-or-disconnect edge.
    pub fn millis_since_active(&self) -> u128 {
        let last = bump_or_read(&self.last_active, false);
        last.elapsed().as_millis()
    }

    pub(crate) fn start_session(&self) -> SessionGuard {
        self.active.fetch_add(1, Ordering::SeqCst);
        bump_or_read(&self.last_active, true);
        SessionGuard {
            active: Arc::clone(&self.active),
            last_active: Arc::clone(&self.last_active),
        }
    }
}

/// Bump the shared instant to now (when `bump`) or just read it. A poisoned
/// lock is recovered in place — the only datum behind it is one `Instant`.
fn bump_or_read(slot: &Mutex<Instant>, bump: bool) -> Instant {
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if bump {
        *guard = Instant::now();
    }
    *guard
}

pub(crate) struct SessionGuard {
    active: Arc<AtomicUsize>,
    last_active: Arc<Mutex<Instant>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        // Bump on disconnect so the linger window starts at the LAST client leaving.
        bump_or_read(&self.last_active, true);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonHello<'a> {
    pub codegraph: &'a str,
    pub pid: u32,
    pub socket_path: String,
    pub protocol: u8,
}

pub(crate) fn serve_session(
    stream: Stream,
    project_root: PathBuf,
    socket_path: String,
    registry: SessionRegistry,
    run_mcp: bool,
) -> Result<()> {
    let _guard = registry.start_session();
    // Split into independent recv/send halves: interprocess `Stream` has no
    // `try_clone`, and an `Arc<Stream>` exposes no `Read` impl, so the reader
    // and writer must own separate halves of the same connection.
    let (recv, mut send) = stream.split();

    // Port of upstream mcp/daemon.ts:253-262: every connection gets
    // a one-line versioned daemon hello before JSON-RPC bytes are forwarded.
    let hello = DaemonHello {
        codegraph: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        socket_path,
        protocol: 1,
    };
    writeln!(send, "{}", serde_json::to_string(&hello)?)?;
    send.flush()?;

    if !run_mcp {
        return Ok(());
    }

    // Port of upstream mcp/session.ts:78-115 in Rust form: one
    // session per connection, while the daemon process keeps the project store warm.
    let reader = BufReader::new(recv);
    let mut server = McpServer::new(Some(project_root));
    server.run(reader, send)
}

pub fn read_daemon_hello(stream: &mut Stream) -> Result<serde_json::Value> {
    let mut reader = BufReader::new(&*stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim())?)
}
