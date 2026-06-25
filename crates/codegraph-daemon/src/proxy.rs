//! MCP proxy mode (issue #411 / colby `mcp/proxy.ts`).
//!
//! The launcher process the MCP host actually spawns becomes a thin
//! stdio<->socket bridge to the shared daemon. Unlike a raw byte pump, this is
//! the LOCAL-HANDSHAKE proxy (colby `runLocalHandshakeProxy`, proxy.ts:204-378):
//!
//!   * `initialize` and `tools/list` are answered LOCALLY from this build's
//!     static constants the instant the host asks, so tool registration is
//!     instant and the daemon cold-start race is avoided. The `initialize` is
//!     ALSO forwarded to the daemon (to prime its engine), but the daemon's
//!     reply to that id is SUPPRESSED — the host already got the local answer.
//!   * `tools/list` is answered locally and NOT forwarded.
//!   * Every OTHER JSON-RPC line is forwarded verbatim host<->daemon.
//!
//! The daemon's one-line versioned hello is consumed and DISCARDED here — it is
//! NOT JSON-RPC and must never reach the host's stdout. Its `codegraph` version
//! and `protocol` are verified against this build; a mismatch returns
//! [`ProxyOutcome::VersionMismatch`] so the caller falls back to direct serving.
//!
//! A PPID watchdog (colby proxy.ts:380-401) forces the proxy to exit if the MCP
//! host dies without closing stdin (SIGKILL on POSIX). The proxy does NOT send a
//! client-hello yet — that is T9.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use interprocess::local_socket::traits::Stream as _;
use serde_json::{json, Value};

use crate::process::{current_ppid, is_process_alive, supervision_lost_reason, SupervisionState};
use crate::session::read_daemon_hello;
use crate::transport::{connect, Rendezvous};

/// The wire protocol version the daemon advertises in its hello
/// (`session.rs` `DaemonHello.protocol`). Proxy and daemon must agree.
const EXPECTED_PROTOCOL: u64 = 1;

/// Poll cadence for the PPID watchdog (mirrors colby `DEFAULT_PPID_POLL_MS`).
const PPID_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Outcome of a proxy attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum ProxyOutcome {
    /// Successfully attached to a same-version daemon and piped stdio until one
    /// end closed. The caller should exit cleanly (do NOT also serve direct).
    Proxied,
    /// The daemon hello did not match this build (version/protocol mismatch).
    /// The caller should transparently fall back to direct serving.
    VersionMismatch,
}

/// Verify the daemon hello matches THIS build: `codegraph` version equals
/// `CARGO_PKG_VERSION` and `protocol` equals [`EXPECTED_PROTOCOL`].
///
/// Returns `None` on a match (proceed) or `Some(VersionMismatch)` on any
/// divergence. Exposed (`pub`) so the daemon-crate integration test can assert
/// the mismatch branch without standing up a real daemon.
pub fn verify_daemon_hello(hello: &Value) -> Option<ProxyOutcome> {
    let version = hello.get("codegraph").and_then(Value::as_str);
    let protocol = hello.get("protocol").and_then(Value::as_u64);
    if version == Some(env!("CARGO_PKG_VERSION")) && protocol == Some(EXPECTED_PROTOCOL) {
        None
    } else {
        Some(ProxyOutcome::VersionMismatch)
    }
}

