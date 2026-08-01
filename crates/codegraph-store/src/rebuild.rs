//! Explicit, fallible finalization of a destructive v2 full rebuild.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md` lines 548-556: under ONE
//! retained exclusive [`IndexLease`], a destructive rebuild
//!
//! 1. classifies the namespace and takes its write authorization,
//! 2. publishes `phase=building` BEFORE deleting or mutating the database,
//! 3. removes only the v2 `codegraph.db`, `-wal`, and `-shm`,
//! 4. rebuilds into a fresh state-gated writer database, and
//! 5. finalizes: restore default pragmas -> final checkpoint + compaction ->
//!    stamp extraction version -> checkpoint that stamp into the main database ->
//!    close the final SQLite connection.
//!
//! Only after every one of those steps succeeded may the rebuild atomically
//! publish `phase=current`; it then removes a tombstone only for an explicit,
//! successful `init`. Any earlier fault leaves the namespace `Building` (or
//! `Missing`, before the first publication), fail-closed and unreadable. The
//! handle's `Drop` is emergency best-effort only: it can never publish `Current`,
//! because no publication call exists on that path.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use codegraph_core::IndexPaths;
use thiserror::Error;

use crate::connection::{Store, StoreError, StoreWriteOpen, StoreWritePurpose};
use crate::{
    IndexLease, IndexLeaseError, IndexLeaseValidationError, StatePhase, StatePublishError,
    publish_index_state,
};

/// Which lifecycle command is performing the destructive rebuild.
///
/// The distinction is narrow and load-bearing: only an explicit, fully
/// successful `init` may remove an existing tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildKind {
    /// `codegraph init` — an explicit initialization/recovery rebuild.
    ExplicitInit,
    /// `codegraph index` / forced migration — an ordinary destructive rebuild.
    Reindex,
}

/// Deterministic finalization boundaries. Private: production always uses the
/// no-op injector, while unit tests inject a fault after (or immediately before)
/// each named boundary without any sleep or polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebuildCheckpoint {
    BeforeWriteAuthorization,
    BuildingPublished,
    BeforeDatabaseRemoval,
    DatabaseRemoved,
    BeforePragmasRestored,
    PragmasRestored,
    BeforeCompaction,
    Compacted,
    BeforeVersionStamp,
    VersionStamped,
    BeforeStampCheckpoint,
    StampCheckpointed,
    BeforeConnectionClose,
    ConnectionClosed,
    BeforeCurrentPublication,
    CurrentPublished,
    BeforeTombstoneRemoval,
    AfterTombstoneRemoval,
}

impl RebuildCheckpoint {
    fn label(self) -> &'static str {
        match self {
            Self::BeforeWriteAuthorization => "classify for full-rebuild authorization",
            Self::BuildingPublished => "publish phase=building",
            Self::BeforeDatabaseRemoval => "prepare v2 database artifact removal",
            Self::DatabaseRemoved => "remove v2 database files",
            Self::BeforePragmasRestored => "prepare default pragma restoration",
            Self::PragmasRestored => "restore default pragmas",
            Self::BeforeCompaction => "prepare database checkpoint and compaction",
            Self::Compacted => "checkpoint and compact the database",
            Self::BeforeVersionStamp => "prepare extraction-version stamp",
            Self::VersionStamped => "stamp the extraction version",
            Self::BeforeStampCheckpoint => "prepare extraction-stamp checkpoint",
            Self::StampCheckpointed => "checkpoint the extraction stamp into the main database",
            Self::BeforeConnectionClose => "prepare final SQLite connection close",
            Self::ConnectionClosed => "close the final SQLite connection",
            Self::BeforeCurrentPublication => "prepare phase=current publication",
            Self::CurrentPublished => "publish phase=current",
            Self::BeforeTombstoneRemoval => "prepare uninitialized tombstone removal",
            Self::AfterTombstoneRemoval => "after removing the uninitialized tombstone",
        }
    }
}

trait RebuildFault {
    fn at(&mut self, checkpoint: RebuildCheckpoint) -> io::Result<()>;

