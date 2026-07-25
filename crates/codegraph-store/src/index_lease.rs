//! Cooperative kernel-lock capability for one resolved v2 index namespace.
//!
//! A lease owns one locked file description behind an [`Arc`]. Cloning a lease
//! clones only that `Arc`; the file is unlocked and closed when the final owner
//! drops. Acquisition always uses the nonblocking standard-library `try_lock*`
//! calls in a bounded loop with a monotonic deadline and cancellation checks.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    RootCreated,
    InitialMetadataValidated,
    HandleOpened,
    KernelLockAcquired,
    FinalPathCorroborated,
}

#[derive(Debug)]
struct LeaseInner {
    file: File,
    mode: LeaseMode,
    db_parent: PathBuf,
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

/// A cloneable capability tied to one resolved v2 database parent.
#[derive(Debug, Clone)]
pub struct IndexLease {
    inner: Arc<LeaseInner>,
}

/// Typed failures while opening or acquiring a permanent index lock.
#[derive(Debug, Error)]
pub enum IndexLeaseError {
    /// An existing namespace has no permanent lock.
    #[error("permanent index lock does not exist: {path}")]
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
    /// The capability belongs to another resolved v2 database parent.
    #[error("index lease belongs to a different v2 database parent")]
    WrongDbParent,
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

    /// Explicitly create a genuinely absent current root and its permanent lock,
    /// then acquire the initial exclusive capability.
    pub fn create_exclusive(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        Self::create_exclusive_with(paths, deadline, cancelled, |_| {})
    }

    fn create_exclusive_with(
        paths: &IndexPaths,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
        mut checkpoint: impl FnMut(AcquireCheckpoint),
    ) -> Result<Self, IndexLeaseError> {
        let root = paths.current_root();
        let lock_path = paths.permanent_lock();
        if cancelled() {
            return Err(IndexLeaseError::Cancelled { path: lock_path });
        }
        if Instant::now() >= deadline {
            return Err(IndexLeaseError::TimedOut { path: lock_path });
        }
        match std::fs::symlink_metadata(root) {
            Ok(_) => {
                return Err(IndexLeaseError::NamespaceAlreadyExists {
                    path: root.to_path_buf(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(IndexLeaseError::CreateRoot {
                    path: root.to_path_buf(),
                    source,
                });
            }
        }
        let parent = root
            .parent()
            .expect("IndexPaths current root always has a parent");
        std::fs::create_dir_all(parent).map_err(|source| IndexLeaseError::CreateRoot {
            path: parent.to_path_buf(),
            source,
        })?;
        std::fs::create_dir(root).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                IndexLeaseError::NamespaceAlreadyExists {
                    path: root.to_path_buf(),
                }
            } else {
                IndexLeaseError::CreateRoot {
                    path: root.to_path_buf(),
                    source,
                }
            }
        })?;
        checkpoint(AcquireCheckpoint::RootCreated);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| classify_create_error(&lock_path, source))?;
        let opened_identity = opened_identity(&file, &lock_path, None)?;
        checkpoint(AcquireCheckpoint::HandleOpened);
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
            checkpoint,
        )
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

    /// Whether this capability belongs to the normalized v2 DB parent in
    /// `paths`. The identity itself remains private.
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
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| {
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
                    return Ok(Self {
                        inner: Arc::new(LeaseInner {
                            file,
                            mode,
                            db_parent,
                        }),
                    });
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
}
