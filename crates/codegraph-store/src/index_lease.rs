//! Cooperative kernel-lock capability for one resolved index namespace.
//!
//! A lease owns one locked file description behind an [`Arc`]. Cloning a lease
//! clones only that `Arc`; the file is unlocked and closed when the final owner
//! drops. Acquisition always uses the nonblocking standard-library `try_lock*`
//! calls in a bounded loop with a monotonic deadline and cancellation checks.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "test-hooks")]
use std::io::Read;

use codegraph_core::IndexPaths;
use thiserror::Error;

use crate::file_identity::{
    FileIdentity, identity_for_file, identity_for_validated_path, is_alias, is_regular,
    metadata_observation_matches, path_still_names_file,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquireCheckpoint {
    ProjectRootOpened,
    RootCreated,
    InitialMetadataValidated,
    HandleOpened,
    KernelLockAcquired,
    FinalPathCorroborated,
    InitialGitignoreLeaseValidated,
    InitialGitignoreRootOpened,
}

#[derive(Debug)]
struct LeaseInner {
    file: File,
    mode: LeaseMode,
    db_parent: PathBuf,
    lock_path: PathBuf,
}

struct PendingAcquisition {
    lock_path: PathBuf,
    mode: LeaseMode,
    db_parent: PathBuf,
    deadline: Instant,
    opened_identity: FileIdentity,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        // This Drop runs exactly once, when the final `Arc<LeaseInner>` owner is
        // gone. Unlock before File's own Drop closes the one locked description.
        // There is no recovery action available from Drop; close also releases
        // the kernel lock if this best-effort explicit unlock reports an error.
        let _ = self.file.unlock();
    }
}

/// A cloneable capability tied to one resolved database parent.
#[derive(Debug, Clone)]
pub struct IndexLease {
    inner: Arc<LeaseInner>,
}