    fn remove_tombstone(&mut self, path: &std::path::Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
}

struct NoFault;

impl RebuildFault for NoFault {
    fn at(&mut self, _checkpoint: RebuildCheckpoint) -> io::Result<()> {
        Ok(())
    }
}

/// Typed failures of a destructive rebuild's lease, state, or finalization work.
#[derive(Debug, Error)]
pub enum RebuildError {
    /// The single outer exclusive lease could not be acquired or created.
    #[error(transparent)]
    Lease(#[from] IndexLeaseError),
    /// The retained capability stopped authorizing this namespace.
    #[error(transparent)]
    LeaseValidation(#[from] IndexLeaseValidationError),
    /// The state-gated store layer refused the rebuild.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A state-slot publication failed.
    #[error(transparent)]
    Publish(#[from] StatePublishError),
    /// The current root could not be inspected before lease selection.
    #[error("cannot inspect index root {path}: {source}")]
    InspectRoot {
        /// Resolved current root.
        path: PathBuf,
        /// Operating-system error.
        source: io::Error,
    },
    /// A v2 database artifact could not be removed under the lease.
    #[error("cannot remove v2 database artifact {path}: {source}")]
    RemoveDatabase {
        /// Exact artifact path.
        path: PathBuf,
        /// Operating-system error.
        source: io::Error,
    },
    /// An interrupted-uninit namespace was reached by an ordinary reindex.
    #[error(
        "index namespace {path} was left uninitialized; only an explicit `init` may rebuild it"
    )]
    UninitializedRequiresExplicitInit {
        /// Resolved current root.
        path: PathBuf,
    },
    /// The tombstone could not be removed after a successful explicit init.
    #[error("cannot remove the uninitialized tombstone {path}: {source}")]
    RemoveTombstone {
        /// Tombstone path.
        path: PathBuf,
        /// Operating-system error.
        source: io::Error,
    },
    /// A SQLite finalization statement failed.
    #[error("cannot {step} for {path}: {source}")]
    Finalize {
        /// Stable finalization-step description.
        step: &'static str,
        /// Database path.
        path: PathBuf,
        /// SQLite error.
        source: rusqlite::Error,
    },
    /// A deterministic test fault interrupted the rebuild at a named boundary.
    #[error("interrupted rebuild at {step}: {source}")]
    Interrupted {
        /// Stable boundary description.
        step: &'static str,
        /// Injected error.
        source: io::Error,
    },
}

/// One destructive rebuild in progress: `phase=building` is already published and
/// the previous database is already removed. The exclusive lease acquired before
/// classification is retained here for the whole rebuild, including
/// finalization, connection close, and the final `phase=current` publication.
///
/// Dropping this unopened handle deliberately performs NO publication: the
/// namespace stays `phase=building`, unreadable and fail-closed, and a rerun
/// rebuilds it from scratch. Once [`FullRebuild::open_store`] consumes it, the
/// resulting [`ActiveFullRebuild`] owns the unique writer and its emergency
/// best-effort, publication-free cleanup.
#[derive(Debug)]
pub struct FullRebuild {
    paths: IndexPaths,
    lease: IndexLease,
    kind: RebuildKind,
    // Retains the state-gated write authorization (and its own lease clone) for
    // the rebuild's whole lifetime so the capability cannot be re-derived.
    _authorization: crate::connection::StoreWriteAuthorization,
}

/// The unique final SQLite writer opened by one [`FullRebuild`]. The writer is
/// owned by this typestate and cannot be supplied by a caller to [`Self::finish`],
/// so finalization is necessarily tied to the exact path, lease, authorization,
/// and connection opened by the rebuild capability.
#[derive(Debug)]
pub struct ActiveFullRebuild {
    paths: IndexPaths,
    lease: IndexLease,
    kind: RebuildKind,
    _authorization: crate::connection::StoreWriteAuthorization,
    store: Option<Store>,
}

/// Acquire the single outer exclusive lease, classify under it, publish
/// `phase=building`, and remove the previous v2 database files.
pub fn begin_full_rebuild(
    paths: &IndexPaths,
    kind: RebuildKind,
    deadline: Instant,
    cancelled: impl FnMut() -> bool,
) -> Result<FullRebuild, RebuildError> {
    begin_full_rebuild_with(paths, kind, deadline, cancelled, &mut NoFault)
}

fn begin_full_rebuild_with(
    paths: &IndexPaths,
    kind: RebuildKind,
    deadline: Instant,
    cancelled: impl FnMut() -> bool,
    fault: &mut impl RebuildFault,
) -> Result<FullRebuild, RebuildError> {
    let lease = acquire_outer_exclusive(paths, deadline, cancelled)?;
    checkpoint(fault, RebuildCheckpoint::BeforeWriteAuthorization)?;
    // Classification happens through the state gate under the already-held
    // exclusive capability; a Future/Corrupt namespace is refused before any
    // byte changes.
    let authorized = if kind == RebuildKind::ExplicitInit {
        Store::open_for_explicit_init_rebuild(paths, lease.clone())?
    } else {
        Store::open_for_write(paths, lease.clone(), StoreWritePurpose::FullRebuild)?
    };
    let authorization = match authorized {
        StoreWriteOpen::FullRebuildRequired(authorization) => authorization,
        other => {
            unreachable!("FullRebuild purpose always yields FullRebuildRequired, got {other:?}")
        }
    };
    // Authorization's status is the accepted classification performed while the
    // exclusive lease was held. Never consult a precheck and then trust a second
    // classification: that would create a classification/authorization TOCTOU.
    if authorization.status() == &crate::ExtractionStatus::Uninitialized
        && kind != RebuildKind::ExplicitInit
    {
        return Err(RebuildError::UninitializedRequiresExplicitInit {
            path: paths.current_root().to_path_buf(),
        });
    }

    begin_from_authorization(paths, lease, kind, authorization, fault)
}

/// Escalate an ALREADY-AUTHORIZED namespace to a destructive full rebuild using
/// the exclusive lease the authorization still retains.
///
/// This is the migration entry point for an incremental sync that classified
/// `Missing`, `Outdated`, or a recoverable `Building` under its one outer
/// exclusive lease (frozen plan lines 557-565). No lease is acquired here: doing
/// so would be a nested acquisition of a lock this process already holds. The
/// retained authorization's status is the accepted classification, so no
/// reclassification-after-precheck TOCTOU is possible.
pub fn resume_full_rebuild(
    paths: &IndexPaths,
    authorization: crate::connection::StoreWriteAuthorization,
) -> Result<FullRebuild, RebuildError> {
    resume_full_rebuild_with(paths, authorization, &mut NoFault)
}

fn resume_full_rebuild_with(
    paths: &IndexPaths,
    authorization: crate::connection::StoreWriteAuthorization,
    fault: &mut impl RebuildFault,
) -> Result<FullRebuild, RebuildError> {
    if authorization.purpose() != StoreWritePurpose::IncrementalSync {
        return Err(RebuildError::Store(StoreError::WritePurposeRejected {
            purpose: authorization.purpose(),
            status: authorization.status().clone(),
        }));
    }
    // Only the states the sync gate escalates may migrate. `Uninitialized` is
    // never escalated: it is reserved for an explicit `init`.
    match authorization.status() {
        crate::ExtractionStatus::Missing
        | crate::ExtractionStatus::Outdated { .. }
        | crate::ExtractionStatus::Building { .. } => {}
        other => {
            return Err(RebuildError::Store(StoreError::WritePurposeRejected {
                purpose: authorization.purpose(),
                status: other.clone(),
            }));
        }
    }
    let lease = authorization.clone_lease();
    lease.validate_exclusive(paths)?;
    begin_from_authorization(paths, lease, RebuildKind::Reindex, authorization, fault)
}

/// Shared destructive prologue: publish `phase=building` BEFORE deleting any
/// database byte, then remove only the v2 database files — all under the one
/// already-held exclusive lease carried by `authorization`.
fn begin_from_authorization(
    paths: &IndexPaths,
    lease: IndexLease,
    kind: RebuildKind,
    authorization: crate::connection::StoreWriteAuthorization,
    fault: &mut impl RebuildFault,
) -> Result<FullRebuild, RebuildError> {
    // `phase=building` is durable BEFORE any destructive database work, so an
    // interruption leaves an explicit, owner-bound marker instead of a bare DB.
    publish_index_state(paths, &lease, StatePhase::Building)?;
    checkpoint(fault, RebuildCheckpoint::BuildingPublished)?;
    checkpoint(fault, RebuildCheckpoint::BeforeDatabaseRemoval)?;
    remove_database_files(paths, &lease)?;
    checkpoint(fault, RebuildCheckpoint::DatabaseRemoved)?;

    Ok(FullRebuild {
        paths: paths.clone(),
        lease,
        kind,
        _authorization: authorization,
    })
}

impl FullRebuild {
    /// Which lifecycle command owns this rebuild.
    #[must_use]
    pub fn kind(&self) -> RebuildKind {
        self.kind
    }

    /// The resolved paths this rebuild is bound to.
    #[must_use]
    pub fn paths(&self) -> &IndexPaths {
        &self.paths
    }

    /// Consume the unopened capability and open its one final SQLite writer.
    /// Consuming `self` makes a second live writer structurally impossible.
    pub fn open_store(self) -> Result<ActiveFullRebuild, RebuildError> {
        self.open_store_with(|| {})
    }

    fn open_store_with(
        self,
        before_open: impl FnOnce(),
    ) -> Result<ActiveFullRebuild, RebuildError> {
        let store = Store::open_rebuild_target_with(&self.paths, &self.lease, before_open)?;
        Ok(ActiveFullRebuild {
            paths: self.paths,
            lease: self.lease,
            kind: self.kind,
            _authorization: self._authorization,
            store: Some(store),
        })
    }
}

impl ActiveFullRebuild {
    /// The resolved paths this active rebuild is bound to.
    #[must_use]
    pub fn paths(&self) -> &IndexPaths {
        &self.paths
    }

    /// Access the exact writer owned by this rebuild.
    #[must_use]
    pub fn store(&self) -> &Store {
        self.store
            .as_ref()
            .expect("an active rebuild owns its writer until finish")
    }

    /// Mutably access the exact writer owned by this rebuild.
    #[must_use]
    pub fn store_mut(&mut self) -> &mut Store {
        self.store
            .as_mut()
            .expect("an active rebuild owns its writer until finish")
    }

