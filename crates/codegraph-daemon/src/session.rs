use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::traits::tokio::Stream as _;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::transport::{AsyncStream, Stream};

/// Max milliseconds the PROXY waits for the daemon's one-line versioned hello
/// after connecting, before giving up so the caller falls back to direct
/// serving. Without this bound a stale/wedged `daemon.sock` (a crashed daemon
/// that left its socket, or a daemon that accepted the connection but is stuck
/// before writing its hello) makes the proxy's `read_line` block forever —
/// which surfaces to an MCP host such as Kiro as a 60s "connection timed out"
/// on the `initialize` handshake. On timeout the hello read returns `Err`,
/// which `spawn_or_proxy` maps to direct serving.
#[cfg(unix)]
pub const DAEMON_HELLO_TIMEOUT_MS: u64 = 2000;

/// Hard cap on the OPTIONAL client-hello line length (mirrors colby
/// `MAX_HELLO_LINE_BYTES`). A first line longer than this is treated as "not a
/// hello" — its bytes are still handed intact to the JSON-RPC layer, never
/// dropped.
pub const MAX_HELLO_LINE_BYTES: usize = 4096;

/// The OPTIONAL hello a client (the proxy) MAY send back AFTER reading the
/// daemon hello and BEFORE any JSON-RPC: it announces the host process pid so
/// the daemon can reap the session if that pid dies (colby `DaemonClientHello`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonClientHello {
    pub host_pid: u32,
}

/// Try to parse one already-read line as a [`DaemonClientHello`], returning the
/// announced host pid. A line that is too long, is not JSON, or lacks `hostPid`
/// is NOT a hello (`None`) — the caller must then treat the line as the first
/// JSON-RPC frame and never drop it.
pub fn parse_client_hello_line(line: &str) -> Option<u32> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_HELLO_LINE_BYTES {
        return None;
    }
    serde_json::from_str::<DaemonClientHello>(trimmed)
        .ok()
        .map(|hello| hello.host_pid)
}

/// A session's force-close handle. Dropping the [`SessionGuard`] removes the
/// session from the registry; the sweep additionally calls
/// [`SessionRegistry::shutdown_session`] to force the session thread (blocked in
/// the rmcp session serve loop) to EOF by half/full-closing its socket.
#[derive(Clone)]
struct SessionEntry {
    pid: Option<u32>,
    #[cfg(unix)]
    recv_fd: Option<std::os::fd::RawFd>,
}

/// Tracks live client sessions (each with an OPTIONAL host pid + a shutdown
/// handle) plus a shared last-active instant bumped on BOTH connect and
/// disconnect, so the accept loop can measure idle time AND reap dead peers.
#[derive(Clone)]
pub struct SessionRegistry {
    sessions: Arc<Mutex<HashMap<u64, SessionEntry>>>,
    next_id: Arc<AtomicU64>,
    last_active: Arc<Mutex<Instant>>,
}

impl std::fmt::Debug for SessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRegistry")
            .field("active", &self.active_count())
            .finish_non_exhaustive()
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            last_active: Arc::new(Mutex::new(Instant::now())),
        }
    }
}

impl SessionRegistry {
    pub fn active_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Milliseconds since the last connect-or-disconnect edge.
    pub fn millis_since_active(&self) -> u128 {
        let last = bump_or_read(&self.last_active, false);
        last.elapsed().as_millis()
    }