/// Typed failures while opening or acquiring a permanent index lock.
#[derive(Debug, Error)]
pub enum IndexLeaseError {
    /// An existing namespace has no permanent lock; state must be classified
    /// before deciding whether explicit init is safe.
    #[error(
        "existing index namespace has no permanent lock file at {path}; a failed background daemon start can leave this stale shape. Run `codegraph status` from the project root to classify it; only `State: missing` is safe to recover with `codegraph init`."
    )]
    LockNotFound { path: PathBuf },
    /// The fixed permanent-lock path is a symlink or Windows reparse point.
    #[error("permanent index lock is an alias and cannot be lock authority: {path}")]
    AliasedLock { path: PathBuf },
    /// The fixed permanent-lock path exists but is not a regular file.
    #[error("permanent index lock is a {kind}, not a regular file: {path}")]
    NonRegularLock { path: PathBuf, kind: &'static str },
    /// The fixed path stopped naming the opened lock during acquisition.
    #[error("permanent index lock changed during acquisition: {path}")]
    LockChangedDuringAcquisition { path: PathBuf },
    /// Another entry won creation of the permanent lock in a newly created root.
    #[error("permanent index lock creation lost a race: {path}")]
    LockCreationConflict { path: PathBuf },
    /// Explicit namespace creation was requested for an existing root.
    #[error("cannot create an initial index lease because the namespace already exists: {path}")]
    NamespaceAlreadyExists { path: PathBuf },
    /// The current root could not be created or inspected.
    #[error("cannot create or inspect index root {path}: {source}")]
    CreateRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The permanent lock file could not be opened.
    #[error("cannot open permanent index lock {path}: {source}")]
    OpenLock {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The bounded acquisition deadline elapsed while another process held an
    /// incompatible lock.
    #[error("timed out acquiring permanent index lock {path}")]
    TimedOut { path: PathBuf },
    /// The caller cancelled bounded acquisition.
    #[error("cancelled while acquiring permanent index lock {path}")]
    Cancelled { path: PathBuf },
    /// The operating system rejected a lock operation for another reason.
    #[error("cannot acquire permanent index lock {path}: {source}")]
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Typed failures when a later writer validates an [`IndexLease`] capability.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IndexLeaseValidationError {
    /// A shared reader lease cannot authorize a writer.
    #[error("a shared index lease cannot authorize a writer")]
    SharedLease,
    /// The capability belongs to another resolved database parent.
    #[error("index lease belongs to a different database parent")]
    WrongDbParent,
    /// The permanent fixed path no longer names the exact locked handle.
    #[error("permanent index lock changed, disappeared, or became an alias: {path}")]
    PermanentLockChanged {
        /// Fixed permanent-lock path expected to name the held handle.
        path: PathBuf,
    },
}

impl IndexLease {
    /// Open and acquire the permanent lock of an existing namespace for shared
    /// reading. This API never creates a directory or file.
    pub fn acquire_shared_existing(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        Self::acquire_existing(paths, LeaseMode::Shared, deadline, cancelled)
    }

    /// Open and acquire the permanent lock of an existing namespace for
    /// exclusive writing. This API never creates a directory or file.
    pub fn acquire_exclusive_existing(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        Self::acquire_existing(paths, LeaseMode::Exclusive, deadline, cancelled)
    }

    /// Acquire the ONE outer exclusive capability of a namespace that may or may
    /// not exist yet: an existing current root must already carry its permanent
    /// lock (never repaired), while a genuinely absent root is created together
    /// with its lock. This is the single entry point every writer that owns a
    /// whole lifecycle operation uses, so no caller re-implements the
    /// existing-vs-initial decision or acquires a second, nested lease.
    pub fn acquire_or_create_exclusive(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        match std::fs::symlink_metadata(paths.current_root()) {
            Ok(_) => Self::acquire_exclusive_existing(paths, deadline, cancelled),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Self::create_exclusive(paths, deadline, cancelled)
            }
            Err(source) => Err(IndexLeaseError::CreateRoot {
                path: paths.current_root().to_path_buf(),
                source,
            }),
        }
    }

    /// Explicitly create a genuinely absent current root and its permanent lock,
    /// then acquire the initial exclusive capability.
    pub fn create_exclusive(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        Self::create_exclusive_with(paths, deadline, cancelled, |_| {})
    }

    /// Create and acquire the permanent lock inside an existing, state-less
    /// namespace selected for stale-cache replacement. Callers must classify
    /// before attempting creation, then re-classify under the genuinely held
    /// returned lease before authorizing or changing any other byte: creating the
    /// lock file is not itself possession of its kernel lock.
    pub(crate) fn create_exclusive_in_existing_root(
        paths: &IndexPaths,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        let root = paths.current_root();
        let lock_path = paths.permanent_lock();
        if cancelled() {
            return Err(IndexLeaseError::Cancelled { path: lock_path });
        }
        if Instant::now() >= deadline {
            return Err(IndexLeaseError::TimedOut { path: lock_path });
        }
        let metadata =
            std::fs::symlink_metadata(root).map_err(|source| IndexLeaseError::CreateRoot {
                path: root.to_path_buf(),
                source,
            })?;
        if !metadata.file_type().is_dir() {
            return Err(IndexLeaseError::CreateRoot {
                path: root.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "existing index root is not a directory",
                ),
            });
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| classify_create_error(&lock_path, source))?;
        let opened_identity = opened_identity(&file, &lock_path, None)?;
        #[cfg(feature = "test-hooks")]
        lease_test_checkpoint(AcquireCheckpoint::HandleOpened);
        Self::acquire_file(
            file,
            PendingAcquisition {
                lock_path,
                mode: LeaseMode::Exclusive,
                db_parent: db_parent(paths),
                deadline,
                opened_identity,
            },
            cancelled,
            |_| {},
        )
    }

    fn create_exclusive_with(
        paths: &IndexPaths,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
        mut checkpoint: impl FnMut(AcquireCheckpoint),
    ) -> Result<Self, IndexLeaseError> {
        use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};

        let root_path = paths.current_root();
        let lock_path = paths.permanent_lock();
        if cancelled() {
            return Err(IndexLeaseError::Cancelled { path: lock_path });
        }
        if Instant::now() >= deadline {
            return Err(IndexLeaseError::TimedOut { path: lock_path });
        }
        let project =
            cap_std::fs::Dir::from_std_file(open_directory_no_follow(paths.project()).map_err(
                |source| IndexLeaseError::CreateRoot {
                    path: paths.project().to_path_buf(),
                    source,
                },
            )?);
        checkpoint(AcquireCheckpoint::ProjectRootOpened);
        debug_assert_eq!(root_path.parent(), Some(paths.project()));
        let root_name = root_path
            .file_name()
            .expect("IndexPaths current root always has a file name");
        match project.symlink_metadata(root_name) {
            Ok(_) => {
                return Err(IndexLeaseError::NamespaceAlreadyExists {
                    path: root_path.to_path_buf(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(IndexLeaseError::CreateRoot {
                    path: root_path.to_path_buf(),
                    source,
                });
            }
        }
        project.create_dir(root_name).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                IndexLeaseError::NamespaceAlreadyExists {
                    path: root_path.to_path_buf(),
                }
            } else {
                IndexLeaseError::CreateRoot {
                    path: root_path.to_path_buf(),
                    source,
                }
            }
        })?;
        let root =
            project
                .open_dir_nofollow(root_name)
                .map_err(|source| IndexLeaseError::CreateRoot {
                    path: root_path.to_path_buf(),
                    source,
                })?;
        checkpoint(AcquireCheckpoint::RootCreated);
        let lock_name = lock_path
            .file_name()
            .expect("IndexPaths permanent lock always has a file name");
        let mut lock_options = cap_std::fs::OpenOptions::new();
        lock_options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let file = root
            .open_with(lock_name, &lock_options)
            .map(cap_std::fs::File::into_std)
            .map_err(|source| classify_create_error(&lock_path, source))?;
        let opened_identity = opened_identity(&file, &lock_path, None)?;
        checkpoint(AcquireCheckpoint::HandleOpened);
        let lease = Self::acquire_file(
            file,
            PendingAcquisition {
                lock_path,
                mode: LeaseMode::Exclusive,
                db_parent: db_parent(paths),
                deadline,
                opened_identity,
            },
            cancelled,
            &mut checkpoint,
        )?;
        ensure_initial_gitignore(paths, &lease, &root, &mut checkpoint);
        Ok(lease)
    }

    /// Whether this capability represents a shared reader lock.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        self.inner.mode == LeaseMode::Shared
    }

    /// Whether this capability represents an exclusive writer lock.
    #[must_use]
    pub fn is_exclusive(&self) -> bool {
        self.inner.mode == LeaseMode::Exclusive
    }

    /// Whether this capability belongs to the normalized DB parent in `paths`.
    /// The identity itself remains private.
    #[must_use]
    pub fn matches_db_parent(&self, paths: &IndexPaths) -> bool {
        self.inner.db_parent == db_parent(paths)
    }

    /// Validate this capability for a future write-capable store open.
    pub fn validate_exclusive(&self, paths: &IndexPaths) -> Result<(), IndexLeaseValidationError> {
        if self.inner.mode != LeaseMode::Exclusive {
            return Err(IndexLeaseValidationError::SharedLease);
        }
        if !self.matches_db_parent(paths) {
            return Err(IndexLeaseValidationError::WrongDbParent);
        }
        let expected_path = paths.permanent_lock();
        if self.inner.lock_path != expected_path
            || !path_still_names_file(&self.inner.lock_path, &self.inner.file).unwrap_or(false)
        {
            return Err(IndexLeaseValidationError::PermanentLockChanged {
                path: expected_path,
            });
        }
        Ok(())
    }

    fn acquire_existing(
        paths: &IndexPaths,
        mode: LeaseMode,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        Self::acquire_existing_with(paths, mode, deadline, cancelled, |_| {})
    }

    fn acquire_existing_with(
        paths: &IndexPaths,
        mode: LeaseMode,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
        mut checkpoint: impl FnMut(AcquireCheckpoint),
    ) -> Result<Self, IndexLeaseError> {
        let lock_path = paths.permanent_lock();
        if cancelled() {
            return Err(IndexLeaseError::Cancelled { path: lock_path });
        }
        if Instant::now() >= deadline {
            return Err(IndexLeaseError::TimedOut { path: lock_path });
        }
        let initial = validated_path_metadata(&lock_path)?;
        let initial_identity =
            identity_for_validated_path(&lock_path, &initial).map_err(|_| changed(&lock_path))?;
        checkpoint(AcquireCheckpoint::InitialMetadataValidated);
        // Shared readers only need a shared kernel lock. Keeping their handle
        // read-only lets status/query commands work in read-only sandboxes and
        // mounts; exclusive lifecycle operations retain write access.
        let mut options = OpenOptions::new();
        options.read(true);
        if mode == LeaseMode::Exclusive {
            options.write(true);
        }
        let file = options.open(&lock_path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                changed(&lock_path)
            } else {
                IndexLeaseError::OpenLock {
                    path: lock_path.clone(),
                    source,
                }
            }
        })?;
        let opened_identity = opened_identity(&file, &lock_path, Some(&initial))?;
        if initial_identity != opened_identity {
            return Err(changed(&lock_path));
        }
        checkpoint(AcquireCheckpoint::HandleOpened);
        Self::acquire_file(
            file,
            PendingAcquisition {
                lock_path,
                mode,
                db_parent: db_parent(paths),
                deadline,
                opened_identity,
            },
            cancelled,
            checkpoint,
        )
    }

    fn acquire_file(
        file: File,
        pending: PendingAcquisition,
        mut cancelled: impl FnMut() -> bool,
        mut checkpoint: impl FnMut(AcquireCheckpoint),
    ) -> Result<Self, IndexLeaseError> {
        let PendingAcquisition {
            lock_path,
            mode,
            db_parent,
            deadline,
            opened_identity,
        } = pending;
        loop {
            if cancelled() {
                return Err(IndexLeaseError::Cancelled { path: lock_path });
            }
            if Instant::now() >= deadline {
                return Err(IndexLeaseError::TimedOut { path: lock_path });
            }

            let attempt = match mode {
                LeaseMode::Shared => file.try_lock_shared(),
                LeaseMode::Exclusive => file.try_lock(),
            };
            match attempt {
                Ok(()) => {
                    checkpoint(AcquireCheckpoint::KernelLockAcquired);
                    if !final_path_matches(&lock_path, &file, opened_identity) {
                        // The locked handle is dropped here instead of becoming
                        // a capability, releasing the kernel lock exactly once.
                        return Err(changed(&lock_path));
                    }
                    checkpoint(AcquireCheckpoint::FinalPathCorroborated);
                    let lease = Self {
                        inner: Arc::new(LeaseInner {
                            file,
                            mode,
                            db_parent,
                            lock_path,
                        }),
                    };
                    #[cfg(feature = "test-hooks")]
                    lease_test_barrier(mode);
                    return Ok(lease);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    // Check cancellation after every observed contention, then
                    // bound the next retry by the monotonic deadline. The short
                    // park avoids a hot spin but never substitutes for try_lock
                    // as the lock authority.
                    if cancelled() {
                        return Err(IndexLeaseError::Cancelled { path: lock_path });
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(IndexLeaseError::TimedOut { path: lock_path });
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    std::thread::park_timeout(remaining.min(Duration::from_millis(5)));
                }
                Err(std::fs::TryLockError::Error(source)) => {
                    return Err(IndexLeaseError::Lock {
                        path: lock_path,
                        source,
                    });
                }
            }
        }
    }
}

