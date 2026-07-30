//! MCP proxy mode (issue #411 / colby `mcp/proxy.ts`).
//!
//! The launcher process the MCP host actually spawns becomes a thin
//! stdio<->socket bridge to the shared daemon. Unlike a raw byte pump, this is
//! the LOCAL-HANDSHAKE proxy (colby `runLocalHandshakeProxy`):
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
//! A PPID watchdog (colby proxy.ts) forces the proxy to exit if the MCP
//! host dies without closing stdin (SIGKILL on POSIX). The proxy does NOT send a
//! client-hello yet.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use interprocess::local_socket::traits::Stream as _;
use serde_json::{Value, json};

use crate::process::{SupervisionState, current_ppid, is_process_alive, supervision_lost_reason};
use crate::session::read_daemon_hello;
use crate::transport::{Rendezvous, connect};

/// The wire protocol version the daemon advertises in its hello
/// (`session.rs` `DaemonHello.protocol`). Proxy and daemon must agree.
const EXPECTED_PROTOCOL: u64 = 1;

/// Poll cadence for the PPID watchdog (mirrors colby `DEFAULT_PPID_POLL_MS`).
const PPID_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Last-resort bound on the post-host-EOF wait for the replies the host is still
/// owed (see [`ReplyLedger`]). Never reached in normal operation — the ledger
/// settles the instant the daemon answers the last forwarded request, or the
/// instant the daemon stream ends. It exists only so a daemon that accepted the
/// connection and then stalled forever cannot wedge the proxy's teardown; it is
/// deliberately LONGER than any caller's own deadline, so a genuinely stalled
/// daemon still surfaces as that caller's failure rather than being absorbed
/// here.
const REPLY_DRAIN_BUDGET: Duration = Duration::from_secs(20);

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

    // Split into independent recv/send halves. interprocess's split hands BOTH
    // halves a refcount over the SAME kernel object (an `Arc<RawPipeStream>` on
    // windows, an `Arc` over one fd on unix), so merely DROPPING the send half
    // does not signal EOF to the daemon — the object stays open via the recv
    // half. On unix we therefore capture the WRITE-side fd before moving `send`
    // into the up pump and, once the host side is done, explicitly half-close it
    // (shutdown(SHUT_WR)), which makes the daemon's session reader hit EOF,
    // flush its last reply, and close. Windows named pipes have NO half-close,
    // so teardown there cannot depend on the daemon closing first; the reply
    // ledger below is what bounds it on every platform.
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
    // What the host is still OWED: every forwarded request id, retired only once
    // its reply has been written to the host. This is the platform-independent
    // teardown condition — see `ReplyLedger`.
    let ledger = Arc::new(ReplyLedger::default());
    // Event channel the watchdog parks on: lets teardown wake it the instant
    // shutdown is decided instead of after the remainder of a poll interval.
    let watchdog_wake = Arc::new(Shutdown::new());
    // The forwarded `initialize` id whose daemon reply must be suppressed.
    let suppressed_id: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    // Both directions write to the host; serialize them behind one lock so an
    // interleaved daemon reply can't split a local answer mid-line.
    let host_out = Arc::new(Mutex::new(host_out));

    // PPID watchdog: a SIGKILL'd host never closes stdin on POSIX, so poll the
    // host pid and flip shutdown when supervision is lost (colby proxy.ts).
    let watchdog =
        spawn_ppid_watchdog(host_ppid, Arc::clone(&watchdog_wake), Arc::clone(&shutdown));

    // daemon -> host pump (own thread): forward every daemon line to the host,
    // except the suppressed-initialize reply.
    let socket_reader = BufReader::new(recv);
    let down_suppressed = Arc::clone(&suppressed_id);
    let down_out = Arc::clone(&host_out);
    let down_ledger = Arc::clone(&ledger);
    let down = thread::spawn(move || {
        let result = pump_daemon_to_host(socket_reader, &down_out, &down_suppressed, &down_ledger);
        // The daemon stream ended: nothing further can ever arrive, so whatever is
        // still outstanding will never be answered. Settle the ledger so a
        // teardown parked on it wakes instead of waiting out the budget.
        down_ledger.abandon();
        result
    });

    // host -> daemon pump (this thread): answer initialize/tools-list locally,
    // forward the rest. Runs to completion on host_in EOF.
    let up_result =
        pump_host_to_daemon(host_in, send, &host_out, &shutdown, &suppressed_id, &ledger);

    // Host side is done, but the daemon may still owe replies to requests we
    // forwarded. Wait for the LEDGER to settle — every owed reply written, or the
    // daemon stream gone — NOT for the down pump to exit.
    //
    // On unix we first half-close the write direction: the daemon reader EOFs,
    // flushes its final reply, and closes, so the down pump reaches EOF on its
    // own and `down.join()` returns. Windows named pipes have NO half-close
    // (`half_close_write` is a documented no-op there), so the daemon's session
    // reader NEVER EOFs while our recv half keeps the pipe open — its rmcp serve
    // loop parks on the next frame, the pipe is never closed, and the down pump
    // therefore never returns. Joining it unconditionally is an UNBOUNDED wait on
    // windows; it is what made this function never return there.
    half_close_write(write_fd);
    let settled = ledger.wait_until_settled(REPLY_DRAIN_BUDGET);
    shutdown.store(true, Ordering::SeqCst);
    // Wake the watchdog at once so its join (in drop) returns promptly instead
    // of waiting out the remainder of a poll interval.
    watchdog_wake.signal();
    drop(watchdog);
    // Join only when the down pump can actually be known to have finished: it
    // exits on daemon EOF, which only the half-closing platform guarantees.
    // Elsewhere it is detached — its thread ends when the daemon closes the pipe
    // (idle-exit, sweep, or shutdown), and it holds only clones.
    if HALF_CLOSE_EOFS_DAEMON {
        let _ = down.join();
    }

    up_result?;
    anyhow::ensure!(
        settled,
        "daemon did not answer every forwarded request within {REPLY_DRAIN_BUDGET:?}"
    );
    Ok(ProxyOutcome::Proxied)
}

