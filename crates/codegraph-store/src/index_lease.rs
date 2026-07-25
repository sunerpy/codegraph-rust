//! Cooperative kernel-lock capability for one resolved v2 index namespace.
//!
//! A lease owns one locked file description behind an [`Arc`]. Cloning a lease
//! clones only that `Arc`; the file is unlocked and closed when the final owner
//! drops. Acquisition always uses the nonblocking standard-library `try_lock*`
//! calls in a bounded loop with a monotonic deadline and cancellation checks.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
struct LeaseInner {
    file: File,
    mode: LeaseMode,
    db_parent: PathBuf,
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
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| IndexLeaseError::OpenLock {
                path: lock_path.clone(),
                source,
            })?;
        Self::acquire_file(
            file,
            lock_path,
            LeaseMode::Exclusive,
            db_parent(paths),
            deadline,
            cancelled,
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
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
        let lock_path = paths.permanent_lock();
        if cancelled() {
            return Err(IndexLeaseError::Cancelled { path: lock_path });
        }
        if Instant::now() >= deadline {
            return Err(IndexLeaseError::TimedOut { path: lock_path });
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    IndexLeaseError::LockNotFound {
                        path: lock_path.clone(),
                    }
                } else {
                    IndexLeaseError::OpenLock {
                        path: lock_path.clone(),
                        source,
                    }
                }
            })?;
        Self::acquire_file(file, lock_path, mode, db_parent(paths), deadline, cancelled)
    }

    fn acquire_file(
        file: File,
        lock_path: PathBuf,
        mode: LeaseMode,
        db_parent: PathBuf,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, IndexLeaseError> {
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

fn db_parent(paths: &IndexPaths) -> PathBuf {
    let db = paths.current_db();
    let parent = db
        .parent()
        .expect("IndexPaths current DB always has its resolved current root as parent");
    debug_assert_eq!(parent, paths.current_root());
    parent.to_path_buf()
}
