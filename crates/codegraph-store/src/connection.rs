use std::path::{Path, PathBuf};
use std::time::Instant;

use codegraph_core::IndexPaths;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::migrations;
use crate::{
    CURRENT_EXTRACTION_VERSION, EXTRACTION_VERSION_KEY, ExtractionStatus, IndexLease,
    IndexLeaseError, IndexLeaseValidationError, classify,
};

/// Why a `Current` state slot could not be corroborated against the SQLite
/// extraction stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractionStampIssue {
    /// The store-owned metadata key is absent.
    Missing,
    /// The stored value is not a canonical decimal `u64`.
    Malformed { found: String },
    /// The stored decimal version differs from this binary's version.
    Mismatch { expected: u64, found: u64 },
}

impl std::fmt::Display for ExtractionStampIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(
                formatter,
                "metadata key {EXTRACTION_VERSION_KEY:?} is missing"
            ),
            Self::Malformed { found } => write!(
                formatter,
                "metadata key {EXTRACTION_VERSION_KEY:?} is not a decimal extraction version: {found:?}"
            ),
            Self::Mismatch { expected, found } => write!(
                formatter,
                "metadata key {EXTRACTION_VERSION_KEY:?} records extraction version {found}, expected {expected}"
            ),
        }
    }
}

/// Explicit authorization requested from [`Store::open_for_write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreWritePurpose {
    /// Mutate an already-current, corroborated database.
    CurrentMutation,
    /// Retain exclusive authority for a later destructive full rebuild or
    /// explicit init recovery. This slice does not perform that rebuild.
    FullRebuild,
    /// Retain exclusive authority for a later interrupted-uninit continuation.
    UninitContinuation,
}

impl std::fmt::Display for StoreWritePurpose {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CurrentMutation => "current mutation",
            Self::FullRebuild => "full rebuild",
            Self::UninitContinuation => "uninit continuation",
        })
    }
}

/// Opaque exclusive capability returned when state requires a later lifecycle
/// operation rather than an immediate SQLite open.
#[derive(Debug)]
pub struct StoreWriteAuthorization {
    pub(crate) status: ExtractionStatus,
    pub(crate) purpose: StoreWritePurpose,
    pub(crate) lease: IndexLease,
}

impl StoreWriteAuthorization {
    /// The under-lease state that authorized this lifecycle operation.
    #[must_use]
    pub fn status(&self) -> &ExtractionStatus {
        &self.status
    }

    /// The narrow lifecycle purpose this capability authorizes.
    #[must_use]
    pub fn purpose(&self) -> StoreWritePurpose {
        self.purpose
    }

    /// Whether the opaque authorization still owns an exclusive lease.
    #[must_use]
    pub fn retains_exclusive_lease(&self) -> bool {
        self.lease.is_exclusive()
    }
}

/// Result of a state-gated writer open.
#[derive(Debug)]
pub enum StoreWriteOpen {
    /// A corroborated Current database is open for ordinary mutation.
    Current(Box<Store>),
    /// SQLite was not opened; a later full-rebuild API must consume this opaque
    /// authorization while its exclusive lease remains alive.
    FullRebuildRequired(StoreWriteAuthorization),
    /// SQLite was not opened; a later uninit-continuation API must consume this
    /// opaque authorization while its exclusive lease remains alive.
    UninitContinuation(StoreWriteAuthorization),
}

/// Typed status probe returned by [`Store::open_for_status`].
#[derive(Debug)]
pub struct StoreStatusOpen {
    /// Stable state observed under a shared lease. `None` means an exclusive
    /// holder prevented a stable observation before the bounded deadline.
    pub status: Option<ExtractionStatus>,
    /// `true` only when shared acquisition timed out behind a writer.
    pub rebuilding: bool,
    store: Option<Store>,
}

