//! Phase D — serve a daemon/CLI session through the rmcp [`CodeGraphHandler`]
//! on a blocking OS thread, preserving the hand-rolled server's reap contract.
//!
//! ## Why a blocking-thread + current-thread-runtime bridge (Decision B5, 12)
//!
//! The daemon runs each connection's session on a per-connection blocking
//! `std::thread`; its EOF/socket-half-close semantics (`read` → 0 → thread
//! returns → `SessionGuard` drop → reap) are load-bearing for dead-client
//! reaping. rmcp owns an ASYNC read loop, so this entry runs rmcp's async stdio
//! serve on a `tokio` CURRENT-THREAD runtime via `block_on` INSIDE that same
//! blocking thread. "Thread blocks until EOF, then returns" is preserved: a
//! socket half/full-close surfaces to the blocking bridge reader as `read → 0`
//! (stream-end), which ends rmcp's serve loop, ends `block_on`, and returns the
//! thread — exactly as `McpServer::run` did.
//!
//! The engine work still runs on `spawn_blocking` inside the handler; on a
//! current-thread runtime those closures run on the blocking pool the runtime
//! spawns, so the single session thread is never starved.
#![cfg(feature = "rmcp")]

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::rmcp_handler::CodeGraphHandler;

/// Adapt a BLOCKING `std::io` reader/writer pair into a `tokio` async transport
/// for rmcp. Because the session thread does nothing but drive this one
/// connection, a blocking syscall inside `poll_*` is sound: the thread is
/// dedicated, and `block_on` on a current-thread runtime has no other task to
/// starve while the syscall parks. A socket half/full-close makes the blocking
/// `read` return `Ok(0)`, which the bridge surfaces as async stream-end — the
/// EOF that ends rmcp's serve loop (the reap contract).
struct BlockingBridge<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> AsyncRead for BlockingBridge<R, W>
where
    R: BufRead + Unpin,
    W: Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let unfilled = buf.initialize_unfilled();
        match std::io::Read::read(&mut self.reader, unfilled) {
            Ok(n) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl<R, W> AsyncWrite for BlockingBridge<R, W>
where
    R: Unpin,
    W: Write + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(self.writer.write(buf))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(self.writer.flush())
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(self.writer.flush())
    }
}

/// Serve one session's `reader`/`writer` (blocking `std::io` socket halves)
/// through the rmcp [`CodeGraphHandler`] on a current-thread runtime, blocking
/// until the client disconnects (socket close → bridge EOF → rmcp serve ends).
///
/// The handler runs in `no_roots`/pinned mode against `project_root`: the daemon
/// is always launched pinned to a resolved project, and adoption is a bare-serve
/// (Zed-local) concern that never flows through the daemon session.
pub fn serve_session_rmcp<R, W>(reader: R, writer: W, project_root: PathBuf) -> anyhow::Result<()>
where
    R: BufRead + Send + 'static + Unpin,
    W: Write + Send + 'static + Unpin,
{
    use rmcp::ServiceExt;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let handler = CodeGraphHandler::new(Some(project_root));
        let transport = BlockingBridge { reader, writer };
        let running = handler
            .serve(transport)
            .await
            .map_err(|e| anyhow::anyhow!("rmcp daemon session serve failed: {e}"))?;
        running
            .waiting()
            .await
            .map_err(|e| anyhow::anyhow!("rmcp daemon session join failed: {e}"))?;
        Ok::<(), anyhow::Error>(())
    })
}
