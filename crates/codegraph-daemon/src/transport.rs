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

#[cfg(test)]
use interprocess::local_socket::ListenerNonblockingMode;
use interprocess::local_socket::ListenerOptions;
#[cfg(test)]
use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::traits::Stream as _;
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};

#[cfg(test)]
pub use interprocess::local_socket::Listener;
pub use interprocess::local_socket::{SendHalf, Stream};

/// Async (tokio) local-socket types for the daemon accept loop. `AsyncListener`
/// yields `AsyncStream`s whose split halves are `AsyncRead`/`AsyncWrite`, and on
/// unix expose `AsFd` for the force-close reap handle.
pub use interprocess::local_socket::tokio::{
    Listener as AsyncListener, RecvHalf as AsyncRecvHalf, Stream as AsyncStream,
};

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

#[cfg(test)]
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