    pub(crate) fn start_session(&self) -> SessionGuard {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                SessionEntry {
                    pid: None,
                    #[cfg(unix)]
                    recv_fd: None,
                },
            );
        bump_or_read(&self.last_active, true);
        SessionGuard {
            id,
            sessions: Arc::clone(&self.sessions),
            last_active: Arc::clone(&self.last_active),
        }
    }

    /// Record the OPTIONAL host pid parsed from a session's client-hello.
    pub(crate) fn set_pid(&self, session_id: u64, pid: u32) {
        if let Some(entry) = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            entry.pid = Some(pid);
        }
    }

    /// Record a session's recv-side socket fd so the sweep can force EOF by
    /// shutting it down (unix only).
    #[cfg(unix)]
    pub(crate) fn set_recv_fd(&self, session_id: u64, fd: std::os::fd::RawFd) {
        if let Some(entry) = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            entry.recv_fd = Some(fd);
        }
    }

    /// Return the ids of sessions whose host pid is KNOWN and NOT alive,
    /// per `is_alive`. Sessions with no announced pid are never returned (never
    /// swept).
    pub(crate) fn dead_session_ids(&self, is_alive: impl Fn(u32) -> bool) -> Vec<u64> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|(id, entry)| match entry.pid {
                Some(pid) if !is_alive(pid) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Force a dead session's reader to EOF by half-closing the read direction
    /// of its socket. The session thread, blocked in the rmcp serve loop, then sees
    /// EOF, returns, and drops its [`SessionGuard`] (which removes the entry +
    /// bumps last-active). No-op when the fd is unknown or on non-unix.
    pub(crate) fn shutdown_session(&self, session_id: u64) {
        #[cfg(unix)]
        {
            let fd = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&session_id)
                .and_then(|entry| entry.recv_fd);
            if let Some(fd) = fd {
                use std::os::fd::BorrowedFd;
                // SAFETY: `fd` is the live recv socket fd recorded by the session
                // thread, which owns the socket and outlives the sweep call (the
                // session only drops AFTER its reader EOFs, which this shutdown
                // triggers). We only issue shutdown(SHUT_RDWR); no ownership is
                // taken and the fd is not closed here.
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
                let _ = rustix::net::shutdown(borrowed, rustix::net::Shutdown::ReadWrite);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = session_id;
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
    id: u64,
    sessions: Arc<Mutex<HashMap<u64, SessionEntry>>>,
    last_active: Arc<Mutex<Instant>>,
}

impl SessionGuard {
    fn session_id(&self) -> u64 {
        self.id
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
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

/// Serve one async session over the daemon's tokio local-socket `stream`
/// through the rmcp handler: daemon hello on accept, optional client-hello pid
/// parse recorded BEFORE serving, force-close reap via the recorded recv fd
/// (unix), and no lost/duplicated bytes at the hello seam.
///
/// The recv half is split with interprocess's OWN async split (not
/// `tokio::io::split`) so its unix `AsFd` raw fd can be recorded for the
/// pid-sweep force-close; the (hello-consumed) recv remainder is re-chained via
/// [`tokio::io::AsyncReadExt::chain`] and joined with the send half into one
/// async transport handed to the standard rmcp serve. On non-unix the recv fd is
/// not recorded (no `SHUT_RDWR`), so the half-dead-peer force-close reap is
/// unix-only; ordinary disconnects still reap via async EOF everywhere.
pub(crate) async fn serve_session_async(
    stream: AsyncStream,
    project_root: PathBuf,
    socket_path: String,
    registry: SessionRegistry,
    run_mcp: bool,
) -> Result<()> {
    let guard = registry.start_session();
    let session_id = guard.session_id();

    let (recv, mut send) = stream.split();

    #[cfg(unix)]
    {
        use interprocess::local_socket::tokio::RecvHalf;
        use std::os::fd::{AsFd, AsRawFd};
        #[allow(irrefutable_let_patterns)]
        if let RecvHalf::UdSocket(uds) = &recv {
            registry.set_recv_fd(session_id, uds.as_fd().as_raw_fd());
        }
    }

    let hello = DaemonHello {
        codegraph: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        socket_path,
        protocol: 1,
    };
    let hello_line = format!("{}\n", serde_json::to_string(&hello)?);
    send.write_all(hello_line.as_bytes()).await?;
    send.flush().await?;

    if !run_mcp {
        return Ok(());
    }

    let (first, recv) = read_first_line_bounded_async(recv, MAX_HELLO_LINE_BYTES + 1).await;
    let line = String::from_utf8_lossy(&first);
    let pid = parse_client_hello_line(&line);
    if let Some(pid) = pid {
        registry.set_pid(session_id, pid);
    }

    // Re-chain: when the first line was a hello it is fully consumed (empty
    // put-back); otherwise the first frame's bytes are prepended intact so no
    // byte is lost or duplicated at the seam.
    let put_back: Vec<u8> = if pid.is_some() { Vec::new() } else { first };
    let chained = AsyncReadExt::chain(Cursor::new(put_back), recv);
    let transport = tokio::io::join(chained, send);
    codegraph_mcp::rmcp_session::serve_session_rmcp_async(transport, project_root).await?;
    Ok(())
}

/// Async analog of [`read_first_line_bounded`]: read from `recv` up to and
/// including the first `\n`, or `max` bytes, whichever comes first. Returns the
/// raw bytes read plus the (unconsumed) recv half so the caller can chain the
/// put-back bytes in front of it. One-byte reads consume EXACTLY the first line.
async fn read_first_line_bounded_async(
    mut recv: crate::transport::AsyncRecvHalf,
    max: usize,
) -> (Vec<u8>, crate::transport::AsyncRecvHalf) {
    let mut buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    while buf.len() < max {
        match recv.read(&mut byte).await {
            Ok(0) => break,
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (buf, recv)
}

/// The session reader/writer bounds. The rmcp path (`serve_session_rmcp`) moves
/// the halves into a tokio runtime, so it needs `Send + Unpin + 'static`. A
/// blanket impl keeps these as zero-cost aliases over the real traits.
pub trait SessionReader: BufRead + Send + Unpin + 'static {}
impl<T: BufRead + Send + Unpin + 'static> SessionReader for T {}
pub trait SessionWriter: Write + Send + Unpin + 'static {}
impl<T: Write + Send + Unpin + 'static> SessionWriter for T {}

/// Buffer-safe generic recv seam (the T9 GENERIC SEAM): read the OPTIONAL
/// client-hello line from `reader`, surface the parsed host pid via
/// `on_client_pid` BEFORE blocking, then hand the SAME logical stream to
/// `serve_session_rmcp(reader, writer, ...)`. Also returns the parsed host pid (`Some`)
/// when the first line was a [`DaemonClientHello`], else `None`.
///
/// `on_client_pid` is invoked exactly once, immediately after the hello phase
/// and BEFORE the (blocking) rmcp serve loop, so a long-lived daemon can record
/// the pid in time for its dead-client sweep. Tests that only care about the
/// return value pass a no-op.
///
/// BUFFER SAFETY: there is exactly ONE reader. We read the first line's bytes
/// once. If they parse as a client-hello we consume them and continue with the
/// remaining `reader`. If they do NOT (a normal client whose first line is its
/// first JSON-RPC frame, OR a timed-out/partial read), we PREPEND the bytes we
/// already read back in front of `reader` via `Cursor::chain`, so the session
/// sees the complete, in-order stream and no byte is ever lost.
pub fn run_session_recv<R: SessionReader, W: SessionWriter>(
    reader: R,
    writer: W,
    project_root: PathBuf,
    run_mcp: bool,
) -> Result<Option<u32>> {
    run_session_recv_with(reader, writer, project_root, run_mcp, |_| {})
}

/// [`run_session_recv`] plus an `on_client_pid` hook fired right after the hello
/// phase and before the blocking run. See [`run_session_recv`] for the buffer
/// safety contract.
pub fn run_session_recv_with<R, W, F>(
    mut reader: R,
    writer: W,
    project_root: PathBuf,
    run_mcp: bool,
    on_client_pid: F,
) -> Result<Option<u32>>
where
    R: SessionReader,
    W: SessionWriter,
    F: FnOnce(Option<u32>),
{
    if !run_mcp {
        on_client_pid(None);
        return Ok(None);
    }

    // Read the first line's bytes ONCE, bounded by MAX_HELLO_LINE_BYTES + 1 (the
    // trailing newline). A timeout/partial read leaves whatever arrived in
    // `first`, which is then prepended intact below.
    let first = read_first_line_bounded(&mut reader, MAX_HELLO_LINE_BYTES + 1);

    let line = String::from_utf8_lossy(&first);
    if let Some(pid) = parse_client_hello_line(&line) {
        // It WAS a client-hello: surface the pid BEFORE blocking, then consume
        // it and serve the rest from the same reader (no bytes to prepend — the
        // hello line is fully consumed).
        on_client_pid(Some(pid));
        serve_stream(reader, writer, project_root)?;
        return Ok(Some(pid));
    }

    // NOT a hello: `first` is the first JSON-RPC frame (or partial bytes). We
    // must not lose it — prepend it in front of the remaining reader so the
    // session sees one continuous, in-order stream.
    on_client_pid(None);
    let chained = BufReader::new(std::io::Read::chain(Cursor::new(first), reader));
    serve_stream(chained, writer, project_root)?;
    Ok(None)
}

/// Serve one session's (hello-consumed) stream to EOF through the rmcp
/// [`CodeGraphHandler`] (the sole MCP transport). Blocks until the reader hits
/// EOF (socket half/full-close), so the session thread returns and its
/// [`SessionGuard`] drops — preserving the dead-client reap contract.
fn serve_stream<R, W>(reader: R, writer: W, project_root: PathBuf) -> Result<()>
where
    R: SessionReader,
    W: SessionWriter,
{
    codegraph_mcp::rmcp_session::serve_session_rmcp(reader, writer, project_root)
}

/// Read bytes from `reader` up to and including the first `\n`, or until `max`
/// bytes have been read, whichever comes first. Returns the raw bytes read
/// (which may be empty on immediate EOF/timeout, or a partial line on a bounded
/// stop). Uses one-byte reads so it consumes EXACTLY the first line and not a
/// byte more — the rest stays buffered in `reader` for the JSON-RPC layer.
fn read_first_line_bounded<R: BufRead>(reader: &mut R, max: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    while buf.len() < max {
        match reader.read(&mut byte) {
            Ok(0) => break, // EOF
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            // Timeout (WouldBlock/TimedOut) or any read error: stop and hand
            // back whatever we have. Nothing is lost — partial bytes are
            // prepended by the caller.
            Err(_) => break,
        }
    }
    buf
}

pub fn read_daemon_hello(stream: &mut Stream) -> Result<serde_json::Value> {
    #[cfg(unix)]
    let _ = stream.set_recv_timeout(Some(std::time::Duration::from_millis(
        DAEMON_HELLO_TIMEOUT_MS,
    )));
    let result = (|| {
        let mut reader = BufReader::new(&*stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(serde_json::from_str(line.trim())?)
    })();
    #[cfg(unix)]
    let _ = stream.set_recv_timeout(None);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_hello_line_extracts_host_pid_from_valid_json() {
        assert_eq!(parse_client_hello_line("{\"hostPid\":4242}"), Some(4242));
        assert_eq!(parse_client_hello_line("  {\"hostPid\": 7}  \n"), Some(7));
    }

    #[test]
    fn parse_client_hello_line_rejects_non_hello_lines() {
        assert_eq!(parse_client_hello_line(""), None);
        assert_eq!(parse_client_hello_line("   "), None);
        assert_eq!(parse_client_hello_line("not json"), None);
        assert_eq!(
            parse_client_hello_line("{\"jsonrpc\":\"2.0\",\"id\":1}"),
            None
        );
        let too_long = format!(
            "{{\"hostPid\":1,\"pad\":\"{}\"}}",
            "x".repeat(MAX_HELLO_LINE_BYTES)
        );
        assert_eq!(parse_client_hello_line(&too_long), None);
    }

    #[test]
    fn run_session_recv_with_run_mcp_false_fires_none_pid_and_returns_none() {
        let reader = std::io::Cursor::new(Vec::<u8>::new());
        let writer = Vec::<u8>::new();
        let mut fired: Option<Option<u32>> = None;
        let result =
            run_session_recv_with(reader, writer, PathBuf::from("/tmp/unused"), false, |pid| {
                fired = Some(pid)
            })
            .expect("run_mcp=false short-circuits without serving");
        assert_eq!(result, None);
        assert_eq!(fired, Some(None));
    }

    #[test]
    fn session_registry_tracks_active_count_and_pid_and_dead_ids() {
        let registry = SessionRegistry::default();
        assert_eq!(registry.active_count(), 0);

        let guard = registry.start_session();
        let id = guard.session_id();
        assert_eq!(registry.active_count(), 1);

        registry.set_pid(id, 1234);
        let dead = registry.dead_session_ids(|pid| pid != 1234);
        assert_eq!(dead, vec![id]);
        let alive = registry.dead_session_ids(|_| true);
        assert!(alive.is_empty());

        drop(guard);
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn session_registry_millis_since_active_advances_over_time() {
        let registry = SessionRegistry::default();
        let first = registry.millis_since_active();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let later = registry.millis_since_active();
        assert!(later >= first);
    }

    #[test]
    fn session_registry_start_bumps_last_active_to_recent() {
        let registry = SessionRegistry::default();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let guard = registry.start_session();
        assert!(
            registry.millis_since_active() < 10,
            "starting a session resets last-active to now"
        );
        drop(guard);
    }

    #[test]
    fn session_registry_debug_reports_active_count() {
        let registry = SessionRegistry::default();
        let debug = format!("{registry:?}");
        assert!(debug.contains("SessionRegistry"));
        assert!(debug.contains("active"));
    }

    #[test]
    fn set_pid_on_unknown_session_is_a_noop() {
        let registry = SessionRegistry::default();
        registry.set_pid(9999, 1);
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn read_first_line_bounded_stops_at_newline_and_on_eof() {
        use std::io::BufReader;
        let mut reader = BufReader::new(std::io::Cursor::new(b"hello\nworld".to_vec()));
        let first = read_first_line_bounded(&mut reader, 64);
        assert_eq!(first, b"hello\n");
        let mut empty = BufReader::new(std::io::Cursor::new(Vec::<u8>::new()));
        assert!(read_first_line_bounded(&mut empty, 64).is_empty());
    }
}

#[cfg(all(test, unix))]
mod hello_timeout_tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use interprocess::local_socket::traits::Listener as _;

    use crate::transport::{Rendezvous, bind, connect};

    #[test]
    fn read_daemon_hello_times_out_on_silent_socket() {
        let dir = std::env::temp_dir().join(format!(
            "cg-hello-timeout-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let socket_path = dir.join("daemon.sock");
        let rendezvous = Rendezvous::from_socket_path(&socket_path);

        let listener = bind(&rendezvous).expect("bind listener");
        let acceptor = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok(stream) => {
                        thread::sleep(Duration::from_secs(4));
                        drop(stream);
                        return;
                    }
                    Err(_) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        });

        let mut stream = connect(&rendezvous).expect("connect to listener");
        let started = Instant::now();
        let result = super::read_daemon_hello(&mut stream);
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "a daemon that never sends a hello must surface as Err, not a value"
        );
        assert!(
            elapsed < Duration::from_millis(super::DAEMON_HELLO_TIMEOUT_MS + 1500),
            "hello read must give up near the bound, not hang (elapsed {elapsed:?})"
        );

        drop(stream);
        let _ = acceptor.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
