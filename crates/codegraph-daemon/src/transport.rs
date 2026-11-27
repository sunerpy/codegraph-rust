//! Cross-platform IPC transport for the daemon, backed by `interprocess`
//! local sockets (Unix-domain socket on unix, named pipe on windows).
//!
//! Both arms are wired: unix binds a `.sock` filesystem path via
//! `GenericFilePath`; windows binds a bare `codegraph-<hash16>` namespaced
//! name via `GenericNamespaced` (interprocess prepends `\\.\pipe\` itself).

use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[cfg(all(test, unix))]
use interprocess::local_socket::ListenerNonblockingMode;
use interprocess::local_socket::ListenerOptions;
#[cfg(all(test, unix))]
use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::traits::Stream as _;
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(all(test, unix))]
pub use interprocess::local_socket::Listener;
pub use interprocess::local_socket::Stream;

/// Async (tokio) local-socket types for the daemon accept loop. `AsyncListener`
/// yields `AsyncStream`s, which a session reads and writes through [`Shared`].
pub use interprocess::local_socket::tokio::{Listener as AsyncListener, Stream as AsyncStream};

/// One local-socket stream shared by its reader and its writer.
///
/// interprocess 2.4.7 deprecated `split` in favour of this shape: `&Stream`
/// implements the I/O traits in both directions (`Read`/`Write` for the sync
/// stream, `AsyncRead`/`AsyncWrite` for the tokio one), so clones of one `Arc`
/// can read and write at the same time. The socket closes when the last clone
/// drops, and dropping one clone never half-closes it; a caller that needs a
/// half-close issues it explicitly through a live clone.
#[derive(Debug)]
pub(crate) struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self(Arc::new(inner))
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> io::Read for Shared<T>
where
    for<'a> &'a T: io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl<T> io::Write for Shared<T>
where
    for<'a> &'a T: io::Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

impl<T> AsyncRead for Shared<T>
where
    for<'a> &'a T: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut inner: &T = &self.get_mut().0;
        Pin::new(&mut inner).poll_read(cx, buf)
    }
}

impl<T> AsyncWrite for Shared<T>
where
    for<'a> &'a T: AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut inner: &T = &self.get_mut().0;
        Pin::new(&mut inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner: &T = &self.get_mut().0;
        Pin::new(&mut inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner: &T = &self.get_mut().0;
        Pin::new(&mut inner).poll_shutdown(cx)
    }
}

/// Half-close the write direction of a sync local-socket stream: the peer's
/// reader sees EOF while this side can still read. Unix only; named pipes have
/// no half-close.
#[cfg(unix)]
pub(crate) fn shutdown_write(stream: &Stream) -> io::Result<()> {
    // `UdSocket` is the only variant on unix, so this binding is irrefutable; a
    // new variant fails to compile here instead of silently skipping the shutdown.
    let Stream::UdSocket(uds) = stream;
    rustix::net::shutdown(uds, rustix::net::Shutdown::Write).map_err(io::Error::from)
}

/// Resolved rendezvous address for a project daemon. On unix `socket_path` is
/// the `.sock` filesystem path; on windows it holds the BARE namespaced name
/// `codegraph-<hash16>` (no `\\.\pipe\` prefix — `GenericNamespaced` adds it).
#[derive(Clone, Debug)]
pub struct Rendezvous {
    pub socket_path: PathBuf,
}

impl Rendezvous {
    pub fn from_socket_path(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    #[cfg(unix)]
    fn name(&self) -> io::Result<interprocess::local_socket::Name<'_>> {
        self.socket_path.as_os_str().to_fs_name::<GenericFilePath>()
    }

    // Windows: convert the stored BARE name (`codegraph-<hash16>`) to a
    // namespaced name. interprocess prepends `\\.\pipe\`, so storing the bare
    // name avoids double-prefixing (plan decision #8/#9).
    #[cfg(windows)]
    fn name(&self) -> io::Result<interprocess::local_socket::Name<'_>> {
        let bare = self.socket_path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "daemon pipe name is not valid UTF-8",
            )
        })?;
        bare.to_ns_name::<GenericNamespaced>()
    }

    #[cfg(unix)]
    pub fn cleanup_path(&self) -> Option<&Path> {
        Some(self.socket_path.as_path())
    }
}

#[cfg(all(test, unix))]
pub fn bind(rendezvous: &Rendezvous) -> io::Result<Listener> {
    let listener = ListenerOptions::new()
        .name(rendezvous.name()?)
        .create_sync()?;
    listener.set_nonblocking(ListenerNonblockingMode::Accept)?;
    Ok(listener)
}

pub fn connect(rendezvous: &Rendezvous) -> io::Result<Stream> {
    Stream::connect(rendezvous.name()?)
}

/// Bind the async (tokio) daemon listener at `rendezvous`. Mirrors [`bind`] but
/// produces an [`AsyncListener`] whose `accept().await` yields async streams for
/// the tokio accept loop. Must be called inside a tokio runtime context.
pub fn bind_tokio(rendezvous: &Rendezvous) -> io::Result<AsyncListener> {
    ListenerOptions::new()
        .name(rendezvous.name()?)
        .create_tokio()
}
