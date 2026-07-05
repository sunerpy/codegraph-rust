//! Serve a daemon/CLI session through the rmcp [`CodeGraphHandler`].
//!
//! ## Two entry points
//!
//! - [`serve_session_rmcp_async`] is the STANDARD path: it takes a genuinely
//!   async `AsyncRead + AsyncWrite` transport (the daemon's tokio local-socket
//!   halves) and hands it straight to rmcp's `handler.serve(transport).await` —
//!   exactly like [`crate::rmcp_handler::serve_stdio_rmcp`] does with
//!   `rmcp::transport::stdio()`. No bridge, no duplex-pump: rmcp owns the async
//!   read loop over a truly async socket, so its serve-loop `select!` stays
//!   cooperative and flushes responses promptly (the Kiro/Zed hang cannot recur
//!   here — there is no blocking `poll_read` to freeze the executor).
//!
//! - [`serve_session_rmcp`] is the BLOCKING-halves adapter retained for the two
//!   callers that still hand in `std::io` halves: the CLI direct-serve path
//!   (adopted-project stdio) and the in-memory `run_session_recv` used by the
//!   `session_buffer` tests. It bridges the blocking halves onto the async serve
//!   via dedicated pump threads + a `tokio::io::duplex`, then drives
//!   [`serve_session_rmcp_async`]. The reap contract for THIS path is a socket
//!   close → blocking reader EOF → duplex EOF → rmcp serve ends. The daemon no
//!   longer uses this path; the daemon's own force-close reap is fd-based.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::rmcp_handler::CodeGraphHandler;

/// Buffer size for the in-process duplex pipe bridging blocking socket halves to
/// rmcp's async transport (blocking-adapter path only). 64 KiB comfortably holds
/// a large tool response chunk without excessive copying.
const DUPLEX_BUF_BYTES: usize = 64 * 1024;

/// Read chunk size for the blocking socket pumps (blocking-adapter path only).
const READ_CHUNK_BYTES: usize = 16 * 1024;

/// Bounded backpressure for the reader→forwarder channel (blocking-adapter path
/// only): a few chunks in flight decouples the blocking reader thread from the
/// async pump without unbounded buffering.
const PUMP_CHANNEL_DEPTH: usize = 16;

/// Serve one session over a genuinely-async `transport` (an `AsyncRead +
/// AsyncWrite`, e.g. the daemon's tokio local-socket recv/send halves joined
/// into a duplex) through the rmcp [`CodeGraphHandler`]. Awaits until the client
/// disconnects (transport EOF) or the transport is force-closed.
///
/// The handler runs pinned to `project_root` (`no_roots` mode): the daemon is
/// always launched pinned to a resolved project.
pub async fn serve_session_rmcp_async<T>(transport: T, project_root: PathBuf) -> anyhow::Result<()>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    use rmcp::ServiceExt;

    let handler = CodeGraphHandler::new(Some(project_root));
    let running = handler
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("rmcp daemon session serve failed: {e}"))?;
    running
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("rmcp daemon session join failed: {e}"))?;
    Ok(())
}

/// Serve one session's blocking `reader`/`writer` (`std::io` socket/stdio halves)
/// through the rmcp [`CodeGraphHandler`], blocking until the client disconnects
/// (socket/stdin close → blocking reader EOF → duplex EOF → rmcp serve ends).
///
/// This adapter exists only for callers that cannot supply async halves: the CLI
/// direct-serve path and the in-memory `run_session_recv` test seam. It bridges
/// the blocking halves onto [`serve_session_rmcp_async`] via dedicated pump
/// threads and a `tokio::io::duplex`, so rmcp still drives a genuinely-async
/// transport (the blocking `poll_read` deadlock is impossible on the rmcp side).
pub fn serve_session_rmcp<R, W>(reader: R, writer: W, project_root: PathBuf) -> anyhow::Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let (rmcp_side, pump_side) = tokio::io::duplex(DUPLEX_BUF_BYTES);
        let (mut pump_read, mut pump_write) = tokio::io::split(pump_side);

        let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(PUMP_CHANNEL_DEPTH);
        let reader_thread = std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; READ_CHUNK_BYTES];
            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if in_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let forwarder = tokio::spawn(async move {
            while let Some(chunk) = in_rx.recv().await {
                if pump_write.write_all(&chunk).await.is_err() || pump_write.flush().await.is_err()
                {
                    break;
                }
            }
            let _ = pump_write.shutdown().await;
        });

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
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let serve_result = serve_session_rmcp_async(rmcp_side, project_root).await;

        // Teardown ORDER guarantees rmcp's final response reaches the writer
        // before we return: drain the pipe to EOF (only after rmcp closed its
        // write half), then JOIN the writer thread so every queued chunk is
        // written+flushed before the runtime drops.
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