/// Deterministic cross-process barriers used only by integration-test builds.
///
/// The existing mode selector writes `S` or `X` only after the kernel lock and
/// final-path corroboration both succeed. The checkpoint selector can separately
/// stop existing-root lock creation at `HandleOpened`, writing `H` before the
/// first kernel-lock attempt. Both protocols wait for one `R` release byte with
/// bounded socket timeouts. No corresponding strings or I/O path are compiled
/// into normal/release builds.
#[cfg(feature = "test-hooks")]
fn lease_test_barrier(mode: LeaseMode) {
    const MODE_ENV: &str = "CODEGRAPH_TEST_LEASE_BARRIER_MODE";

    let expected = match std::env::var(MODE_ENV) {
        Ok(value) => value,
        Err(_) => return,
    };
    let (mode_name, marker) = match mode {
        LeaseMode::Shared => ("shared", b'S'),
        LeaseMode::Exclusive => ("exclusive", b'X'),
    };
    if expected != mode_name {
        return;
    }
    lease_test_wait(marker, MODE_ENV);
}

#[cfg(feature = "test-hooks")]
fn lease_test_checkpoint(checkpoint: AcquireCheckpoint) {
    const CHECKPOINT_ENV: &str = "CODEGRAPH_TEST_LEASE_BARRIER_CHECKPOINT";

    if checkpoint != AcquireCheckpoint::HandleOpened
        || !matches!(std::env::var(CHECKPOINT_ENV), Ok(value) if value == "handle-opened")
    {
        return;
    }
    lease_test_wait(b'H', CHECKPOINT_ENV);
}