/// Whether [`half_close_write`] actually makes the daemon's session reader see
/// EOF. True on unix (`shutdown(SHUT_WR)` on the shared socket fd); FALSE on
/// windows, where named pipes have no half-close, so the daemon keeps its
/// session open and the daemon->host pump never reaches EOF on its own.
#[cfg(unix)]
const HALF_CLOSE_EOFS_DAEMON: bool = true;
#[cfg(not(unix))]
const HALF_CLOSE_EOFS_DAEMON: bool = false;

/// The proxy's platform-independent teardown condition: the set of forwarded
/// request ids the host is still OWED a reply for.
///
/// The proxy cannot tear down the instant the host's stdin closes — the daemon
/// may still be computing the answer to a `tools/call` already in flight, and
/// dropping it would lose the host's result. The ORIGINAL teardown waited for
/// the daemon->host pump to hit EOF, which is only reachable when the proxy can
/// half-close its write direction and make the daemon close first. That is a
/// UNIX property: windows named pipes have no half-close, so on windows nothing
/// ever ends that pump and the wait was unbounded.
///
/// A ledger replaces "wait for the peer to close" with "wait until nothing is
/// owed", which holds on every platform: an id is recorded when its request is
/// forwarded and retired when its reply is written to the host, so
/// [`wait_until_settled`](Self::wait_until_settled) returns exactly when the last
/// answer has been delivered. [`abandon`](Self::abandon) settles it when the
/// daemon stream ends (nothing more can arrive), and notifications — which have
/// no id and are never answered — are never recorded.
#[derive(Default)]
struct ReplyLedger {
    state: Mutex<LedgerState>,
    settled: Condvar,
}

#[derive(Default)]
struct LedgerState {
    outstanding: Vec<Value>,
    abandoned: bool,
}

impl ReplyLedger {
    fn record(&self, id: Value) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.outstanding.push(id);
    }

    fn retire(&self, id: &Value) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = state.outstanding.iter().position(|owed| owed == id) {
            state.outstanding.remove(at);
        }
        if state.outstanding.is_empty() {
            self.settled.notify_all();
        }
    }

    fn abandon(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.abandoned = true;
        self.settled.notify_all();
    }

    /// Park until nothing is owed (or the daemon stream ended), bounded by
    /// `budget`. `true` means settled; `false` means the budget ran out with
    /// replies still owed.
    fn wait_until_settled(&self, budget: Duration) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (state, _) = self
            .settled
            .wait_timeout_while(state, budget, |state| {
                !state.abandoned && !state.outstanding.is_empty()
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.abandoned || state.outstanding.is_empty()
    }
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
    ledger: &Arc<ReplyLedger>,
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
                //
                // PRESERVE the static full-tool-surface answer
                // (`visible_tool_definitions`), NOT the dynamic
                // required-projectPath variant. This is correct here — the daemon
                // proxy path is only entered when the project has a `.codegraph/`
                // index (the daemon only starts pinned to an indexed root), so
                // the default project always resolves and the direct rmcp path's
                // dynamic `list_tools` would return the SAME full surface. The two
                // paths therefore do not diverge; the static call is retained
                // deliberately (reviewer-signed-off) rather than by omission.
                if let Some(id) = id {
                    let tools = json!({
                        "tools": codegraph_mcp::schemas::visible_tool_definitions()
                    });
                    write_host_line(host_out, &reply(&id, tools))?;
                }
            }
            _ => {
                // Everything else (tools/call, ping, notifications, ...) is
                // forwarded verbatim to the daemon. Only a REQUEST (one carrying
                // an `id`) is ever answered, so only that owes the host a reply;
                // a notification has no id and is deliberately not recorded.
                if let Some(id) = id {
                    ledger.record(id);
                }
                forward_to_daemon(&mut daemon_send, &line)?;
            }
        }
    }
    Ok(())
}