impl StoreStatusOpen {
    /// The retained read-only Store used to corroborate a Current state.
    #[must_use]
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// Consume this probe and return its retained read-only Store, if Current.
    #[must_use]
    pub fn into_store(self) -> Option<Store> {
        self.store
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open SQLite database {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to configure SQLite pragmas for {path}: {source}")]
    Configure {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to initialize or migrate SQLite schema for {path}: {source}")]
    Migrate {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error(transparent)]
    Lease(#[from] IndexLeaseError),
    #[error(transparent)]
    LeaseValidation(#[from] IndexLeaseValidationError),
    #[error("state-gated Store open rejected index state {status}")]
    StateRejected { status: ExtractionStatus },
    #[error("{purpose} is not authorized for index state {status}")]
    WritePurposeRejected {
        purpose: StoreWritePurpose,
        status: ExtractionStatus,
    },
    #[error("state is missing but a database artifact already exists at {path}")]
    MissingStateWithDatabase { path: PathBuf },
    #[error("index state {status} exists without its permanent lock at {path}")]
    StateWithoutPermanentLock {
        status: ExtractionStatus,
        path: PathBuf,
    },
    #[error("Current index state is blocked by the uninitialized tombstone at {path}")]
    CurrentTombstoned { path: PathBuf },
    #[error("Current index state has no SQLite database at {path}")]
    CurrentDatabaseMissing { path: PathBuf },
    #[error("Current index state has an unexpected SQLite sidecar at {path}")]
    CurrentWithDatabaseSidecar { path: PathBuf },
    #[error("failed to inspect database artifact {path}: {source}")]
    InspectArtifact {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read extraction stamp from SQLite database {path}: {source}")]
    ReadExtractionStamp {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to read SQLite database bytes from {path}: {source}")]
    ReadDatabase {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("SQLite database {path} has an invalid extraction stamp: {issue}")]
    InvalidExtractionStamp {
        path: PathBuf,
        issue: ExtractionStampIssue,
    },
    #[error("only a state-gated write Store may stamp the extraction version")]
    StampNotAuthorized,
    #[error("failed to stamp extraction version in SQLite database {path}: {source}")]
    StampExtractionVersion {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug)]
pub struct Store {
    // Field order is protocol behavior: Rust drops fields in declaration order,
    // so both SQLite connections close before the final retained lease owner
    // can unlock.
    pub(crate) conn: Connection,
    _source_conn: Option<Connection>,
    path: PathBuf,
    access: StoreAccess,
    guarded_paths: Option<IndexPaths>,
    lease: Option<IndexLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreAccess {
    Legacy,
    StateRead,
    StateWrite,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut conn = Connection::open(db_path).map_err(|source| StoreError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;

        migrations::configure_auto_vacuum_for_fresh_db(&conn).map_err(|source| {
            StoreError::Configure {
                path: db_path.to_path_buf(),
                source,
            }
        })?;

        configure_connection(&conn).map_err(|source| StoreError::Configure {
            path: db_path.to_path_buf(),
            source,
        })?;

        migrations::ensure_schema_and_migrations(&mut conn).map_err(|source| {
            StoreError::Migrate {
                path: db_path.to_path_buf(),
                source,
            }
        })?;

        Ok(Self {
            conn,
            _source_conn: None,
            path: db_path.to_path_buf(),
            access: StoreAccess::Legacy,
            guarded_paths: None,
            lease: None,
        })
    }

    /// Classify only the two fixed state slots. This never opens SQLite and
    /// never creates or mutates a filesystem entry.
    #[must_use]
    pub fn extraction_status(paths: &IndexPaths) -> ExtractionStatus {
        classify(paths).status().clone()
    }

    /// Acquire and retain a bounded shared lease, require Current state, and
    /// corroborate the existing database through a read-only/no-create SQLite
    /// handle and the exact current extraction stamp.
    pub fn open_for_read(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self> {
        let lease = match IndexLease::acquire_shared_existing(paths, deadline, cancelled) {
            Ok(lease) => lease,
            Err(IndexLeaseError::LockNotFound { .. }) => {
                return Err(lockless_state_error(paths)?);
            }
            Err(error) => return Err(StoreError::Lease(error)),
        };
        let status = Self::extraction_status(paths);
        reject_missing_database_artifacts(paths, &status)?;
        if status != ExtractionStatus::Current {
            return Err(StoreError::StateRejected { status });
        }
        open_current_read_only(paths, lease)
    }

    /// Probe status under a short shared lease. Contention is status data rather
    /// than an error and never opens SQLite. Non-Current states are reported
    /// without SQLite; Current is corroborated through [`Self::open_for_read`]'s
    /// exact read-only path while retaining the same acquired lease.
    pub fn open_for_status(
        paths: &IndexPaths,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<StoreStatusOpen> {
        let lease = match IndexLease::acquire_shared_existing(paths, deadline, cancelled) {
            Ok(lease) => lease,
            Err(IndexLeaseError::TimedOut { .. }) => {
                return Ok(StoreStatusOpen {
                    status: None,
                    rebuilding: true,
                    store: None,
                });
            }
            Err(IndexLeaseError::LockNotFound { .. }) => {
                let status = Self::extraction_status(paths);
                if status == ExtractionStatus::Missing {
                    reject_missing_database_artifacts(paths, &status)?;
                    return Ok(StoreStatusOpen {
                        status: Some(status),
                        rebuilding: false,
                        store: None,
                    });
                }
                if matches!(
                    status,
                    ExtractionStatus::Future { .. } | ExtractionStatus::Corrupt { .. }
                ) {
                    return Err(StoreError::StateRejected { status });
                }
                return Err(StoreError::StateWithoutPermanentLock {
                    status,
                    path: paths.permanent_lock(),
                });
            }
            Err(error) => return Err(StoreError::Lease(error)),
        };

        let status = Self::extraction_status(paths);
        reject_missing_database_artifacts(paths, &status)?;
        let store = if status == ExtractionStatus::Current {
            Some(open_current_read_only(paths, lease)?)
        } else {
            drop(lease);
            None
        };
        Ok(StoreStatusOpen {
            status: Some(status),
            rebuilding: false,
            store,
        })
    }

    /// Validate one already-held exclusive lease immediately, classify under
    /// it, and either open a corroborated Current DB or return an opaque,
    /// lease-retaining authorization for a later lifecycle operation.
    pub fn open_for_write(
        paths: &IndexPaths,
        lease: IndexLease,
        purpose: StoreWritePurpose,
    ) -> Result<StoreWriteOpen> {
        Self::open_for_write_with(paths, lease, purpose, || {})
    }

    fn open_for_write_with(
        paths: &IndexPaths,
        lease: IndexLease,
        purpose: StoreWritePurpose,
        before_write_open: impl FnOnce(),
    ) -> Result<StoreWriteOpen> {
        lease.validate_exclusive(paths)?;
        let status = Self::extraction_status(paths);

        if matches!(
            status,
            ExtractionStatus::Future { .. } | ExtractionStatus::Corrupt { .. }
        ) {
            return Err(StoreError::StateRejected { status });
        }
        if status == ExtractionStatus::Missing
            && let Some(path) = first_existing_database_artifact(paths)?
        {
            return Err(StoreError::MissingStateWithDatabase { path });
        }

        match purpose {
            StoreWritePurpose::CurrentMutation if status == ExtractionStatus::Current => {
                // Corroborate through a separate read-only/no-create handle before
                // any write-capable setup can alter pragmas or sidecars.
                let (corroboration, source_conn) = corroborate_current_database(paths)?;
                drop(corroboration);
                drop(source_conn);
                before_write_open();
                // The fixed lock path may be replaced after classification and
                // read-only corroboration. Revalidate the exact held handle at
                // the latest checkpoint, immediately before the first
                // write-capable SQLite open.
                lease.validate_exclusive(paths)?;
                let db_path = paths.current_db();
                let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                    .map_err(|source| StoreError::Open {
                        path: db_path.clone(),
                        source,
                    })?;
                configure_connection(&conn).map_err(|source| StoreError::Configure {
                    path: db_path.clone(),
                    source,
                })?;
                Ok(StoreWriteOpen::Current(Box::new(Self {
                    conn,
                    _source_conn: None,
                    path: db_path,
                    access: StoreAccess::StateWrite,
                    guarded_paths: Some(paths.clone()),
                    lease: Some(lease),
                })))
            }
            StoreWritePurpose::FullRebuild => {
                if status == ExtractionStatus::Current {
                    let (corroboration, source_conn) = corroborate_current_database(paths)?;
                    drop(corroboration);
                    drop(source_conn);
                }
                Ok(StoreWriteOpen::FullRebuildRequired(
                    StoreWriteAuthorization {
                        status,
                        purpose,
                        lease,
                    },
                ))
            }
            StoreWritePurpose::UninitContinuation if status == ExtractionStatus::Uninitialized => {
                Ok(StoreWriteOpen::UninitContinuation(
                    StoreWriteAuthorization {
                        status,
                        purpose,
                        lease,
                    },
                ))
            }
            _ => Err(StoreError::WritePurposeRejected { purpose, status }),
        }
    }

    /// Write the store-owned extraction metadata key and exact current decimal
    /// value. Read/status/legacy Stores are rejected before SQL execution.
    pub fn stamp_extraction_version(&self) -> Result<usize> {
        if self.access != StoreAccess::StateWrite || self.lease.is_none() {
            return Err(StoreError::StampNotAuthorized);
        }
        let paths = self
            .guarded_paths
            .as_ref()
            .expect("state-gated write Store always retains its IndexPaths");
        self.lease
            .as_ref()
            .expect("state-gated write Store always retains its lease")
            .validate_exclusive(paths)?;
        self.set_project_metadata(
            EXTRACTION_VERSION_KEY,
            &CURRENT_EXTRACTION_VERSION.to_string(),
        )
        .map_err(|source| StoreError::StampExtractionVersion {
            path: self.path.clone(),
            source,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        migrations::get_current_version(&self.conn)
    }
}

fn open_current_read_only(paths: &IndexPaths, lease: IndexLease) -> Result<Store> {
    let (conn, source_conn) = corroborate_current_database(paths)?;
    Ok(Store {
        conn,
        _source_conn: Some(source_conn),
        path: paths.current_db(),
        access: StoreAccess::StateRead,
        guarded_paths: Some(paths.clone()),
        lease: Some(lease),
    })
}

fn corroborate_current_database(paths: &IndexPaths) -> Result<(Connection, Connection)> {
    let db_path = paths.current_db();
    if artifact_exists(&paths.tombstone())? {
        return Err(StoreError::CurrentTombstoned {
            path: paths.tombstone(),
        });
    }
    if !artifact_exists(&db_path)? {
        return Err(StoreError::CurrentDatabaseMissing { path: db_path });
    }
    if let Some(path) = first_existing_database_sidecar(paths)? {
        return Err(StoreError::CurrentWithDatabaseSidecar { path });
    }
    let (conn, source_conn) = open_read_only_without_sidecars(&db_path)?;
    let stamp = conn
        .query_row(
            "SELECT value FROM project_metadata WHERE key = ?1",
            [EXTRACTION_VERSION_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| StoreError::ReadExtractionStamp {
            path: db_path.clone(),
            source,
        })?;
    validate_extraction_stamp(&db_path, stamp)?;
    Ok((conn, source_conn))
}

fn open_read_only_without_sidecars(db_path: &Path) -> Result<(Connection, Connection)> {
    // Opening an existing WAL-mode DB read-only is not enough to prevent SQLite
    // from creating `-wal`/`-shm` on the first query. Open the real path with
    // exactly READ_ONLY/no-create flags, then deserialize the already-checkpointed
    // main-DB bytes into a separate in-memory SQLite handle before executing SQL.
    // SQLite therefore performs the stamp query over a READONLY deserialized
    // image and never enters the file pager path that authors sidecars. The outer
    // shared IndexLease keeps the authoritative DB stable for the Store's whole
    // life, and the READ_ONLY source connection remains retained beside it.
    // Open the existing path with the required READ_ONLY/no-create flags before
    // constructing the sidecar-free query handle. This validates SQLite's own
    // file-open contract without executing a pager query that could create WAL
    // sidecars; that source connection is retained unchanged for the Store's
    // lifetime.
    let source_conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|source| StoreError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
    let bytes = std::fs::read(db_path).map_err(|source| StoreError::ReadDatabase {
        path: db_path.to_path_buf(),
        source,
    })?;
    let mut conn = Connection::open_in_memory().map_err(|source| StoreError::Open {
        path: db_path.to_path_buf(),
        source,
    })?;
    deserialize_read_only(&mut conn, &bytes).map_err(|source| StoreError::Open {
        path: db_path.to_path_buf(),
        source,
    })?;
    Ok((conn, source_conn))
}

fn deserialize_read_only(conn: &mut Connection, bytes: &[u8]) -> rusqlite::Result<()> {
    use std::ptr::NonNull;

    let (size, deserialize_size) = deserialize_sizes(bytes.len())?;
    // Keep every fallible size conversion above the ownership-bearing
    // allocation. After sqlite3_malloc64 succeeds, no Rust `?` may return before
    // sqlite3_deserialize assumes FREEONCLOSE ownership.
    // SAFETY: sqlite3_malloc64 returns an allocation suitable for
    // SQLITE_DESERIALIZE_FREEONCLOSE. The byte copy uses the exact allocation
    // length. Once sqlite3_deserialize is called, SQLite owns and frees the
    // allocation both on success and on failure.
    let allocation = unsafe { rusqlite::ffi::sqlite3_malloc64(size) };
    let allocation = NonNull::new(allocation.cast::<u8>()).ok_or_else(|| {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOMEM), None)
    })?;
    // SAFETY: allocation is valid for `bytes.len()` bytes and non-overlapping
    // with the borrowed source slice.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocation.as_ptr(), bytes.len());
    }
    if bytes.len() >= 20 && bytes.starts_with(b"SQLite format 3\0") {
        // A clean checkpointed WAL database still records WAL read/write format
        // bytes (2) in its main-file header. The deserialized image has no WAL
        // sidecar by design, so normalize only this private in-memory copy to
        // rollback format (1) before SQLite reads it. Disk bytes remain exact.
        // SAFETY: the allocation has `bytes.len() >= 20` writable bytes here.
        unsafe {
            allocation.as_ptr().add(18).write(1);
            allocation.as_ptr().add(19).write(1);
        }
    }
    let schema = c"main";
    // SAFETY: `conn.handle()` is live for this call; allocation and lengths meet
    // sqlite3_deserialize's contract. READONLY forbids image mutation and
    // FREEONCLOSE transfers allocation ownership to SQLite on success.
    let result = unsafe {
        rusqlite::ffi::sqlite3_deserialize(
            conn.handle(),
            schema.as_ptr(),
            allocation.as_ptr(),
            deserialize_size,
            deserialize_size,
            rusqlite::ffi::SQLITE_DESERIALIZE_FREEONCLOSE
                | rusqlite::ffi::SQLITE_DESERIALIZE_READONLY,
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        // FREEONCLOSE also requires SQLite to free the buffer before returning
        // from a failed sqlite3_deserialize call; do not free it a second time.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(result),
            None,
        ));
    }
    Ok(())
}

fn deserialize_sizes(len: usize) -> rusqlite::Result<(u64, i64)> {
    let allocation_size = u64::try_from(len).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let image_size = i64::try_from(len).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok((allocation_size, image_size))
}

fn validate_extraction_stamp(db_path: &Path, stamp: Option<String>) -> Result<()> {
    let Some(stamp) = stamp else {
        return Err(StoreError::InvalidExtractionStamp {
            path: db_path.to_path_buf(),
            issue: ExtractionStampIssue::Missing,
        });
    };
    let built = stamp
        .parse::<u64>()
        .map_err(|_| StoreError::InvalidExtractionStamp {
            path: db_path.to_path_buf(),
            issue: ExtractionStampIssue::Malformed {
                found: stamp.clone(),
            },
        })?;
    if built.to_string() != stamp {
        return Err(StoreError::InvalidExtractionStamp {
            path: db_path.to_path_buf(),
            issue: ExtractionStampIssue::Malformed { found: stamp },
        });
    }
    if built != CURRENT_EXTRACTION_VERSION {
        return Err(StoreError::InvalidExtractionStamp {
            path: db_path.to_path_buf(),
            issue: ExtractionStampIssue::Mismatch {
                expected: CURRENT_EXTRACTION_VERSION,
                found: built,
            },
        });
    }
    Ok(())
}

fn first_existing_database_artifact(paths: &IndexPaths) -> Result<Option<PathBuf>> {
    let db = paths.current_db();
    let mut artifacts = vec![db.clone()];
    artifacts.push(PathBuf::from(format!("{}-wal", db.display())));
    artifacts.push(PathBuf::from(format!("{}-shm", db.display())));
    for path in artifacts {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(StoreError::InspectArtifact { path, source }),
        }
    }
    Ok(None)
}

fn first_existing_database_sidecar(paths: &IndexPaths) -> Result<Option<PathBuf>> {
    let db = paths.current_db();
    for path in [
        PathBuf::from(format!("{}-wal", db.display())),
        PathBuf::from(format!("{}-shm", db.display())),
    ] {
        if artifact_exists(&path)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn reject_missing_database_artifacts(paths: &IndexPaths, status: &ExtractionStatus) -> Result<()> {
    if status == &ExtractionStatus::Missing
        && let Some(path) = first_existing_database_artifact(paths)?
    {
        return Err(StoreError::MissingStateWithDatabase { path });
    }
    Ok(())
}

fn lockless_state_error(paths: &IndexPaths) -> Result<StoreError> {
    let status = Store::extraction_status(paths);
    if status == ExtractionStatus::Missing {
        reject_missing_database_artifacts(paths, &status)?;
        return Ok(StoreError::StateRejected { status });
    }
    if matches!(
        status,
        ExtractionStatus::Future { .. } | ExtractionStatus::Corrupt { .. }
    ) {
        return Ok(StoreError::StateRejected { status });
    }
    Ok(StoreError::StateWithoutPermanentLock {
        status,
        path: paths.permanent_lock(),
    })
}

fn artifact_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(StoreError::InspectArtifact {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    // Order mirrors the upstream configureConnection exactly. busy_timeout must be
    // first so later file-touching pragmas wait instead of immediately failing.
    conn.busy_timeout(std::time::Duration::from_millis(5_000))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", -64_000)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 268_435_456_i64)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum SnapshotKind {
        Directory,
        File(Vec<u8>),
        Symlink(PathBuf),
    }

    fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<PathBuf, SnapshotKind> {
        fn walk(
            root: &Path,
            directory: &Path,
            out: &mut std::collections::BTreeMap<PathBuf, SnapshotKind>,
        ) {
            let mut paths = std::fs::read_dir(directory)
                .unwrap_or_else(|error| {
                    panic!("snapshot read_dir {}: {error}", directory.display())
                })
                .map(|entry| {
                    entry
                        .unwrap_or_else(|error| {
                            panic!("snapshot entry in {}: {error}", directory.display())
                        })
                        .path()
                })
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or_else(|error| panic!("snapshot strip {}: {error}", path.display()))
                    .to_path_buf();
                let metadata = std::fs::symlink_metadata(&path).unwrap_or_else(|error| {
                    panic!("snapshot metadata {}: {error}", path.display())
                });
                let file_type = metadata.file_type();
                let kind = if file_type.is_dir() {
                    SnapshotKind::Directory
                } else if file_type.is_file() {
                    SnapshotKind::File(std::fs::read(&path).unwrap_or_else(|error| {
                        panic!("snapshot read {}: {error}", path.display())
                    }))
                } else if file_type.is_symlink() {
                    SnapshotKind::Symlink(std::fs::read_link(&path).unwrap_or_else(|error| {
                        panic!("snapshot read_link {}: {error}", path.display())
                    }))
                } else {
                    panic!("snapshot unsupported entry kind: {}", path.display());
                };
                assert!(
                    out.insert(relative, kind).is_none(),
                    "duplicate snapshot path"
                );
                if file_type.is_dir() {
                    walk(root, &path, out);
                }
            }
        }

        let mut out = std::collections::BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codegraph-conn-{label}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn pragmas_match_upstream_connection_settings() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();

        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            -64_000
        );
        assert_eq!(
            conn.query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn impossible_deserialize_size_is_rejected_before_ownership_bearing_allocation() {
        assert!(matches!(
            deserialize_sizes(usize::MAX),
            Err(rusqlite::Error::InvalidQuery)
        ));
    }

    #[test]
    fn open_creates_parent_dir_migrates_and_exposes_accessors() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-open-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_path = base.join("nested").join("graph.db");
        let store = Store::open(&db_path).expect("open creates nested dirs and migrates");

        assert!(db_path.exists(), "db file created");
        assert_eq!(store.path(), db_path.as_path());
        assert_eq!(
            store.schema_version().unwrap(),
            crate::migrations::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            store
                .connection()
                .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap()
                .to_lowercase(),
            "wal"
        );

        drop(store);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reopening_existing_db_keeps_schema_version() {
        let db_path = temp_db_path("reopen");
        let v1 = Store::open(&db_path).unwrap().schema_version().unwrap();
        let v2 = Store::open(&db_path).unwrap().schema_version().unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1, crate::migrations::CURRENT_SCHEMA_VERSION);

        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", db_path.display()));
        }
    }

    #[test]
    #[cfg(unix)]
    fn open_on_unwritable_path_surfaces_open_error() {
        let bogus = Path::new("/proc/definitely-not-writable/graph.db");
        match Store::open(bogus) {
            Ok(_) => panic!("open must fail on an unwritable location"),
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    matches!(err, StoreError::CreateDir { .. } | StoreError::Open { .. }),
                    "unexpected error variant: {msg}"
                );
            }
        }
    }

    #[test]
    fn open_on_a_non_sqlite_file_surfaces_migrate_error() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-garbage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let db_path = base.join("graph.db");
        std::fs::write(&db_path, b"this is not a sqlite database at all").unwrap();

        let Err(err) = Store::open(&db_path) else {
            panic!("a non-sqlite file must fail to open+migrate");
        };
        assert!(
            matches!(
                err,
                StoreError::Configure { .. } | StoreError::Migrate { .. } | StoreError::Open { .. }
            ),
            "a corrupt db must surface a Configure/Migrate/Open error, got: {err}"
        );
        assert!(err.to_string().contains(&db_path.display().to_string()));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_when_db_path_is_a_directory_surfaces_open_error() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-dbdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_path = base.join("graph.db");
        std::fs::create_dir_all(&db_path).unwrap();

        let Err(err) = Store::open(&db_path) else {
            panic!("opening a directory as a db file must fail");
        };
        assert!(
            matches!(err, StoreError::Open { .. } | StoreError::Configure { .. }),
            "a directory db path must surface an Open/Configure error, got: {err}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_when_parent_is_a_file_surfaces_create_dir_error() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-fileparent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let db_path = blocker.join("nested").join("graph.db");

        let Err(err) = Store::open(&db_path) else {
            panic!("creating a dir under a regular file must fail at CreateDir");
        };
        assert!(
            matches!(err, StoreError::CreateDir { .. }),
            "unexpected error variant: {err}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn store_error_messages_name_the_path_for_each_variant() {
        let p = PathBuf::from("/tmp/x.db");
        let io_err = || std::io::Error::other("boom");
        let sql_err = || rusqlite::Error::InvalidQuery;

        let create = StoreError::CreateDir {
            path: p.clone(),
            source: io_err(),
        };
        assert!(create.to_string().contains("/tmp/x.db"));
        let open = StoreError::Open {
            path: p.clone(),
            source: sql_err(),
        };
        assert!(open.to_string().contains("/tmp/x.db"));
        let configure = StoreError::Configure {
            path: p.clone(),
            source: sql_err(),
        };
        assert!(configure.to_string().contains("/tmp/x.db"));
        let migrate = StoreError::Migrate {
            path: p,
            source: sql_err(),
        };
        assert!(migrate.to_string().contains("/tmp/x.db"));
    }

    #[test]
    fn current_writer_revalidates_lock_at_the_last_pre_open_checkpoint() {
        fn deadline() -> Instant {
            Instant::now() + std::time::Duration::from_secs(5)
        }

        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-late-lock-replacement-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&base).unwrap();
        let project = base.canonicalize().unwrap();
        let paths = IndexPaths::resolve(&project, None).unwrap();
        let initial = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
        let fixture = Store::open(&paths.current_db()).unwrap();
        fixture
            .set_project_metadata(
                EXTRACTION_VERSION_KEY,
                &CURRENT_EXTRACTION_VERSION.to_string(),
            )
            .unwrap();
        fixture.restore_default_pragmas().unwrap();
        drop(fixture);
        let state = serde_json::json!({
            "sequence": 1,
            "storageProtocol": crate::CURRENT_STORAGE_PROTOCOL,
            "extractionVersion": CURRENT_EXTRACTION_VERSION,
            "phase": "current",
            "projectIdentity": paths.project_identity(),
            "checksum": crate::checksum_hex(
                1,
                crate::CURRENT_STORAGE_PROTOCOL,
                CURRENT_EXTRACTION_VERSION,
                "current",
                paths.project_identity(),
            ),
        });
        std::fs::write(&paths.state_slots()[0], serde_json::to_vec(&state).unwrap()).unwrap();
        drop(initial);
        let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false).unwrap();
        let displaced = paths.current_root().join("displaced-at-write-open.lock");
        let expected_after_replacement = std::cell::RefCell::new(None);

        let error =
            Store::open_for_write_with(&paths, lease, StoreWritePurpose::CurrentMutation, || {
                std::fs::rename(paths.permanent_lock(), &displaced).unwrap();
                std::fs::write(paths.permanent_lock(), b"late replacement").unwrap();
                expected_after_replacement.replace(Some(snapshot_tree(&project)));
            })
            .expect_err("late lock replacement must prevent write-capable SQLite open");
        assert!(matches!(
            error,
            StoreError::LeaseValidation(IndexLeaseValidationError::PermanentLockChanged { .. })
        ));
        assert_eq!(
            snapshot_tree(&project),
            expected_after_replacement
                .into_inner()
                .expect("checkpoint captured post-replacement tree"),
            "rejection must not mutate any byte after the deterministic replacement checkpoint"
        );

        let _ = std::fs::remove_dir_all(base);
    }
}