/// Run the local-handshake proxy: connect to the daemon at `socket_path`,
/// verify+discard its hello, then bridge `host_in`/`host_out` to the daemon
/// using JSON-RPC newline framing, answering `initialize`+`tools/list` locally.
///
/// `host_ppid` (typically [`current_ppid`]) drives a watchdog that exits the
/// proxy if the host dies without closing stdin. Returns
/// [`ProxyOutcome::Proxied`] once either stream closes, or
/// [`ProxyOutcome::VersionMismatch`] if the daemon is the wrong version (caller
/// falls back to direct).
pub fn run_proxy<R: BufRead, W: Write + Send + 'static>(
    socket_path: &Path,
    host_ppid: Option<u32>,
    host_in: R,
    host_out: W,
) -> Result<ProxyOutcome> {
    let rendezvous = Rendezvous::from_socket_path(socket_path);
    let mut stream = connect(&rendezvous)
        .with_context(|| format!("connecting to daemon socket {}", socket_path.display()))?;

    // Consume + DISCARD the daemon hello line. It is NOT JSON-RPC; it must never
    // reach the host. `read_daemon_hello` builds a throwaway BufReader, reads ONE
    // line, and drops it — safe here because the daemon sends the hello alone and
    // only begins forwarding JSON-RPC after the proxy starts writing (T9 will
    // refactor the daemon side to a single long-lived reader for the client
    // hello; the proxy does not send one yet).
    let hello = read_daemon_hello(&mut stream).context("reading daemon hello")?;
    if let Some(mismatch) = verify_daemon_hello(&hello) {
        return Ok(mismatch);
    }

    // Split into independent recv/send halves. interprocess's sync UDS split
    // hands BOTH halves an `Arc` over the SAME fd, so merely DROPPING the send
    // half does not signal EOF to the daemon — the fd stays open via the recv
    // half. We therefore capture the WRITE-side fd before moving `send` into the
    // up pump and, once the host side is done, explicitly half-close it
    // (shutdown(SHUT_WR)); that is what makes the daemon's session reader hit
    // EOF, flush its last reply, and close — which in turn EOFs our recv pump so
    // `down.join()` never hangs. The fd stays valid through teardown because the
    // recv half keeps the shared socket open.
    let (recv, mut send) = stream.split();
    let write_fd = write_raw_fd(&send);

    // Send the OPTIONAL client-hello FIRST (T9), before any JSON-RPC: it
    // announces the host pid this proxy serves so the daemon can reap our
    // session if the host dies. The daemon reads it from its ONE long-lived
    // recv reader; a daemon that does not understand it simply ignores a
    // non-JSON-RPC first line. Use the served host pid when known, else our own
    // parent pid.
    let host_pid = host_ppid.unwrap_or_else(current_ppid);
    let client_hello = json!({ "hostPid": host_pid }).to_string();
    forward_to_daemon(&mut send, &client_hello).context("sending client hello")?;

    // Shared shutdown flag flipped by the watchdog on host death and polled
    // per-line by the up pump. Its byte-for-byte pump semantics are unchanged.
    let shutdown = Arc::new(AtomicBool::new(false));
    // Event channel the watchdog parks on: lets teardown wake it the instant
    // shutdown is decided instead of after the remainder of a poll interval.
    let watchdog_wake = Arc::new(Shutdown::new());
    // The forwarded `initialize` id whose daemon reply must be suppressed.
    let suppressed_id: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    // Both directions write to the host; serialize them behind one lock so an
    // interleaved daemon reply can't split a local answer mid-line.
    let host_out = Arc::new(Mutex::new(host_out));

    // PPID watchdog: a SIGKILL'd host never closes stdin on POSIX, so poll the
    // host pid and flip shutdown when supervision is lost (colby proxy.ts:380).
    let watchdog =
        spawn_ppid_watchdog(host_ppid, Arc::clone(&watchdog_wake), Arc::clone(&shutdown));

    // daemon -> host pump (own thread): forward every daemon line to the host,
    // except the suppressed-initialize reply.
    let socket_reader = BufReader::new(recv);
    let down_suppressed = Arc::clone(&suppressed_id);
    let down_out = Arc::clone(&host_out);
    let down =
        thread::spawn(move || pump_daemon_to_host(socket_reader, &down_out, &down_suppressed));

    // host -> daemon pump (this thread): answer initialize/tools-list locally,
    // forward the rest. Runs to completion on host_in EOF.
    let up_result = pump_host_to_daemon(host_in, send, &host_out, &shutdown, &suppressed_id);

    // Host side is done. Half-close the write direction so the daemon reader
    // EOFs (it flushes its final reply first); the down pump then drains those
    // replies and exits on its own EOF. Do NOT flip `shutdown` before the join
    // or it would race the drain and drop the last reply.
    half_close_write(write_fd);
    let _ = down.join();
    shutdown.store(true, Ordering::SeqCst);
    // Wake the watchdog at once so its join (in drop) returns promptly instead
    // of waiting out the remainder of a poll interval.
    watchdog_wake.signal();
    drop(watchdog);

    up_result?;
    Ok(ProxyOutcome::Proxied)
}

