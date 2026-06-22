//! Cross-platform IPC transport for the daemon, backed by `interprocess`
//! local sockets (Unix-domain socket on unix, named pipe on windows).
//!
//! The Unix arm is fully wired here; the windows pipe-name field is reserved
//! as a `#[cfg(windows)]` seam so a later commit only adds the windows arm.

use std::io;
use std::path::{Path, PathBuf};

use interprocess::local_socket::traits::{Listener as _, Stream as _};
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
};

pub use interprocess::local_socket::{Listener, Stream};

/// Resolved rendezvous address for a project daemon. On unix it is the
/// `.sock` filesystem path; the windows pipe-name field is a future seam.
#[derive(Clone, Debug)]
pub struct Rendezvous {
    pub socket_path: PathBuf,
    #[cfg(windows)]
    pub pipe_name: String,
}

impl Rendezvous {
    pub fn from_socket_path(socket_path: impl Into<PathBuf>) -> Self {
        let socket_path = socket_path.into();
        Self {
            #[cfg(windows)]
            pipe_name: String::new(),
            socket_path,
        }
    }

    #[cfg(unix)]
    fn name(&self) -> io::Result<interprocess::local_socket::Name<'_>> {
        self.socket_path.as_os_str().to_fs_name::<GenericFilePath>()
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