#[cfg(feature = "test-hooks")]
fn lease_test_wait(marker: u8, selector_env: &str) {
    const ADDR_ENV: &str = "CODEGRAPH_TEST_LEASE_BARRIER_ADDR";
    const WAIT: Duration = Duration::from_secs(10);

    let address = std::env::var(ADDR_ENV)
        .unwrap_or_else(|_| panic!("{selector_env} requires {ADDR_ENV}"))
        .parse()
        .unwrap_or_else(|error| panic!("invalid {ADDR_ENV}: {error}"));
    let mut stream = std::net::TcpStream::connect_timeout(&address, WAIT)
        .unwrap_or_else(|error| panic!("connect lease test barrier {address}: {error}"));
    stream
        .set_read_timeout(Some(WAIT))
        .expect("set lease test barrier read timeout");
    stream
        .set_write_timeout(Some(WAIT))
        .expect("set lease test barrier write timeout");
    stream
        .write_all(&[marker])
        .expect("acknowledge lease test barrier arrival");
    let mut release = [0_u8; 1];
    stream
        .read_exact(&mut release)
        .expect("receive lease test barrier release");
    assert_eq!(release, [b'R'], "invalid lease test barrier release byte");
}

const INITIAL_GITIGNORE_BYTES: &[u8] = b"*\n";