/// The write-side fd handle carried between `write_raw_fd` and
/// `half_close_write`. On unix it is the real socket `RawFd`; on non-unix there
/// is no half-close and the value is always `None`, so a unit placeholder keeps
/// both cfg variants' signatures structurally identical for the cfg-agnostic
/// caller. NEVER name `std::os::fd::*` outside the `#[cfg(unix)]` path — that
/// module does not exist on Windows.
#[cfg(unix)]
type WriteFd = std::os::fd::RawFd;
#[cfg(not(unix))]
type WriteFd = ();

/// Capture the write-side raw fd from the send half before it is moved into the
/// up pump. `None` on non-unix (no half-close there).
#[cfg(unix)]
fn write_raw_fd(send: &crate::transport::SendHalf) -> Option<WriteFd> {
    use std::os::fd::{AsFd, AsRawFd};
    // The enum `SendHalf` does not surface `AsFd`/`AsRawFd`; the concrete
    // `UdSocket` variant does. Match it to read the raw fd.
    match send {
        interprocess::local_socket::SendHalf::UdSocket(uds) => Some(uds.as_fd().as_raw_fd()),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

#[cfg(not(unix))]
fn write_raw_fd(_send: &crate::transport::SendHalf) -> Option<WriteFd> {
    None
}

/// Half-close the WRITE direction of the daemon socket (`shutdown(SHUT_WR)`),
/// leaving the read direction open to drain the daemon's final reply. This is
/// the EOF signal the daemon's blocking line-reader needs; a plain drop of the
/// send half is insufficient because interprocess shares one fd across halves.
#[cfg(unix)]
fn half_close_write(write_fd: Option<WriteFd>) {
    use std::os::fd::BorrowedFd;
    if let Some(fd) = write_fd {
        // SAFETY: `fd` is the live socket fd captured at split time; the recv
        // half still owns the socket, so the fd is valid for this borrow. We
        // only issue shutdown(SHUT_WR) on it — no ownership is taken.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let _ = rustix::net::shutdown(borrowed, rustix::net::Shutdown::Write);
    }
}

/// Windows named pipes have no half-close; the proxy relies on the full-stream
/// drop + the daemon's own idle/sweep lifecycle instead.
#[cfg(not(unix))]
fn half_close_write(_write_fd: Option<WriteFd>) {}

/// host -> daemon: read host_in line-by-line; answer `initialize`+`tools/list`
/// locally, forward everything else. On `initialize`, ALSO forward it to prime
/// the daemon engine and record its id so the daemon reply is suppressed.
fn pump_host_to_daemon<R, S, W>(
    host_in: R,
    mut daemon_send: S,
    host_out: &Arc<Mutex<W>>,
    shutdown: &Arc<AtomicBool>,
    suppressed_id: &Arc<Mutex<Option<Value>>>,
) -> Result<()>
where
    R: BufRead,
    S: Write,
    W: Write,
{
    for line in host_in.lines() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let parsed: Option<Value> = serde_json::from_str(&line).ok();
        let method = parsed
            .as_ref()
            .and_then(|v| v.get("method"))
            .and_then(Value::as_str);
        let id = parsed.as_ref().and_then(|v| v.get("id")).cloned();

        match method {
            Some("initialize") => {
                // Answer locally, then forward to prime the daemon and suppress
                // its reply to this id.
                if let Some(id) = id.clone() {
                    write_host_line(host_out, &reply(&id, codegraph_mcp::initialize_result()))?;
                    *suppressed_id
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
                }
                forward_to_daemon(&mut daemon_send, &line)?;
            }
            Some("tools/list") => {
                // Answer locally; do NOT forward (the daemon would re-answer it).
                if let Some(id) = id {
                    let tools = json!({
                        "tools": codegraph_mcp::schemas::visible_tool_definitions()
                    });
                    write_host_line(host_out, &reply(&id, tools))?;
                }
            }
            _ => {
                // Everything else (tools/call, ping, notifications, ...) is
                // forwarded verbatim to the daemon.
                forward_to_daemon(&mut daemon_send, &line)?;
            }
        }
    }
    Ok(())
}

/// daemon -> host: forward each daemon line to the host, dropping the response
/// to the suppressed-initialize id. Drains to socket EOF (NOT a `shutdown`
/// flag): the daemon closes the socket only after flushing its last reply, so
/// exiting on EOF alone guarantees the final `tools/call` answer is delivered.
fn pump_daemon_to_host<S, W>(
    daemon_recv: S,
    host_out: &Arc<Mutex<W>>,
    suppressed_id: &Arc<Mutex<Option<Value>>>,
) -> Result<()>
where
    S: BufRead,
    W: Write,
{
    for line in daemon_recv.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        // Suppress the daemon's reply to the forwarded initialize id.
        if let Ok(resp) = serde_json::from_str::<Value>(&line) {
            let is_reply = resp.get("result").is_some() || resp.get("error").is_some();
            if is_reply {
                let resp_id = resp.get("id");
                let suppressed = suppressed_id
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let (Some(resp_id), Some(want)) = (resp_id, suppressed.as_ref()) {
                    if resp_id == want {
                        continue;
                    }
                }
            }
        }

        write_host_line(host_out, &line)?;
    }
    Ok(())
}

