//! Phase D — serve a daemon/CLI session through the rmcp [`CodeGraphHandler`]
//! on a blocking OS thread, preserving the hand-rolled server's reap contract.
//!
//! ## Why a blocking-thread + duplex-pump bridge (Decision B5, 12)
//!
//! The daemon runs each connection's session on a per-connection blocking
//! `std::thread`; its EOF/socket-half-close semantics (`read` → 0 → thread
//! returns → `SessionGuard` drop → reap) are load-bearing for dead-client
//! reaping. rmcp owns an ASYNC read loop, so this entry runs rmcp's async serve
//! on a `tokio` runtime via `block_on` INSIDE that same blocking thread.
//!
//! ## The transport MUST be genuinely async (the Kiro/Zed hang fix)
//!
//! The obvious bridge — a `tokio::io::AsyncRead` whose `poll_read` calls the
//! BLOCKING `std::io::read` and always returns `Poll::Ready` — DEADLOCKS a real
//! client. rmcp's `serve_inner` polls `transport.receive()` (an
//! `AsyncBufReadExt::read_until`) inside a `tokio::select!`. A `poll_read` that
//! blocks the executor thread inside the read syscall never yields back to that
//! `select!`, so the loop can never take its `Event::ToSink` branch to FLUSH a
//! response a handler already produced. `initialize` (handled INLINE before the
//! serve loop) still completes, but the first `tools/list`/`tools/call` that
//! arrives after a pause hangs forever (Kiro "Elapsed 2h", Zed "request
//! timeout"). Adding runtime worker threads does NOT help: the stuck future is
//! the single serve-loop `select!`, not a starved sibling task.
//!
//! The fix: pump the blocking socket halves on dedicated OS threads and hand
//! rmcp a `tokio::io::DuplexStream` — a genuinely async pipe whose
//! `poll_read`/`poll_write` return `Poll::Pending` + register a waker, so the
//! serve-loop `select!` stays cooperative and flushes responses promptly. Each
//! blocking half is bridged to the async side by a `tokio::sync::mpsc` channel:
//! the reader thread does blocking `read` → sends chunks to an async forwarder
//! that writes them into the pipe; an async drainer reads the pipe and sends
//! chunks to the writer thread's blocking `write`. No nested `block_on`.
//!
//! The reap contract is preserved end-to-end: a socket half/full-close makes the
//! blocking reader's `read` return `Ok(0)`; the reader thread drops its channel
//! sender, the forwarder ends and drops its duplex write half, rmcp sees EOF on
//! `receive()`, ends its serve loop, ends `block_on`, and the session thread
//! returns — exactly as `McpServer::run` did. The engine work additionally runs
//! on `spawn_blocking` inside the handler.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::rmcp_handler::CodeGraphHandler;

/// Buffer size for the in-process duplex pipe bridging the blocking socket
/// halves to rmcp's async transport. 64 KiB comfortably holds a large tool
/// response chunk (the `explore` result is ~10 KiB) without excessive copying.
const DUPLEX_BUF_BYTES: usize = 64 * 1024;

/// Read chunk size for the blocking socket pumps. Bytes are forwarded verbatim
/// (rmcp does its own newline framing), so chunk size is transparent.
const READ_CHUNK_BYTES: usize = 16 * 1024;

/// Bounded backpressure for the reader→forwarder channel: a few chunks in flight
/// decouples the blocking reader thread from the async pump without unbounded
/// buffering.
const PUMP_CHANNEL_DEPTH: usize = 16;

/// Serve one session's `reader`/`writer` (blocking `std::io` socket halves)
/// through the rmcp [`CodeGraphHandler`], blocking until the client disconnects
/// (socket close → blocking reader EOF → duplex EOF → rmcp serve ends).
///
/// The handler runs in `no_roots`/pinned mode against `project_root`: the daemon
/// is always launched pinned to a resolved project, and adoption is a bare-serve
/// (Zed-local) concern that never flows through the daemon session.
pub fn serve_session_rmcp<R, W>(reader: R, writer: W, project_root: PathBuf) -> anyhow::Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    use rmcp::ServiceExt;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        // rmcp drives THIS end of the pipe (async); dedicated OS threads drive
        // the blocking socket halves and are bridged to the pipe via channels.
        let (rmcp_side, pump_side) = tokio::io::duplex(DUPLEX_BUF_BYTES);
        let (mut pump_read, mut pump_write) = tokio::io::split(pump_side);

        // host→rmcp. Blocking reader thread → mpsc → async forwarder → pipe.
        let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(PUMP_CHANNEL_DEPTH);
        let reader_thread = std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; READ_CHUNK_BYTES];
            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) | Err(_) => break, // socket EOF/close or error → done
                    Ok(n) => {
                        // A closed receiver (rmcp gone) ends the pump.
                        if in_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            // Drop `in_tx` → forwarder sees channel close → drops pipe write half
            // → rmcp `receive()` hits EOF (the reap trigger).
        });
        let forwarder = tokio::spawn(async move {
            while let Some(chunk) = in_rx.recv().await {
                if pump_write.write_all(&chunk).await.is_err() || pump_write.flush().await.is_err()
                {
                    break;
                }
            }
            // Explicitly shut down the write direction so the duplex signals EOF
            // to `rmcp_side` even though the split read half (`pump_read`) keeps
            // the underlying `DuplexStream` alive. A plain drop of one split half
            // does NOT close the stream; `shutdown()` does.
            let _ = pump_write.shutdown().await;
        });

        // rmcp→host. Async drainer reads pipe → mpsc → blocking writer thread.
        let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let writer_thread = std::thread::spawn(move || {
            let mut writer = writer;
            while let Ok(chunk) = out_rx.recv() {
                if writer.write_all(&chunk).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });
        let drainer = tokio::spawn(async move {
            let mut buf = [0u8; READ_CHUNK_BYTES];
            loop {
                match pump_read.read(&mut buf).await {
                    Ok(0) | Err(_) => break, // pipe closed (rmcp serve ended)
                    Ok(n) => {
                        if out_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            // Drop `out_tx` → writer thread's `recv` errors → thread exits.
        });

        let handler = CodeGraphHandler::new(Some(project_root));
        let serve_result = async {
            let running = handler
                .serve(rmcp_side)
                .await
                .map_err(|e| anyhow::anyhow!("rmcp daemon session serve failed: {e}"))?;
            running
                .waiting()
                .await
                .map_err(|e| anyhow::anyhow!("rmcp daemon session join failed: {e}"))?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        // Teardown ORDER guarantees rmcp's final response reaches the writer
        // BEFORE we return, even when input EOFs the instant after the request
        // (the `session_buffer` in-memory-Cursor race): (1) `drainer.await` loops
        // until pipe EOF, which only occurs AFTER rmcp closed its write half, so
        // it has forwarded EVERY response byte into `out_tx`, then drops `out_tx`;
        // (2) JOIN `writer_thread` so it drains `out_rx` and writes+flushes all
        // queued chunks before returning — the prior `let _ = writer_thread;` let
        // `block_on` return (dropping the runtime) with chunks still queued,
        // losing the response. The reader thread exits on its own input EOF (or is
        // released by the socket close on the reap path), so it needs no join.
        let _ = forwarder.await;
        let _ = drainer.await;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = writer_thread.join();
        })
        .await;
        let _ = reader_thread;
        serve_result
    })
}