/// Best-effort Git hygiene for a namespace this process just created. Existing
/// roots never reach this function, so a custom/config-only root is never
/// backfilled and unrelated files cannot become hidden retroactively.
fn ensure_initial_gitignore(
    paths: &IndexPaths,
    lease: &IndexLease,
    root: &cap_std::fs::Dir,
    checkpoint: &mut impl FnMut(AcquireCheckpoint),
) {
    if let Err(error) = create_initial_gitignore(paths, lease, root, checkpoint) {
        tracing::warn!(
            path = %paths.gitignore().display(),
            %error,
            "could not create the new index root's .gitignore; the index is unaffected"
        );
    }
}

/// Create, never replace, the new root's nested ignore file. The root is opened
/// without following aliases and every child operation is relative to that
/// retained directory capability, so replacing the pathname cannot redirect a
/// later create outside the namespace.
fn create_initial_gitignore(
    paths: &IndexPaths,
    lease: &IndexLease,
    root: &cap_std::fs::Dir,
    checkpoint: &mut impl FnMut(AcquireCheckpoint),
) -> io::Result<()> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};

    lease
        .validate_exclusive(paths)
        .map_err(|error| io::Error::other(error.to_string()))?;
    checkpoint(AcquireCheckpoint::InitialGitignoreLeaseValidated);
    validate_initial_root(paths, lease, root)?;
    checkpoint(AcquireCheckpoint::InitialGitignoreRootOpened);
    let path = paths.gitignore();
    let name = path
        .file_name()
        .expect("IndexPaths gitignore always has a file name");
    match root.symlink_metadata(name) {
        Ok(metadata) if cap_metadata_is_regular(&metadata) => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing to replace a non-regular .gitignore entry",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = cap_std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut file = match root.open_with(name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = root.symlink_metadata(name)?;
            if cap_metadata_is_regular(&metadata) {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing to follow or replace a raced .gitignore alias",
            ));
        }
        Err(error) => return Err(error),
    };
    file.write_all(INITIAL_GITIGNORE_BYTES)?;
    file.flush()?;
    file.sync_all()
}

fn validate_initial_root(
    paths: &IndexPaths,
    lease: &IndexLease,
    root: &cap_std::fs::Dir,
) -> io::Result<()> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};

    let lock_path = paths.permanent_lock();
    let lock_name = lock_path
        .file_name()
        .expect("IndexPaths permanent lock always has a file name");
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let relative_lock = root.open_with(lock_name, &options)?.into_std();
    if !is_regular(&relative_lock.metadata()?) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened index root does not contain a regular permanent lock",
        ));
    }
    if identity_for_file(&relative_lock)? != identity_for_file(&lease.inner.file)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened index root does not contain the leased permanent lock",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_directory_no_follow(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    Ok(File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

#[cfg(windows)]
fn open_directory_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        // Omitting FILE_SHARE_DELETE pins the opened root while relative child
        // operations use it, matching cap-std's Windows directory contract.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || is_alias(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "index root is not a non-aliased directory",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_directory_no_follow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "safe directory-relative creation is unsupported on this platform",
    ))
}

fn cap_metadata_is_regular(metadata: &cap_std::fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    true
}

fn validated_path_metadata(lock_path: &Path) -> Result<std::fs::Metadata, IndexLeaseError> {
    let metadata = std::fs::symlink_metadata(lock_path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            IndexLeaseError::LockNotFound {
                path: lock_path.to_path_buf(),
            }
        } else {
            IndexLeaseError::OpenLock {
                path: lock_path.to_path_buf(),
                source,
            }
        }
    })?;
    if is_alias(&metadata) {
        return Err(IndexLeaseError::AliasedLock {
            path: lock_path.to_path_buf(),
        });
    }
    if !is_regular(&metadata) {
        let kind = if metadata.file_type().is_dir() {
            "directory"
        } else {
            "non-regular filesystem entry"
        };
        return Err(IndexLeaseError::NonRegularLock {
            path: lock_path.to_path_buf(),
            kind,
        });
    }
    Ok(metadata)
}