    /// Apply the full-index pragma profile only after revalidating the retained
    /// fixed-lock capability at this pragma mutation boundary.
    pub fn set_bulk_index_pragmas(&self) -> Result<(), RebuildError> {
        let store = self
            .store
            .as_ref()
            .expect("an active rebuild owns its writer until finish");
        let path = store.path().to_path_buf();
        let pragma = |result: rusqlite::Result<()>| {
            result.map_err(|source| RebuildError::Finalize {
                step: "set bulk-index pragmas",
                path: path.clone(),
                source,
            })
        };
        store.validate_state_write_authority()?;
        pragma(store.connection().pragma_update(None, "synchronous", "OFF"))?;
        store.validate_state_write_authority()?;
        pragma(
            store
                .connection()
                .pragma_update(None, "cache_size", -262_144),
        )?;
        store.validate_state_write_authority()?;
        pragma(
            store
                .connection()
                .pragma_update(None, "mmap_size", 1_073_741_824_i64),
        )?;
        if !matches!(std::env::var(crate::CODEGRAPH_NO_WAL_DEFER), Ok(value) if value == "1") {
            store.validate_state_write_authority()?;
            pragma(
                store
                    .connection()
                    .pragma_update(None, "wal_autocheckpoint", 0),
            )?;
        }
        Ok(())
    }

    /// Apply the bulk-index WAL valve only after revalidating the retained
    /// capability at the checkpoint mutation boundary.
    pub fn checkpoint_wal_if_over(&self, threshold_bytes: u64) -> Result<bool, RebuildError> {
        let store = self
            .store
            .as_ref()
            .expect("an active rebuild owns its writer until finish");
        if store.wal_size_bytes()? <= threshold_bytes {
            return Ok(false);
        }
        store.validate_state_write_authority()?;
        store
            .connection()
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(|source| RebuildError::Finalize {
                step: "checkpoint bulk-index WAL valve",
                path: store.path().to_path_buf(),
                source,
            })?;
        Ok(true)
    }

    /// Explicit fallible completion. Every step runs under the same retained
    /// exclusive lease; any failure propagates and leaves the namespace
    /// `Building` and unreadable.
    pub fn finish(self) -> Result<(), RebuildError> {
        self.finish_with(&mut NoFault)
    }

    fn finish_with(mut self, fault: &mut impl RebuildFault) -> Result<(), RebuildError> {
        // `finish` consumes the handle, so a caller cannot finalize twice and an
        // abandoned rebuild can only reach the publication-free Drop path.
        let store = self
            .store
            .as_ref()
            .expect("an active rebuild owns its final writer until finish");
        let db_path = store.path().to_path_buf();

        checkpoint(fault, RebuildCheckpoint::BeforePragmasRestored)?;
        restore_default_pragmas(store, &db_path)?;
        checkpoint(fault, RebuildCheckpoint::PragmasRestored)?;

        checkpoint(fault, RebuildCheckpoint::BeforeCompaction)?;
        compact(store, &db_path)?;
        checkpoint(fault, RebuildCheckpoint::Compacted)?;

        checkpoint(fault, RebuildCheckpoint::BeforeVersionStamp)?;
        store.stamp_extraction_version()?;
        checkpoint(fault, RebuildCheckpoint::VersionStamped)?;

        // The stamp must live in the main database file, not only in the WAL:
        // a Current namespace is corroborated from main-file bytes.
        checkpoint(fault, RebuildCheckpoint::BeforeStampCheckpoint)?;
        store.checkpoint_wal_truncate()?;
        checkpoint(fault, RebuildCheckpoint::StampCheckpointed)?;

        // Close the final SQLite connection (and drop its lease clone) BEFORE
        // publishing Current, so no writer handle survives the transition. The
        // outer lease below is still held by `self`.
        checkpoint(fault, RebuildCheckpoint::BeforeConnectionClose)?;
        let store = self
            .store
            .take()
            .expect("the validated final writer is still owned before close");
        store.close()?;
        checkpoint(fault, RebuildCheckpoint::ConnectionClosed)?;

        checkpoint(fault, RebuildCheckpoint::BeforeCurrentPublication)?;
        publish_index_state(&self.paths, &self.lease, StatePhase::Current)?;
        checkpoint(fault, RebuildCheckpoint::CurrentPublished)?;

        if self.kind == RebuildKind::ExplicitInit {
            // Current+tombstone is deliberately fail-closed. Revalidate the same
            // retained fixed-lock handle immediately before the removal
            // operation, then inject any deterministic failure AT that operation.
            checkpoint(fault, RebuildCheckpoint::BeforeTombstoneRemoval)?;
            self.lease.validate_exclusive(&self.paths)?;
            remove_tombstone(&self.paths, fault)?;
        }
        checkpoint(fault, RebuildCheckpoint::AfterTombstoneRemoval)?;
        Ok(())
    }

    /// Run the publication-free emergency cleanup used by [`Drop`]. The callback
    /// is a private deterministic test seam at the last pre-close boundary;
    /// production passes a no-op. Returning the close result lets the regression
    /// prove that this path uses [`Store::close`]'s state gate rather than a raw
    /// field drop. Validation and SQLite close remain separate API calls, so this
    /// is an explicit best-effort boundary, not a claim of portable atomicity.
    fn emergency_cleanup_with(
        &mut self,
        before_close: impl FnOnce(),
    ) -> Option<Result<(), StoreError>> {
        let store = self.store.take()?;
        let path = store.path().to_path_buf();
        if let Err(error) = restore_default_pragmas(&store, &path) {
            log_emergency_cleanup_error("restore default pragmas", &path, &error);
        }
        if let Err(error) = compact(&store, &path) {
            log_emergency_cleanup_error("compact the database", &path, &error);
        }
        before_close();
        Some(store.close())
    }
}

impl Drop for ActiveFullRebuild {
    fn drop(&mut self) {
        // Emergency cleanup is deliberately limited to operations safe while
        // `phase=building`: best-effort pragma restoration/checkpoint and compact,
        // each guarded by a fresh validation of the retained capability, followed
        // by the explicit state-gated final connection close. There is no
        // state-publication or tombstone-removal call in this path, so Drop
        // structurally cannot publish Current.
        if let Some(Err(error)) = self.emergency_cleanup_with(|| {}) {
            let path = self.paths.current_db();
            log_emergency_cleanup_error("close the final SQLite connection", &path, &error);
        }
    }
}

fn log_emergency_cleanup_error(step: &str, path: &std::path::Path, error: &dyn std::fmt::Display) {
    // This crate intentionally has no logger dependency. STDERR is protocol-safe
    // for CLI and stdio-MCP callers, unlike stdout. Ignore the write result so an
    // emergency cleanup diagnostic can never turn Drop into a panic.
    let _ = writeln!(
        io::stderr().lock(),
        "codegraph-store: emergency rebuild cleanup could not {step} for {}: {error}",
        path.display()
    );
}

impl std::ops::Deref for ActiveFullRebuild {
    type Target = Store;

    fn deref(&self) -> &Self::Target {
        self.store()
    }
}

impl std::ops::DerefMut for ActiveFullRebuild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.store_mut()
    }
}