/// Build a JSON-RPC 2.0 success response line for `id` with `result`.
fn reply(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// Write one newline-framed line to the shared host writer and flush.
fn write_host_line<W: Write>(host_out: &Arc<Mutex<W>>, line: &str) -> Result<()> {
    let mut out = host_out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

/// Forward one host line to the daemon socket with newline framing + flush.
fn forward_to_daemon<S: Write>(daemon_send: &mut S, line: &str) -> Result<()> {
    writeln!(daemon_send, "{line}")?;
    daemon_send.flush()?;
    Ok(())
}

/// Event-driven shutdown signal for the PPID watchdog.
///
/// The watchdog must still wake every [`PPID_POLL_INTERVAL`] to re-run the
/// supervision check (host death is detected by polling, not by an event), but
/// the *shutdown* itself is an event: when teardown signals it the watchdog has
/// to wake at once so `WatchdogGuard::drop`'s join returns promptly instead of
/// blocking out the remainder of a `thread::sleep` (the old up-to-500ms stall).
///
/// A `Condvar` over a `Mutex<bool>` gives exactly that: [`wait_timeout`] parks
/// until either the timer elapses (a poll tick) or [`signal`] wakes it (a
/// shutdown). The `bool` is also the predicate guarding against the lost-wakeup
/// race — a `signal()` that lands before the wait is seen on entry.
///
/// [`wait_timeout`]: Shutdown::wait_timeout
/// [`signal`]: Shutdown::signal
struct Shutdown {
    signaled: Mutex<bool>,
    woke: Condvar,
}

impl Shutdown {
    fn new() -> Self {
        Self {
            signaled: Mutex::new(false),
            woke: Condvar::new(),
        }
    }

    /// Raise the shutdown event and wake every parked waiter at once.
    fn signal(&self) {
        let mut guard = self
            .signaled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = true;
        self.woke.notify_all();
    }

    /// Park until `signal()` fires or `timeout` elapses. Returns `true` when the
    /// wake was a shutdown signal, `false` on a plain timeout (a poll tick). The
    /// predicate is checked under the lock first, so a signal racing ahead of
    /// the wait is never missed and spurious wakeups never report a false
    /// shutdown.
    fn wait_timeout(&self, timeout: Duration) -> bool {
        let guard = self
            .signaled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (guard, _) = self
            .woke
            .wait_timeout_while(guard, timeout, |&mut signaled| !signaled)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
}

/// Spawn the PPID watchdog. Returns a guard whose drop joins the thread; the
/// thread exits the instant `wake` is signaled (by teardown or by its own
/// host-death detection) — the `Condvar` wake makes the join return without
/// waiting out a poll.
///
/// Two signals, two consumers: `wake` (a `Condvar`) is what the watchdog itself
/// parks on, so teardown can break it immediately; `pump_shutdown` (the
/// `AtomicBool` the pump loops poll per-line) is flipped by the watchdog when it
/// detects host death, so the host->daemon pump tears down too. On host death
/// the watchdog flips BOTH; on a clean teardown the caller signals `wake` (the
/// up pump has already exited on its own EOF, so its `AtomicBool` is moot).
fn spawn_ppid_watchdog(
    host_ppid: Option<u32>,
    wake: Arc<Shutdown>,
    pump_shutdown: Arc<AtomicBool>,
) -> WatchdogGuard {
    let original_ppid = current_ppid();
    let handle = thread::spawn(move || loop {
        if wake.wait_timeout(PPID_POLL_INTERVAL) {
            break;
        }
        let state = SupervisionState {
            original_ppid,
            current_ppid: current_ppid(),
            host_pid: host_ppid,
        };
        if supervision_lost_reason(&state, is_process_alive).is_some() {
            pump_shutdown.store(true, Ordering::SeqCst);
            wake.signal();
            break;
        }
    });
    WatchdogGuard {
        handle: Some(handle),
    }
}

struct WatchdogGuard {
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for WatchdogGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A watchdog still needs to wake every poll interval to run the supervision
    /// check (host-death detection is a poll, not an event), but the *shutdown*
    /// signal must wake it immediately. With a plain `thread::sleep`, a signal
    /// raised mid-sleep is invisible until the sleep elapses, so `drop`+`join`
    /// blocked up to one full poll interval (~500ms) on every clean exit.
    ///
    /// `Shutdown::wait_timeout` is the fix: it parks on a `Condvar` and returns
    /// the instant `signal()` is called. This test parks a thread on a LONG
    /// timeout, signals from the main thread, and asserts the parked thread
    /// woke in WELL UNDER the timeout — proving the wake is event-driven, not a
    /// timer that must run out.
    #[test]
    fn shutdown_signal_wakes_waiter_immediately() {
        let long = Duration::from_secs(10);
        let shutdown = Arc::new(Shutdown::new());

        let waiter = Arc::clone(&shutdown);
        let parked = thread::spawn(move || {
            let start = Instant::now();
            // Returns `true` only if shutdown was signaled (vs. timing out).
            let signaled = waiter.wait_timeout(long);
            (signaled, start.elapsed())
        });

        // Give the waiter a beat to actually park on the condvar, then signal.
        thread::sleep(Duration::from_millis(20));
        shutdown.signal();

        let (signaled, elapsed) = parked.join().expect("waiter thread panicked");
        assert!(signaled, "wait_timeout must report the shutdown signal");
        assert!(
            elapsed < Duration::from_millis(500),
            "Condvar wake must be near-immediate, not bounded by the {long:?} \
             timeout; woke after {elapsed:?}"
        );
    }

    /// When NO signal arrives, `wait_timeout` must run out the timer and report
    /// `false` (a poll tick, not a shutdown) — this is what keeps the watchdog
    /// polling `supervision_lost_reason` every `PPID_POLL_INTERVAL`.
    #[test]
    fn shutdown_wait_times_out_when_not_signaled() {
        let shutdown = Shutdown::new();
        let start = Instant::now();
        let signaled = shutdown.wait_timeout(Duration::from_millis(40));
        assert!(
            !signaled,
            "an un-signaled wait must report a timeout, not a signal"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(40),
            "wait_timeout must actually wait out the poll interval"
        );
    }

    /// A signal raised BEFORE the wait must be observed immediately (the
    /// predicate is checked under the lock before parking), so a shutdown that
    /// races ahead of the watchdog loop is never missed.
    #[test]
    fn shutdown_already_signaled_returns_without_waiting() {
        let shutdown = Shutdown::new();
        shutdown.signal();
        let start = Instant::now();
        let signaled = shutdown.wait_timeout(Duration::from_secs(10));
        assert!(signaled, "a pre-set shutdown must be seen on entry");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "a pre-signaled wait must not park"
        );
    }
}
