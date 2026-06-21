use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use codegraph_mcp::McpServer;
use serde::Serialize;

#[derive(Clone, Debug, Default)]
pub struct SessionRegistry {
    active: Arc<AtomicUsize>,
}

impl SessionRegistry {
    pub fn active_count(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    pub(crate) fn start_session(&self) -> SessionGuard {
        self.active.fetch_add(1, Ordering::SeqCst);
        SessionGuard {
            active: Arc::clone(&self.active),
        }
    }
}

pub(crate) struct SessionGuard {
    active: Arc<AtomicUsize>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
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
    mut stream: UnixStream,
    project_root: PathBuf,
    socket_path: String,
    registry: SessionRegistry,
    run_mcp: bool,
) -> Result<()> {
    let _guard = registry.start_session();
    // Port of upstream mcp/daemon.ts:253-262: every connection gets
    // a one-line versioned daemon hello before JSON-RPC bytes are forwarded.
    let hello = DaemonHello {
        codegraph: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        socket_path,
        protocol: 1,
    };
    writeln!(stream, "{}", serde_json::to_string(&hello)?)?;
    stream.flush()?;

    if !run_mcp {
        return Ok(());
    }

    // Port of upstream mcp/session.ts:78-115 in Rust form: one
    // session per connection, while the daemon process keeps the project store warm.
    let reader_stream = stream.try_clone().context("cloning daemon socket")?;
    let reader = BufReader::new(reader_stream);
    let mut server = McpServer::new(Some(project_root));
    server.run(reader, stream)
}

pub fn read_daemon_hello(stream: &mut UnixStream) -> Result<serde_json::Value> {
    let reader_stream = stream.try_clone().context("cloning daemon client socket")?;
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim())?)
}
