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

use interprocess::local_socket::traits::{Listener as _, Stream as _};
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use interprocess::local_socket::{ListenerNonblockingMode, ListenerOptions};

pub use interprocess::local_socket::{Listener, Stream};

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