fn opened_identity(
    file: &File,
    lock_path: &Path,
    initial: Option<&std::fs::Metadata>,
) -> Result<FileIdentity, IndexLeaseError> {
    let opened = file
        .metadata()
        .map_err(|source| IndexLeaseError::OpenLock {
            path: lock_path.to_path_buf(),
            source,
        })?;
    if !is_regular(&opened)
        || initial.is_some_and(|initial| !metadata_observation_matches(initial, &opened))
    {
        return Err(changed(lock_path));
    }
    identity_for_file(file).map_err(|_| changed(lock_path))
}

fn final_path_matches(lock_path: &Path, file: &File, opened: FileIdentity) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(lock_path) else {
        return false;
    };
    if !is_regular(&metadata) {
        return false;
    }
    let Ok(path_identity) = identity_for_validated_path(lock_path, &metadata) else {
        return false;
    };
    path_identity == opened && path_still_names_file(lock_path, file).unwrap_or(false)
}

fn changed(lock_path: &Path) -> IndexLeaseError {
    IndexLeaseError::LockChangedDuringAcquisition {
        path: lock_path.to_path_buf(),
    }
}

fn classify_create_error(lock_path: &Path, source: io::Error) -> IndexLeaseError {
    if source.kind() == io::ErrorKind::AlreadyExists || std::fs::symlink_metadata(lock_path).is_ok()
    {
        IndexLeaseError::LockCreationConflict {
            path: lock_path.to_path_buf(),
        }
    } else {
        IndexLeaseError::OpenLock {
            path: lock_path.to_path_buf(),
            source,
        }
    }
}