/// daemon -> host: forward each daemon line to the host, dropping the response
/// to the suppressed-initialize id and retiring each delivered reply from
/// `ledger` — that retirement, not this pump's own EOF, is what releases the
/// teardown, so the final `tools/call` answer is delivered on every platform
/// including ones with no socket half-close. Still drains to EOF (NOT a
/// `shutdown` flag) so a daemon that keeps talking is never cut off mid-reply.
fn pump_daemon_to_host<S, W>(
    daemon_recv: S,
    host_out: &Arc<Mutex<W>>,
    suppressed_id: &Arc<Mutex<Option<Value>>>,
    ledger: &Arc<ReplyLedger>,
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

        // Suppress the daemon's reply to the forwarded initialize id. That id is
        // answered locally and never recorded as owed, so nothing is retired here.
        let mut delivered_id = None;
        if let Ok(resp) = serde_json::from_str::<Value>(&line) {
            let is_reply = resp.get("result").is_some() || resp.get("error").is_some();
            if is_reply {
                let resp_id = resp.get("id");
                let suppressed = suppressed_id
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let (Some(resp_id), Some(want)) = (resp_id, suppressed.as_ref())
                    && resp_id == want
                {
                    continue;
                }
                delivered_id = resp_id.cloned();
            }
        }

        write_host_line(host_out, &line)?;
        // Retire AFTER the host write: the teardown may proceed the instant the
        // ledger settles, so the reply must already be in the host's hands.
        if let Some(id) = delivered_id {
            ledger.retire(&id);
        }
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
    let handle = thread::spawn(move || {
        loop {
            if wake.wait_timeout(PPID_POLL_INTERVAL) {
                break;
            }
            let state = SupervisionState {
                original_ppid,
                current_ppid: current_ppid(),
                host_pid: host_ppid,
                // The proxy is a short-lived child of the real host (never setsid'd),
                // so ppid divergence DOES mean the host died — keep that signal.
                session_leader: false,
            };
            if supervision_lost_reason(&state, is_process_alive).is_some() {
                pump_shutdown.store(true, Ordering::SeqCst);
                wake.signal();
                break;
            }
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

    #[test]
    fn verify_daemon_hello_accepts_matching_build() {
        let hello = json!({
            "codegraph": env!("CARGO_PKG_VERSION"),
            "protocol": EXPECTED_PROTOCOL,
        });
        assert_eq!(verify_daemon_hello(&hello), None);
    }

    #[test]
    fn verify_daemon_hello_rejects_version_or_protocol_divergence() {
        let wrong_version = json!({ "codegraph": "0.0.0-nope", "protocol": EXPECTED_PROTOCOL });
        assert_eq!(
            verify_daemon_hello(&wrong_version),
            Some(ProxyOutcome::VersionMismatch)
        );
        let wrong_protocol = json!({
            "codegraph": env!("CARGO_PKG_VERSION"),
            "protocol": EXPECTED_PROTOCOL + 1,
        });
        assert_eq!(
            verify_daemon_hello(&wrong_protocol),
            Some(ProxyOutcome::VersionMismatch)
        );
        let missing_fields = json!({ "unrelated": true });
        assert_eq!(
            verify_daemon_hello(&missing_fields),
            Some(ProxyOutcome::VersionMismatch)
        );
    }

    #[test]
    fn reply_builds_jsonrpc_success_envelope() {
        let line = reply(&json!(7), json!({ "ok": true }));
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["jsonrpc"], json!("2.0"));
        assert_eq!(parsed["id"], json!(7));
        assert_eq!(parsed["result"]["ok"], json!(true));
    }

    #[test]
    fn write_host_line_and_forward_to_daemon_frame_and_flush() {
        let host_out = Arc::new(Mutex::new(Vec::<u8>::new()));
        write_host_line(&host_out, "hello").unwrap();
        let written = host_out.lock().unwrap().clone();
        assert_eq!(String::from_utf8(written).unwrap(), "hello\n");

        let mut daemon = Vec::<u8>::new();
        forward_to_daemon(&mut daemon, "frame").unwrap();
        assert_eq!(String::from_utf8(daemon).unwrap(), "frame\n");
    }

    #[test]
    fn pump_host_to_daemon_answers_initialize_and_tools_list_locally() {
        let host_in = std::io::Cursor::new(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n\
             \n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\"}\n"
                .as_bytes()
                .to_vec(),
        );
        let mut daemon_sink = Vec::<u8>::new();
        let host_out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let suppressed: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let ledger = Arc::new(ReplyLedger::default());

        pump_host_to_daemon(
            host_in,
            &mut daemon_sink,
            &host_out,
            &shutdown,
            &suppressed,
            &ledger,
        )
        .expect("pump runs to host_in EOF");

        let to_host = String::from_utf8(host_out.lock().unwrap().clone()).unwrap();
        assert!(to_host.contains("\"id\":1"), "initialize answered locally");
        assert!(to_host.contains("\"id\":2"), "tools/list answered locally");

        let to_daemon = String::from_utf8(daemon_sink).unwrap();
        assert!(
            to_daemon.contains("initialize"),
            "initialize forwarded to prime daemon"
        );
        assert!(to_daemon.contains("tools/call"), "other methods forwarded");
        assert!(
            !to_daemon.contains("tools/list"),
            "tools/list not forwarded"
        );

        assert_eq!(
            *suppressed.lock().unwrap(),
            Some(json!(1)),
            "the forwarded initialize id is recorded for reply suppression"
        );

        // Only the FORWARDED request whose reply must come from the daemon (id 3)
        // is owed. The locally answered initialize (id 1, suppressed) and
        // tools/list (id 2, not forwarded) are already delivered, so recording
        // them would leave the ledger permanently unsettled.
        assert!(
            !ledger.wait_until_settled(Duration::from_millis(0)),
            "the forwarded tools/call still owes the host a reply"
        );
        ledger.retire(&json!(3));
        assert!(
            ledger.wait_until_settled(Duration::from_millis(0)),
            "retiring the only owed id settles the ledger"
        );
    }

    #[test]
    fn pump_host_to_daemon_stops_when_shutdown_flagged() {
        let host_in = std::io::Cursor::new(
            "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n"
                .as_bytes()
                .to_vec(),
        );
        let mut daemon_sink = Vec::<u8>::new();
        let host_out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let shutdown = Arc::new(AtomicBool::new(true));
        let suppressed: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let ledger = Arc::new(ReplyLedger::default());

        pump_host_to_daemon(
            host_in,
            &mut daemon_sink,
            &host_out,
            &shutdown,
            &suppressed,
            &ledger,
        )
        .expect("pump exits promptly on a pre-set shutdown");
        assert!(
            daemon_sink.is_empty(),
            "no line forwarded once shutdown is set"
        );
    }

    #[test]
    fn pump_daemon_to_host_suppresses_the_recorded_initialize_reply() {
        let daemon_recv = std::io::Cursor::new(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"suppressed\":true}}\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"forwarded\":true}}\n\
             \n"
            .as_bytes()
            .to_vec(),
        );
        let host_out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let suppressed: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(Some(json!(1))));
        let ledger = Arc::new(ReplyLedger::default());
        ledger.record(json!(3));

        pump_daemon_to_host(daemon_recv, &host_out, &suppressed, &ledger).expect("drains to EOF");

        let to_host = String::from_utf8(host_out.lock().unwrap().clone()).unwrap();
        assert!(!to_host.contains("suppressed"), "id 1 reply is dropped");
        assert!(to_host.contains("forwarded"), "id 3 reply is delivered");
        assert!(
            ledger.wait_until_settled(Duration::from_millis(0)),
            "delivering the owed reply retires it from the ledger"
        );
    }

    /// The teardown wait must be released by the LEDGER, not by the daemon
    /// closing its end: a windows named pipe has no half-close, so the daemon
    /// never EOFs while the proxy's recv half holds the pipe open. Waiting on the
    /// ledger settles on the last reply DELIVERED, which happens on every
    /// platform.
    #[test]
    fn reply_ledger_settles_on_the_last_delivered_reply_not_on_peer_close() {
        let ledger = Arc::new(ReplyLedger::default());
        ledger.record(json!(1));
        ledger.record(json!("two"));
        assert!(
            !ledger.wait_until_settled(Duration::from_millis(0)),
            "two owed replies keep the ledger unsettled"
        );

        let settler = Arc::clone(&ledger);
        let parked = thread::spawn(move || {
            let start = Instant::now();
            let settled = settler.wait_until_settled(Duration::from_secs(10));
            (settled, start.elapsed())
        });

        thread::sleep(Duration::from_millis(20));
        ledger.retire(&json!(1));
        ledger.retire(&json!("two"));

        let (settled, elapsed) = parked.join().expect("waiter thread panicked");
        assert!(settled, "the ledger settles once nothing is owed");
        assert!(
            elapsed < Duration::from_secs(5),
            "settling must wake the waiter at once, not run out the budget; \
             woke after {elapsed:?}"
        );
    }

    /// A daemon stream that ends with replies still owed can never answer them,
    /// so `abandon` settles the wait immediately instead of burning the budget.
    #[test]
    fn reply_ledger_abandon_settles_an_unanswerable_wait() {
        let ledger = Arc::new(ReplyLedger::default());
        ledger.record(json!(4));
        ledger.abandon();
        let start = Instant::now();
        assert!(ledger.wait_until_settled(Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    /// Retiring an id that was never owed (a daemon reply to something the proxy
    /// did not forward, e.g. the primed `initialize`) must not underflow the
    /// ledger or falsely settle a wait that still owes another reply.
    #[test]
    fn reply_ledger_ignores_an_unrecorded_retirement() {
        let ledger = ReplyLedger::default();
        ledger.record(json!(1));
        ledger.retire(&json!(99));
        assert!(
            !ledger.wait_until_settled(Duration::from_millis(0)),
            "an unrelated retirement must not settle a ledger that still owes id 1"
        );
        ledger.retire(&json!(1));
        assert!(ledger.wait_until_settled(Duration::from_millis(0)));
    }

    /// An unsettled ledger must still give up at its budget so teardown is
    /// bounded even against a daemon that accepted the connection and stalled.
    #[test]
    fn reply_ledger_reports_an_unsettled_budget_expiry() {
        let ledger = ReplyLedger::default();
        ledger.record(json!(5));
        let start = Instant::now();
        assert!(!ledger.wait_until_settled(Duration::from_millis(40)));
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn ppid_watchdog_guard_joins_cleanly_on_wake_signal() {
        let wake = Arc::new(Shutdown::new());
        let pump_shutdown = Arc::new(AtomicBool::new(false));
        let guard = spawn_ppid_watchdog(Some(std::process::id()), Arc::clone(&wake), pump_shutdown);
        wake.signal();
        let start = Instant::now();
        drop(guard);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "guard drop must join promptly after a wake signal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_proxy_falls_back_on_version_mismatch_over_a_real_socket() {
        use crate::transport::{Rendezvous, bind, connect};
        use interprocess::local_socket::traits::Listener as _;
        use std::io::Write as _;

        let dir = std::env::temp_dir().join(format!(
            "cg-proxy-mismatch-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("daemon.sock");
        let rendezvous = Rendezvous::from_socket_path(&socket_path);
        let listener = bind(&rendezvous).expect("bind listener");

        // Readiness sync + loop-accept: the listener is non-blocking (bind sets
        // ListenerNonblockingMode::Accept), so accept() returns WouldBlock until
        // run_proxy connects; the acceptor loops over that, and the ready signal
        // lets the main thread wait for the bound socket before run_proxy's
        // one-shot connect fires — removing the accept<->connect race that flakes
        // on a loaded CI runner (production run_proxy attaches to a live daemon).
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let acceptor = thread::spawn(move || {
            let _ = ready_tx.send(());
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(mut stream) => {
                        let hello =
                            json!({ "codegraph": "0.0.0-wrong", "protocol": 1 }).to_string();
                        let _ = writeln!(stream, "{hello}");
                        let _ = stream.flush();
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("acceptor thread signals readiness before accept");

        let host_in = std::io::Cursor::new(Vec::<u8>::new());
        let host_out = Vec::<u8>::new();
        let outcome = run_proxy(&socket_path, Some(std::process::id()), host_in, host_out)
            .expect("proxy connects and reads the hello");
        assert_eq!(outcome, ProxyOutcome::VersionMismatch);

        let _ = acceptor.join();
        let _ = connect(&rendezvous);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