fn restore_default_pragmas(store: &Store, path: &std::path::Path) -> Result<(), RebuildError> {
    store.validate_state_write_authority()?;
    store
        .connection()
        .pragma_update(None, "wal_checkpoint", "TRUNCATE")
        .map_err(|source| RebuildError::Finalize {
            step: RebuildCheckpoint::PragmasRestored.label(),
            path: path.to_path_buf(),
            source,
        })?;
    store.validate_state_write_authority()?;
    store
        .connection()
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|source| RebuildError::Finalize {
            step: RebuildCheckpoint::PragmasRestored.label(),
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn compact(store: &Store, path: &std::path::Path) -> Result<(), RebuildError> {
    store.validate_state_write_authority()?;
    store
        .connection()
        .pragma_update(None, "wal_checkpoint", "TRUNCATE")
        .map_err(|source| RebuildError::Finalize {
            step: RebuildCheckpoint::Compacted.label(),
            path: path.to_path_buf(),
            source,
        })?;
    store.validate_state_write_authority()?;
    let mut statement = store
        .connection()
        .prepare("PRAGMA incremental_vacuum")
        .map_err(|source| RebuildError::Finalize {
            step: RebuildCheckpoint::Compacted.label(),
            path: path.to_path_buf(),
            source,
        })?;
    let mut rows = statement
        .query([])
        .map_err(|source| RebuildError::Finalize {
            step: RebuildCheckpoint::Compacted.label(),
            path: path.to_path_buf(),
            source,
        })?;
    while rows
        .next()
        .map_err(|source| RebuildError::Finalize {
            step: RebuildCheckpoint::Compacted.label(),
            path: path.to_path_buf(),
            source,
        })?
        .is_some()
    {}
    Ok(())
}

fn checkpoint(
    fault: &mut impl RebuildFault,
    checkpoint: RebuildCheckpoint,
) -> Result<(), RebuildError> {
    fault
        .at(checkpoint)
        .map_err(|source| RebuildError::Interrupted {
            step: checkpoint.label(),
            source,
        })
}

fn acquire_outer_exclusive(
    paths: &IndexPaths,
    deadline: Instant,
    cancelled: impl FnMut() -> bool,
) -> Result<IndexLease, RebuildError> {
    // An existing namespace is never repaired: its permanent lock must already
    // exist, otherwise acquisition fails closed. The existing-vs-initial
    // decision itself lives in `IndexLease` so every lifecycle owner (rebuild,
    // forced sync migration) takes the SAME one outer capability.
    Ok(IndexLease::acquire_or_create_exclusive(
        paths, deadline, cancelled,
    )?)
}

/// Remove only the v2 `codegraph.db`, `-wal`, and `-shm`. The legacy sibling is
/// never a target, and no other namespace child is touched.
fn remove_database_files(paths: &IndexPaths, lease: &IndexLease) -> Result<(), RebuildError> {
    let db = paths.current_db();
    for suffix in ["", "-wal", "-shm"] {
        let mut native = db.as_os_str().to_os_string();
        native.push(suffix);
        let path = PathBuf::from(native);
        lease.validate_exclusive(paths)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(RebuildError::RemoveDatabase { path, source }),
        }
    }
    Ok(())
}

fn remove_tombstone(paths: &IndexPaths, fault: &mut impl RebuildFault) -> Result<(), RebuildError> {
    let path = paths.tombstone();
    match fault.remove_tombstone(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RebuildError::RemoveTombstone { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CURRENT_EXTRACTION_VERSION, EXTRACTION_VERSION_KEY, ExtractionStatus};
    use std::time::Duration;

    struct TempProject(PathBuf);

    impl TempProject {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "codegraph-rebuild-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("create rebuild unit project");
            Self(path.canonicalize().expect("canonicalize rebuild project"))
        }

        fn paths(&self) -> IndexPaths {
            IndexPaths::resolve(&self.0, None).expect("resolve rebuild unit paths")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn deadline() -> Instant {
        Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("rebuild unit deadline")
    }

    /// Bounded deadline for refusal probes. It is NOT timing evidence: every
    /// assertion using it also pins the classified state, and a lease-contention
    /// refusal is rejected outright in [`assert_unreadable`].
    fn short_deadline() -> Instant {
        Instant::now()
            .checked_add(Duration::from_millis(50))
            .expect("rebuild unit short deadline")
    }

    struct FailAt(RebuildCheckpoint);

    impl RebuildFault for FailAt {
        fn at(&mut self, checkpoint: RebuildCheckpoint) -> io::Result<()> {
            if checkpoint == self.0 {
                Err(io::Error::other(format!(
                    "injected rebuild fault at {checkpoint:?}"
                )))
            } else {
                Ok(())
            }
        }
    }

    struct FailTombstoneRemoval;

    impl RebuildFault for FailTombstoneRemoval {
        fn at(&mut self, _checkpoint: RebuildCheckpoint) -> io::Result<()> {
            Ok(())
        }

        fn remove_tombstone(&mut self, _path: &std::path::Path) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected failure at tombstone removal operation",
            ))
        }
    }

    struct PublishUninitializedBeforeAuthorization {
        paths: IndexPaths,
    }

    impl RebuildFault for PublishUninitializedBeforeAuthorization {
        fn at(&mut self, checkpoint: RebuildCheckpoint) -> io::Result<()> {
            if checkpoint != RebuildCheckpoint::BeforeWriteAuthorization {
                return Ok(());
            }
            let record = serde_json::json!({
                "sequence": 0,
                "storageProtocol": crate::CURRENT_STORAGE_PROTOCOL,
                "extractionVersion": CURRENT_EXTRACTION_VERSION,
                "phase": "uninitialized",
                "projectIdentity": self.paths.project_identity(),
                "checksum": crate::checksum_hex(
                    0,
                    crate::CURRENT_STORAGE_PROTOCOL,
                    CURRENT_EXTRACTION_VERSION,
                    "uninitialized",
                    self.paths.project_identity(),
                ),
            });
            std::fs::write(
                &self.paths.state_slots()[0],
                serde_json::to_vec(&record).expect("serialize test state"),
            )
        }
    }

    /// Replaces the fixed permanent lock immediately before the `phase=current`
    /// publication, so the publication itself fails AFTER the database was fully
    /// finalized and closed.
    struct ReplaceLockBeforeCurrent {
        lock: PathBuf,
        displaced: PathBuf,
    }

    struct ReplaceLockAt {
        checkpoint: RebuildCheckpoint,
        lock: PathBuf,
        displaced: PathBuf,
    }

    impl RebuildFault for ReplaceLockAt {
        fn at(&mut self, checkpoint: RebuildCheckpoint) -> io::Result<()> {
            if checkpoint == self.checkpoint {
                std::fs::rename(&self.lock, &self.displaced)?;
                std::fs::write(&self.lock, b"replacement at mutation boundary")?;
            }
            Ok(())
        }
    }

    impl RebuildFault for ReplaceLockBeforeCurrent {
        fn at(&mut self, checkpoint: RebuildCheckpoint) -> io::Result<()> {
            if checkpoint == RebuildCheckpoint::BeforeCurrentPublication {
                std::fs::rename(&self.lock, &self.displaced)?;
                std::fs::write(&self.lock, b"late replacement")?;
            }
            Ok(())
        }
    }

    fn build_graph(store: &Store) {
        store
            .set_project_metadata("indexed_with_version", "test")
            .expect("write a metadata row into the rebuild target");
    }

    fn sidecars(paths: &IndexPaths) -> Vec<PathBuf> {
        let db = paths.current_db();
        ["-wal", "-shm"]
            .into_iter()
            .map(|suffix| {
                let mut native = db.as_os_str().to_os_string();
                native.push(suffix);
                PathBuf::from(native)
            })
            .filter(|path| path.exists())
            .collect()
    }

    /// Fail-closed with NO writer holding the lease: the read gate must refuse on
    /// state/artifact grounds, not merely because acquisition timed out.
    fn assert_unreadable(paths: &IndexPaths, context: &str) {
        let error = Store::open_for_read(paths, short_deadline(), || false)
            .err()
            .unwrap_or_else(|| panic!("{context}: an unfinished rebuild must not be readable"));
        assert!(
            !matches!(error, StoreError::Lease(_)),
            "{context}: refusal must be a state/artifact refusal, not lease contention: {error}"
        );
        assert_ne!(
            Store::extraction_status(paths),
            ExtractionStatus::Current,
            "{context}: an unfinished rebuild must never classify Current"
        );
    }

    /// Fail-closed while the rebuild still holds its exclusive lease: any refusal
    /// (state or bounded shared-acquisition timeout) is acceptable, but the state
    /// must not be Current.
    fn assert_unreadable_under_writer(paths: &IndexPaths, context: &str) {
        assert!(
            Store::open_for_read(paths, short_deadline(), || false).is_err(),
            "{context}: a building namespace must never be readable"
        );
        assert_ne!(
            Store::extraction_status(paths),
            ExtractionStatus::Current,
            "{context}: a building namespace must never classify Current"
        );
    }

    /// Publish the authoritative `uninitialized` slot and then the tombstone, in
    /// the exact order `uninit --force` uses, so the staged residue is a genuine
    /// interrupted-uninit namespace rather than a contradictory Current+tombstone.
    fn stage_interrupted_uninit(paths: &IndexPaths) {
        let lease = IndexLease::acquire_exclusive_existing(paths, deadline(), || false)
            .expect("acquire exclusive to stage an interrupted uninit");
        publish_index_state(paths, &lease, StatePhase::Uninitialized)
            .expect("publish the uninitialized slot");
        std::fs::write(paths.tombstone(), b"uninitialized").expect("publish the tombstone");
        drop(lease);
        assert_eq!(
            Store::extraction_status(paths),
            ExtractionStatus::Uninitialized
        );
    }

    fn run_successful_rebuild(paths: &IndexPaths, kind: RebuildKind) {
        let rebuild = begin_full_rebuild(paths, kind, deadline(), || false)
            .expect("begin a destructive rebuild");
        let rebuild = rebuild.open_store().expect("open the rebuild target");
        build_graph(&rebuild);
        rebuild.finish().expect("finish the rebuild");
    }

    #[test]
    fn successful_rebuild_publishes_readable_current_after_every_finalization_step() {
        let project = TempProject::new("happy");
        let paths = project.paths();

        let rebuild = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
            .expect("begin the initial rebuild");
        // `phase=building` is durable before the graph exists, and the namespace
        // is not readable while it builds.
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION
            }
        );
        assert_unreadable_under_writer(&paths, "mid-rebuild");

        let rebuild = rebuild.open_store().expect("open the rebuild target");
        build_graph(&rebuild);
        rebuild.finish().expect("finish the rebuild");

        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
        assert!(
            sidecars(&paths).is_empty(),
            "the final checkpoint/compaction/close pipeline must satisfy the sidecar-free Current contract: {:?}",
            sidecars(&paths)
        );
        let store = Store::open_for_read(&paths, deadline(), || false)
            .expect("a finalized namespace must be readable through the state gate");
        assert_eq!(
            store
                .get_project_metadata(EXTRACTION_VERSION_KEY)
                .expect("read the extraction stamp"),
            Some(CURRENT_EXTRACTION_VERSION.to_string()),
            "the stamp must be checkpointed into the main database file"
        );
    }

    #[test]
    fn every_rebuild_fault_leaves_building_or_missing_and_unreadable() {
        let pre_current = [
            RebuildCheckpoint::BeforeWriteAuthorization,
            RebuildCheckpoint::BuildingPublished,
            RebuildCheckpoint::BeforeDatabaseRemoval,
            RebuildCheckpoint::DatabaseRemoved,
            RebuildCheckpoint::BeforePragmasRestored,
            RebuildCheckpoint::PragmasRestored,
            RebuildCheckpoint::BeforeCompaction,
            RebuildCheckpoint::Compacted,
            RebuildCheckpoint::BeforeVersionStamp,
            RebuildCheckpoint::VersionStamped,
            RebuildCheckpoint::BeforeStampCheckpoint,
            RebuildCheckpoint::StampCheckpointed,
            RebuildCheckpoint::BeforeConnectionClose,
            RebuildCheckpoint::ConnectionClosed,
            RebuildCheckpoint::BeforeCurrentPublication,
        ];

        for checkpoint in pre_current {
            let project = TempProject::new("fault");
            let paths = project.paths();
            let mut fault = FailAt(checkpoint);

            let begun = begin_full_rebuild_with(
                &paths,
                RebuildKind::ExplicitInit,
                deadline(),
                || false,
                &mut fault,
            );
            match begun {
                Err(error) => {
                    assert!(
                        matches!(error, RebuildError::Interrupted { .. }),
                        "fault={checkpoint:?} must interrupt begin: {error}"
                    );
                }
                Ok(rebuild) => {
                    let rebuild = rebuild.open_store().expect("open the rebuild target");
                    build_graph(&rebuild);
                    let error = rebuild
                        .finish_with(&mut fault)
                        .expect_err("the injected fault must interrupt finalization");
                    assert!(
                        matches!(error, RebuildError::Interrupted { .. }),
                        "fault={checkpoint:?} must interrupt finish: {error}"
                    );
                }
            }

            let status = Store::extraction_status(&paths);
            assert!(
                matches!(
                    status,
                    ExtractionStatus::Building { .. } | ExtractionStatus::Missing
                ),
                "fault={checkpoint:?} must leave Building or Missing, got {status:?}"
            );
            assert_unreadable(&paths, &format!("fault={checkpoint:?}"));
        }
    }

    #[test]
    fn faults_at_or_after_current_publication_have_already_published_current() {
        for checkpoint in [
            RebuildCheckpoint::CurrentPublished,
            RebuildCheckpoint::AfterTombstoneRemoval,
        ] {
            let project = TempProject::new("post-current");
            let paths = project.paths();
            let mut fault = FailAt(checkpoint);

            let rebuild = begin_full_rebuild_with(
                &paths,
                RebuildKind::ExplicitInit,
                deadline(),
                || false,
                &mut fault,
            )
            .expect("begin the rebuild");
            let rebuild = rebuild.open_store().expect("open the rebuild target");
            build_graph(&rebuild);
            let error = rebuild
                .finish_with(&mut fault)
                .expect_err("the injected fault must be reported");
            assert!(matches!(error, RebuildError::Interrupted { .. }), "{error}");

            // Current is published LAST, so a fault at/after it observes Current:
            // the ordering guarantee is that nothing before it can be readable.
            assert_eq!(
                Store::extraction_status(&paths),
                ExtractionStatus::Current,
                "fault={checkpoint:?}"
            );
        }
    }

    #[test]
    fn explicit_init_recovery_publication_faults_leave_only_protocol_valid_states() {
        for (checkpoint, expected_step, expected_status) in [
            (
                RebuildCheckpoint::BuildingPublished,
                "publish phase=building",
                ExtractionStatus::Building {
                    built: CURRENT_EXTRACTION_VERSION,
                },
            ),
            (
                RebuildCheckpoint::CurrentPublished,
                "publish phase=current",
                ExtractionStatus::Current,
            ),
        ] {
            let project = TempProject::new("explicit-init-recovery-fault");
            let paths = project.paths();
            run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
            stage_interrupted_uninit(&paths);
            let mut fault = FailAt(checkpoint);

            let begun = begin_full_rebuild_with(
                &paths,
                RebuildKind::ExplicitInit,
                deadline(),
                || false,
                &mut fault,
            );
            let error = match begun {
                Err(error) => error,
                Ok(rebuild) => {
                    let rebuild = rebuild
                        .open_store()
                        .expect("open explicit-init recovery writer");
                    build_graph(&rebuild);
                    rebuild
                        .finish_with(&mut fault)
                        .expect_err("publication checkpoint must interrupt recovery")
                }
            };
            match error {
                RebuildError::Interrupted { step, source } => {
                    assert_eq!(step, expected_step, "checkpoint={checkpoint:?}");
                    assert_eq!(source.kind(), io::ErrorKind::Other);
                }
                other => panic!("checkpoint={checkpoint:?}: unexpected error {other:?}"),
            }

            assert_eq!(
                Store::extraction_status(&paths),
                expected_status,
                "checkpoint={checkpoint:?} must publish a complete protocol state"
            );
            assert!(
                paths.tombstone().is_file(),
                "checkpoint={checkpoint:?}: recovery interruption retains tombstone"
            );
            assert!(
                paths.state_slots().iter().all(|slot| slot.is_file()),
                "checkpoint={checkpoint:?}: both crash-safe slots remain present"
            );

            run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
            assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
            assert!(
                !paths.tombstone().exists(),
                "a later successful explicit init removes the retained tombstone"
            );
        }
    }

    #[test]
    fn failed_current_publication_after_finalization_never_becomes_readable_current() {
        let project = TempProject::new("publish-fails");
        let paths = project.paths();
        let displaced = paths.current_root().join("displaced-index.lock");
        let mut fault = ReplaceLockBeforeCurrent {
            lock: paths.permanent_lock(),
            displaced,
        };

        let rebuild = begin_full_rebuild_with(
            &paths,
            RebuildKind::ExplicitInit,
            deadline(),
            || false,
            &mut fault,
        )
        .expect("begin the rebuild");
        let rebuild = rebuild.open_store().expect("open the rebuild target");
        build_graph(&rebuild);
        let error = rebuild
            .finish_with(&mut fault)
            .expect_err("a replaced permanent lock must fail the Current publication");
        assert!(
            matches!(
                error,
                RebuildError::Publish(StatePublishError::Lease(
                    IndexLeaseValidationError::PermanentLockChanged { .. }
                ))
            ),
            "unexpected error: {error}"
        );

        // The database WAS finalized and closed before the publication attempt,
        // yet the namespace must not be reported as a readable Current.
        assert!(
            sidecars(&paths).is_empty(),
            "the final connection must already be closed: {:?}",
            sidecars(&paths)
        );
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION
            },
            "a failed Current publication must leave phase=building"
        );
        assert_unreadable(&paths, "failed Current publication");
    }

    #[test]
    fn stale_lease_is_rejected_at_every_destructive_and_finalization_mutation_boundary() {
        for checkpoint in [
            RebuildCheckpoint::BeforeDatabaseRemoval,
            RebuildCheckpoint::BeforePragmasRestored,
            RebuildCheckpoint::BeforeCompaction,
            RebuildCheckpoint::BeforeVersionStamp,
            RebuildCheckpoint::BeforeStampCheckpoint,
            RebuildCheckpoint::BeforeConnectionClose,
            RebuildCheckpoint::BeforeCurrentPublication,
            RebuildCheckpoint::BeforeTombstoneRemoval,
        ] {
            let project = TempProject::new("stale-boundary");
            let paths = project.paths();
            let displaced = paths.current_root().join(format!(
                "displaced-{}.lock",
                checkpoint.label().replace(' ', "-")
            ));
            let mut fault = ReplaceLockAt {
                checkpoint,
                lock: paths.permanent_lock(),
                displaced,
            };

            let begun = begin_full_rebuild_with(
                &paths,
                RebuildKind::ExplicitInit,
                deadline(),
                || false,
                &mut fault,
            );
            let error = match begun {
                Err(error) => error,
                Ok(rebuild) => {
                    let rebuild = rebuild.open_store().expect("open the final writer");
                    build_graph(&rebuild);
                    if checkpoint == RebuildCheckpoint::BeforeTombstoneRemoval {
                        std::fs::write(paths.tombstone(), b"retained")
                            .expect("stage tombstone for the removal boundary");
                    }
                    rebuild
                        .finish_with(&mut fault)
                        .expect_err("a replaced lock must reject the next mutation")
                }
            };
            assert!(
                matches!(
                    error,
                    RebuildError::LeaseValidation(
                        IndexLeaseValidationError::PermanentLockChanged { .. }
                    ) | RebuildError::Store(StoreError::LeaseValidation(
                        IndexLeaseValidationError::PermanentLockChanged { .. }
                    )) | RebuildError::Publish(StatePublishError::Lease(
                        IndexLeaseValidationError::PermanentLockChanged { .. }
                    ))
                ),
                "checkpoint={checkpoint:?}: unexpected stale-lease result: {error}"
            );
            if checkpoint == RebuildCheckpoint::BeforeTombstoneRemoval {
                assert!(
                    paths.tombstone().exists(),
                    "stale authority must be rejected before tombstone removal"
                );
            } else {
                assert_ne!(
                    Store::extraction_status(&paths),
                    ExtractionStatus::Current,
                    "checkpoint={checkpoint:?}: stale authority must not publish Current"
                );
            }
        }
    }

    #[test]
    fn stale_lease_is_rejected_immediately_before_write_capable_sqlite_open() {
        let project = TempProject::new("stale-write-open");
        let paths = project.paths();
        let rebuild = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
            .expect("begin rebuild before stale-open injection");
        let displaced = paths.current_root().join("displaced-before-open.lock");
        let error = rebuild
            .open_store_with(|| {
                std::fs::rename(paths.permanent_lock(), &displaced)
                    .expect("displace fixed lock before SQLite open");
                std::fs::write(paths.permanent_lock(), b"replacement before SQLite open")
                    .expect("install replacement lock");
            })
            .expect_err("stale capability must reject the write-capable open");
        assert!(
            matches!(
                error,
                RebuildError::Store(StoreError::LeaseValidation(
                    IndexLeaseValidationError::PermanentLockChanged { .. }
                ))
            ),
            "unexpected stale-open error: {error}"
        );
        assert!(
            !paths.current_db().exists(),
            "revalidation must happen immediately before SQLite creates the DB"
        );
    }

    #[test]
    fn dropping_an_unfinished_rebuild_never_publishes_current() {
        let project = TempProject::new("drop");
        let paths = project.paths();

        {
            let rebuild =
                begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
                    .expect("begin the rebuild");
            let rebuild = rebuild.open_store().expect("open the rebuild target");
            rebuild
                .set_bulk_index_pragmas()
                .expect("enable bulk pragmas before emergency cleanup");
            build_graph(&rebuild);
            drop(rebuild);
        }

        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION
            },
            "Drop is emergency cleanup only and must never publish Current"
        );
        assert!(
            sidecars(&paths).is_empty(),
            "Drop must best-effort checkpoint and close its writer: {:?}",
            sidecars(&paths)
        );
        assert_unreadable(&paths, "dropped rebuild");
    }

    #[test]
    fn emergency_cleanup_close_is_explicitly_state_gated() {
        let project = TempProject::new("drop-close-gate");
        let paths = project.paths();
        let mut rebuild =
            begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
                .expect("begin the rebuild")
                .open_store()
                .expect("open the rebuild target");
        rebuild
            .set_bulk_index_pragmas()
            .expect("enable bulk pragmas before emergency cleanup");
        build_graph(&rebuild);

        let displaced = paths
            .current_root()
            .join("displaced-before-drop-close.lock");
        let result = rebuild
            .emergency_cleanup_with(|| {
                // Deterministically replace the fixed lock AFTER emergency pragma
                // restoration/compaction and immediately BEFORE the shared
                // Store::close path validates. No sleep or race inference is used.
                std::fs::rename(paths.permanent_lock(), &displaced)
                    .expect("displace fixed lock before emergency close");
                std::fs::write(
                    paths.permanent_lock(),
                    b"replacement before emergency close",
                )
                .expect("install replacement lock before emergency close");
            })
            .expect("the active rebuild owns one writer to close");
        let error = result.expect_err("the explicit close gate must reject stale authority");
        assert!(
            matches!(
                error,
                StoreError::LeaseValidation(IndexLeaseValidationError::PermanentLockChanged { .. })
            ),
            "unexpected emergency-close result: {error}"
        );
        // The consumed Store necessarily falls back to Rust Drop after this
        // validation error. This assertion proves the explicit close boundary
        // was capability-gated; it does NOT claim validation+close atomicity or
        // that resource release can be suppressed after ownership is consumed.
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            },
            "emergency close rejection must not publish Current"
        );
        assert!(
            !paths.tombstone().exists(),
            "emergency cleanup must have no tombstone-removal or creation path"
        );
    }

    #[test]
    fn reindex_refusal_uses_the_status_accepted_by_under_lease_authorization() {
        let project = TempProject::new("authorization-status");
        let paths = project.paths();
        let lease = IndexLease::create_exclusive(&paths, deadline(), || false)
            .expect("stage a valid empty namespace");
        drop(lease);
        let mut fault = PublishUninitializedBeforeAuthorization {
            paths: paths.clone(),
        };

        let error = begin_full_rebuild_with(
            &paths,
            RebuildKind::Reindex,
            deadline(),
            || false,
            &mut fault,
        )
        .expect_err("Reindex must refuse the status accepted by authorization");
        assert!(
            matches!(
                error,
                RebuildError::UninitializedRequiresExplicitInit { .. }
            ),
            "unexpected error: {error}"
        );
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Uninitialized
        );
        assert!(
            !paths.current_db().exists(),
            "refusal from accepted authorization must precede DB mutation"
        );
    }

    #[test]
    fn only_successful_explicit_init_removes_the_tombstone() {
        let project = TempProject::new("tombstone-reindex");
        let paths = project.paths();
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
        stage_interrupted_uninit(&paths);

        // An ordinary reindex is not authorized for an interrupted-uninit
        // namespace at all, so it can never reach tombstone removal.
        let error = begin_full_rebuild(&paths, RebuildKind::Reindex, deadline(), || false)
            .expect_err("a reindex must refuse an interrupted-uninit namespace");
        assert!(
            matches!(
                error,
                RebuildError::UninitializedRequiresExplicitInit { .. }
            ),
            "unexpected error: {error}"
        );
        assert!(
            paths.tombstone().exists(),
            "a refused reindex must never remove the tombstone"
        );

        // Explicit init: the tombstone is removed, but only after full success.
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
        assert!(
            !paths.tombstone().exists(),
            "a successful explicit init must remove the tombstone"
        );
        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    }

    #[test]
    fn a_successful_reindex_preserves_an_unrelated_tombstone_free_namespace() {
        // Complementary to the interrupted-uninit case: an ordinary reindex over a
        // clean Current namespace never touches the tombstone path either.
        let project = TempProject::new("tombstone-clean");
        let paths = project.paths();
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
        run_successful_rebuild(&paths, RebuildKind::Reindex);
        assert!(
            !paths.tombstone().exists(),
            "a reindex must not create a tombstone"
        );
        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    }

    #[test]
    fn an_earlier_fault_never_removes_the_tombstone() {
        let project = TempProject::new("tombstone-fault");
        let paths = project.paths();
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
        stage_interrupted_uninit(&paths);

        let mut fault = FailAt(RebuildCheckpoint::ConnectionClosed);
        let rebuild = begin_full_rebuild_with(
            &paths,
            RebuildKind::ExplicitInit,
            deadline(),
            || false,
            &mut fault,
        )
        .expect("begin the rebuild");
        let rebuild = rebuild.open_store().expect("open the rebuild target");
        build_graph(&rebuild);
        rebuild
            .finish_with(&mut fault)
            .expect_err("the injected fault must interrupt finalization");

        assert!(
            paths.tombstone().exists(),
            "a failed rebuild must never remove the tombstone"
        );
    }

    #[test]
    fn tombstone_removal_failure_is_failed_closed_and_explicit_init_retry_recovers() {
        let project = TempProject::new("tombstone-remove-failure");
        let paths = project.paths();
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
        stage_interrupted_uninit(&paths);

        let rebuild = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
            .expect("begin explicit recovery");
        let rebuild = rebuild.open_store().expect("open recovery writer");
        build_graph(&rebuild);
        let error = rebuild
            .finish_with(&mut FailTombstoneRemoval)
            .expect_err("the removal operation itself must fail");
        assert!(
            matches!(
                error,
                RebuildError::RemoveTombstone { ref source, .. }
                    if source.kind() == io::ErrorKind::PermissionDenied
            ),
            "unexpected error: {error}"
        );
        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
        assert!(paths.tombstone().exists());
        assert!(
            matches!(
                Store::open_for_read(&paths, deadline(), || false),
                Err(StoreError::CurrentTombstoned { .. })
            ),
            "Current+tombstone must stay failed closed, never silently healthy"
        );

        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);
        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
        assert!(!paths.tombstone().exists());
        Store::open_for_read(&paths, deadline(), || false)
            .expect("an explicit-init retry must recover Current+tombstone residue");
    }

    #[test]
    fn rebuild_retains_one_exclusive_lease_and_never_reacquires() {
        let project = TempProject::new("lease");
        let paths = project.paths();
        let rebuild = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
            .expect("begin the rebuild");
        assert!(rebuild.lease.is_exclusive());
        // A competing writer cannot acquire while the rebuild holds the lease,
        // proving the capability is retained rather than released between steps.
        let competing = IndexLease::acquire_exclusive_existing(
            &paths,
            Instant::now() + Duration::from_millis(50),
            || false,
        );
        assert!(
            matches!(competing, Err(IndexLeaseError::TimedOut { .. })),
            "a retained exclusive lease must block a competing writer: {competing:?}"
        );

        let rebuild = rebuild.open_store().expect("open the rebuild target");
        build_graph(&rebuild);
        rebuild.finish().expect("finish the rebuild");

        // Only after the rebuild handle is gone may another writer acquire.
        IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
            .expect("the lease is released once the finished rebuild is dropped");
    }

    #[test]
    fn existing_root_without_a_permanent_lock_fails_closed() {
        let project = TempProject::new("lockless");
        let paths = project.paths();
        std::fs::create_dir_all(paths.current_root()).expect("stage a lockless root");

        let error = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
            .expect_err("a lockless existing namespace must never be repaired");
        assert!(
            matches!(
                error,
                RebuildError::Lease(IndexLeaseError::LockNotFound { .. })
            ),
            "unexpected error: {error}"
        );
        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Missing);
        assert!(
            !paths.current_db().exists(),
            "a refused rebuild must not create a database"
        );
    }

    /// Take the incremental-sync authorization the way a real sync does: ONE
    /// outer exclusive lease, one classification under it.
    fn incremental_sync_authorization(
        paths: &IndexPaths,
    ) -> Result<crate::connection::StoreWriteAuthorization, StoreError> {
        let lease = IndexLease::acquire_or_create_exclusive(paths, deadline(), || false)
            .expect("acquire the one outer exclusive lease");
        match Store::open_for_write(paths, lease, StoreWritePurpose::IncrementalSync)? {
            StoreWriteOpen::FullRebuildRequired(authorization) => Ok(authorization),
            other => panic!("expected an escalation authorization, got {other:?}"),
        }
    }

    /// A sync that classified `Outdated` escalates through the SAME retained
    /// lease: `resume_full_rebuild` acquires nothing, publishes `phase=building`
    /// before touching the database, and finalizes to a readable `Current`.
    #[test]
    fn resume_full_rebuild_migrates_outdated_under_the_retained_lease() {
        let project = TempProject::new("resume-outdated");
        let paths = project.paths();
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);

        // Stage an OLDER built version through the accepted publisher path, then
        // rewrite only the version field of the authoritative slot.
        let outdated = CURRENT_EXTRACTION_VERSION - 1;
        let record = serde_json::json!({
            "sequence": 99,
            "storageProtocol": crate::CURRENT_STORAGE_PROTOCOL,
            "extractionVersion": outdated,
            "phase": "current",
            "projectIdentity": paths.project_identity(),
            "checksum": crate::checksum_hex(
                99,
                crate::CURRENT_STORAGE_PROTOCOL,
                outdated,
                "current",
                paths.project_identity(),
            ),
        });
        std::fs::write(
            &paths.state_slots()[0],
            serde_json::to_vec(&record).expect("serialize the outdated slot"),
        )
        .expect("stage the outdated slot");
        let _ = std::fs::remove_file(&paths.state_slots()[1]);
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Outdated { built: outdated }
        );

        let authorization =
            incremental_sync_authorization(&paths).expect("Outdated must authorize escalation");
        assert!(
            authorization.retains_exclusive_lease(),
            "the escalation authorization must still own the one outer exclusive lease"
        );
        let rebuild = resume_full_rebuild(&paths, authorization).expect("resume the full rebuild");
        // Publication precedes destruction, so an interruption here is Building.
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            }
        );
        assert!(
            !paths.current_db().exists(),
            "the outdated database must be removed after phase=building is durable"
        );
        let rebuild = rebuild.open_store().expect("open the migration writer");
        build_graph(&rebuild);
        rebuild.finish().expect("finalize the migration");

        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
        Store::open_for_read(&paths, deadline(), || false)
            .expect("a finalized migration must be readable");
    }

    /// The incremental-sync gate is state-directed: only Missing/Outdated/
    /// Building escalate. A corroborated Current opens the incremental writer,
    /// and an interrupted-uninit namespace is refused outright.
    #[test]
    fn incremental_sync_gate_escalates_only_the_migratable_states() {
        let missing = TempProject::new("gate-missing");
        let missing_paths = missing.paths();
        incremental_sync_authorization(&missing_paths)
            .expect("Missing must authorize escalation, not a row-level update");

        let current = TempProject::new("gate-current");
        let current_paths = current.paths();
        run_successful_rebuild(&current_paths, RebuildKind::ExplicitInit);
        let lease = IndexLease::acquire_or_create_exclusive(&current_paths, deadline(), || false)
            .expect("acquire exclusive over the Current namespace");
        match Store::open_for_write(&current_paths, lease, StoreWritePurpose::IncrementalSync)
            .expect("Current must authorize an incremental writer")
        {
            StoreWriteOpen::Current(_) => {}
            other => panic!("Current must stay incremental, got {other:?}"),
        }

        let building = TempProject::new("gate-building");
        let building_paths = building.paths();
        let begun = begin_full_rebuild(&building_paths, RebuildKind::Reindex, deadline(), || false)
            .expect("stage an interrupted Building namespace");
        drop(begun);
        incremental_sync_authorization(&building_paths)
            .expect("a recoverable Building must authorize escalation");

        let uninit = TempProject::new("gate-uninit");
        let uninit_paths = uninit.paths();
        run_successful_rebuild(&uninit_paths, RebuildKind::ExplicitInit);
        stage_interrupted_uninit(&uninit_paths);
        let error = incremental_sync_authorization(&uninit_paths)
            .expect_err("an uninitialized namespace is reserved for an explicit init");
        assert!(
            matches!(
                error,
                StoreError::WritePurposeRejected {
                    purpose: StoreWritePurpose::IncrementalSync,
                    status: ExtractionStatus::Uninitialized,
                }
            ),
            "unexpected refusal: {error}"
        );
    }

    /// `resume_full_rebuild` refuses an authorization whose retained status it
    /// did not accept for escalation, so no caller can smuggle a Current or
    /// Uninitialized namespace into a destructive migration.
    #[test]
    fn resume_full_rebuild_refuses_a_non_migratable_authorization() {
        let project = TempProject::new("resume-refuses");
        let paths = project.paths();
        run_successful_rebuild(&paths, RebuildKind::ExplicitInit);

        // A FullRebuild-purpose authorization over a Current namespace is valid
        // for `begin_full_rebuild` but is NOT a sync escalation.
        let lease = IndexLease::acquire_or_create_exclusive(&paths, deadline(), || false)
            .expect("acquire exclusive");
        let authorization =
            match Store::open_for_write(&paths, lease, StoreWritePurpose::FullRebuild)
                .expect("Current authorizes a full rebuild")
            {
                StoreWriteOpen::FullRebuildRequired(authorization) => authorization,
                other => panic!("unexpected open result {other:?}"),
            };
        let before = std::fs::read(paths.current_db()).expect("read the Current database bytes");
        let error = resume_full_rebuild(&paths, authorization)
            .expect_err("a Current authorization must not resume a migration");
        assert!(
            matches!(
                error,
                RebuildError::Store(StoreError::WritePurposeRejected {
                    status: ExtractionStatus::Current,
                    ..
                })
            ),
            "unexpected refusal: {error}"
        );
        assert_eq!(
            std::fs::read(paths.current_db()).expect("re-read the database bytes"),
            before,
            "a refused resume must not change one database byte"
        );
        assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    }

    #[test]
    fn opening_the_final_writer_consumes_the_unopened_capability() {
        let project = TempProject::new("single-writer");
        let paths = project.paths();
        let rebuild = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
            .expect("begin the rebuild");
        let rebuild = rebuild.open_store().expect("open the one final writer");
        build_graph(&rebuild);
        // `FullRebuild::open_store(self)` consumes the unopened capability. This
        // is a compile-time ownership guarantee: there is no remaining value on
        // which a second open can be invoked.
        drop(rebuild);
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            },
            "refusing a second writer must never publish Current"
        );
    }
}