fn db_parent(paths: &IndexPaths) -> PathBuf {
    let db = paths.current_db();
    let parent = db
        .parent()
        .expect("IndexPaths current DB always has its resolved current root as parent");
    debug_assert_eq!(parent, paths.current_root());
    parent.to_path_buf()
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempProject(PathBuf);

    impl TempProject {
        fn new(label: &str) -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "codegraph-index-lease-unit-{label}-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create lease unit-test project");
            Self(
                path.canonicalize()
                    .expect("canonical lease unit-test project"),
            )
        }

        fn paths(&self) -> IndexPaths {
            IndexPaths::resolve(&self.0, None).expect("resolve lease unit-test paths")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn deadline() -> Instant {
        Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("test deadline")
    }

    #[test]
    fn replacement_after_kernel_lock_is_rejected_and_releases_the_opened_file() {
        let project = TempProject::new("replacement-after-lock");
        let paths = project.paths();
        std::fs::create_dir(paths.current_root()).expect("create current root");
        let lock_path = paths.permanent_lock();
        let displaced = paths.current_root().join("displaced.lock");
        let replacement = paths.current_root().join("replacement.lock");
        std::fs::write(&lock_path, b"original").expect("write original lock");
        std::fs::write(&replacement, b"replacement").expect("write replacement lock");

        let mut replaced = false;
        let error = IndexLease::acquire_existing_with(
            &paths,
            LeaseMode::Exclusive,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::KernelLockAcquired && !replaced {
                    std::fs::rename(&lock_path, &displaced).expect("displace opened lock");
                    std::fs::rename(&replacement, &lock_path).expect("install replacement lock");
                    replaced = true;
                }
            },
        )
        .expect_err("replacement must invalidate the acquired handle");
        assert!(matches!(
            error,
            IndexLeaseError::LockChangedDuringAcquisition { path } if path == lock_path
        ));

        let displaced_handle = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&displaced)
            .expect("open displaced original");
        displaced_handle
            .try_lock()
            .expect("rejected authority must release its kernel lock");
        displaced_handle
            .unlock()
            .expect("unlock displaced original");

        let final_lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
            .expect("fresh contender acquires the final fixed lock");
        drop(final_lease);
    }

    #[test]
    fn replacement_after_initial_validation_is_rejected_before_authority_returns() {
        let project = TempProject::new("replacement-before-open");
        let paths = project.paths();
        std::fs::create_dir(paths.current_root()).expect("create current root");
        let lock_path = paths.permanent_lock();
        let displaced = paths.current_root().join("validated.lock");
        let replacement = paths.current_root().join("replacement.lock");
        std::fs::write(&lock_path, b"validated").expect("write validated lock");
        std::fs::write(&replacement, b"replacement").expect("write replacement lock");

        let mut replaced = false;
        let error = IndexLease::acquire_existing_with(
            &paths,
            LeaseMode::Exclusive,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::InitialMetadataValidated && !replaced {
                    std::fs::rename(&lock_path, &displaced).expect("displace validated lock");
                    std::fs::rename(&replacement, &lock_path).expect("install replacement lock");
                    replaced = true;
                }
            },
        )
        .expect_err("opened file must match the initially validated object");
        assert!(matches!(
            error,
            IndexLeaseError::LockChangedDuringAcquisition { path } if path == lock_path
        ));

        for path in [&displaced, &lock_path] {
            let handle = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .expect("open race participant");
            handle
                .try_lock()
                .expect("rejected acquisition returns no locked authority");
            handle.unlock().expect("unlock race participant");
        }
    }

    #[test]
    fn initial_creation_rejects_an_alias_that_wins_after_root_creation() {
        let project = TempProject::new("creation-alias-race");
        let paths = project.paths();
        let external = project.0.join("external.lock");
        let external_bytes = b"external-lock-must-stay-unchanged";
        std::fs::write(&external, external_bytes).expect("write external target");

        let mut installed = false;
        let error = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::RootCreated && !installed {
                    symlink(&external, paths.permanent_lock()).expect("install competing alias");
                    installed = true;
                }
            },
        )
        .expect_err("create-new must reject a competing alias");
        assert!(matches!(
            error,
            IndexLeaseError::LockCreationConflict { path } if path == paths.permanent_lock()
        ));
        assert_eq!(std::fs::read(&external).unwrap(), external_bytes);

        let external_handle = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&external)
            .expect("open external target");
        external_handle
            .try_lock()
            .expect("failed creation never locks the alias target");
        external_handle.unlock().expect("unlock external target");
    }

    #[test]
    fn initial_creation_rejects_a_regular_entry_that_wins_after_root_creation() {
        let project = TempProject::new("creation-regular-race");
        let paths = project.paths();
        let competing_bytes = b"competing-regular-lock";

        let mut installed = false;
        let error = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::RootCreated && !installed {
                    std::fs::write(paths.permanent_lock(), competing_bytes)
                        .expect("install competing regular lock");
                    installed = true;
                }
            },
        )
        .expect_err("create-new must reject a competing regular entry");
        assert!(matches!(
            error,
            IndexLeaseError::LockCreationConflict { path } if path == paths.permanent_lock()
        ));
        assert_eq!(
            std::fs::read(paths.permanent_lock()).unwrap(),
            competing_bytes
        );

        let competing_handle = OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.permanent_lock())
            .expect("open competing regular lock");
        competing_handle
            .try_lock()
            .expect("failed creation never locks the competing entry");
        competing_handle.unlock().expect("unlock competing entry");
    }

    #[test]
    fn initial_lock_creation_never_follows_a_replaced_index_root() {
        let project = TempProject::new("lock-parent-race");
        let external = TempProject::new("lock-parent-external");
        let paths = project.paths();
        let displaced = project.0.join("displaced-lock-index-root");

        let mut replaced = false;
        let error = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::RootCreated && !replaced {
                    std::fs::rename(paths.current_root(), &displaced)
                        .expect("displace the opened root before lock creation");
                    symlink(&external.0, paths.current_root())
                        .expect("replace the root with an external alias");
                    replaced = true;
                }
            },
        )
        .expect_err("the replacement path must never become lock authority");

        assert!(matches!(
            error,
            IndexLeaseError::LockChangedDuringAcquisition { path }
                if path == paths.permanent_lock()
        ));
        assert!(
            !external.0.join("index.lock").exists(),
            "initial lock creation must stay relative to the opened root"
        );
        assert!(
            displaced.join("index.lock").is_file(),
            "the no-longer-authoritative directory received the relative create"
        );
        assert!(
            !external.0.join(".gitignore").exists(),
            "failed lock corroboration must not reach Git hygiene"
        );
    }

    #[test]
    fn initial_namespace_creation_never_follows_a_replaced_project_root() {
        let project = TempProject::new("project-parent-race");
        let external = TempProject::new("project-parent-external");
        let paths = project.paths();
        let displaced = project.0.with_file_name(format!(
            "codegraph-index-lease-displaced-project-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));

        let mut replaced = false;
        let error = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::ProjectRootOpened && !replaced {
                    std::fs::rename(&project.0, &displaced)
                        .expect("displace the opened project root");
                    symlink(&external.0, &project.0)
                        .expect("replace the project path with an external alias");
                    replaced = true;
                }
            },
        )
        .expect_err("the replacement project path must never become lock authority");

        assert!(matches!(
            error,
            IndexLeaseError::LockChangedDuringAcquisition { path }
                if path == paths.permanent_lock()
        ));
        assert!(
            !external.0.join(".codegraph").exists(),
            "namespace creation must stay relative to the opened project"
        );
        assert!(
            displaced.join(".codegraph/index.lock").is_file(),
            "the relative create stays with the opened project capability"
        );

        std::fs::remove_file(&project.0).expect("remove replacement project alias");
        std::fs::rename(&displaced, &project.0).expect("restore project for cleanup");
    }

    #[test]
    fn initial_gitignore_never_follows_an_alias_raced_after_lock_validation() {
        let project = TempProject::new("gitignore-alias-race");
        let paths = project.paths();
        let external = project.0.with_file_name(format!(
            "codegraph-index-lease-external-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));

        let mut installed = false;
        let lease = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::FinalPathCorroborated && !installed {
                    symlink(&external, paths.gitignore())
                        .expect("install dangling gitignore after lock validation");
                    installed = true;
                }
            },
        )
        .expect("the index lease remains valid when optional git hygiene is refused");

        assert!(
            std::fs::symlink_metadata(paths.gitignore())
                .expect("raced alias remains")
                .file_type()
                .is_symlink()
        );
        assert!(
            !external.exists(),
            "the best-effort gitignore write must not follow the raced alias"
        );
        drop(lease);
    }

    #[test]
    fn initial_gitignore_never_follows_a_replaced_index_root() {
        let project = TempProject::new("gitignore-parent-race");
        let external = TempProject::new("gitignore-parent-external");
        let paths = project.paths();
        let displaced = project.0.join("displaced-index-root");

        let mut replaced = false;
        let lease = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::InitialGitignoreLeaseValidated && !replaced {
                    std::fs::rename(paths.current_root(), &displaced)
                        .expect("displace the validated index root");
                    symlink(&external.0, paths.current_root())
                        .expect("replace the index root with an external alias");
                    replaced = true;
                }
            },
        )
        .expect("optional Git hygiene must not invalidate the acquired lease");

        assert!(
            !external.0.join(".gitignore").exists(),
            "a replaced index root must never redirect .gitignore creation outside the project"
        );
        drop(lease);
    }

    #[test]
    fn initial_gitignore_uses_the_opened_root_after_its_path_is_replaced() {
        let project = TempProject::new("gitignore-open-root-race");
        let external = TempProject::new("gitignore-open-root-external");
        let paths = project.paths();
        let displaced = project.0.join("displaced-open-index-root");

        let mut replaced = false;
        let lease = IndexLease::create_exclusive_with(
            &paths,
            deadline(),
            || false,
            |point| {
                if point == AcquireCheckpoint::InitialGitignoreRootOpened && !replaced {
                    std::fs::rename(paths.current_root(), &displaced)
                        .expect("displace the opened index root");
                    symlink(&external.0, paths.current_root())
                        .expect("replace the opened root path with an external alias");
                    replaced = true;
                }
            },
        )
        .expect("directory-relative Git hygiene keeps the acquired lease valid");

        assert!(
            !external.0.join(".gitignore").exists(),
            "relative creation must never follow the replacement path"
        );
        assert_eq!(
            std::fs::read(displaced.join(".gitignore")).unwrap(),
            INITIAL_GITIGNORE_BYTES,
            "the create remains bound to the directory capability that was opened"
        );
        drop(lease);
    }

    #[test]
    fn lockless_root_recovery_lock_not_found_is_actionable() {
        let path = PathBuf::from("/tmp/project/.codegraph/index.lock");
        let error = IndexLeaseError::LockNotFound { path: path.clone() };
        assert_eq!(
            error.to_string(),
            format!(
                "existing index namespace has no permanent lock file at {}; a failed background daemon start can leave this stale shape. Run `codegraph status` from the project root to classify it; only `State: missing` is safe to recover with `codegraph init`.",
                path.display()
            )
        );
    }
}
