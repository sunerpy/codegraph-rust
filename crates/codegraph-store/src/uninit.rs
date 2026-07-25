//! Crash-recoverable `uninit --force` lifecycle.
//!
//! The state slot authenticates interrupted-uninit residue. Publish it under one
//! retained exclusive lease before creating the tombstone or deleting v2 bytes.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use codegraph_core::IndexPaths;
use thiserror::Error;

use crate::{
    ExtractionStatus, IndexLease, IndexLeaseError, IndexLeaseValidationError, StatePhase,
    StatePublishError, Store, StoreError, StoreWriteOpen, StoreWritePurpose, publish_index_state,
};

const TOMBSTONE_BYTES: &[u8] = b"uninitialized\n";

/// Result of one complete new-uninit or cleanup-continuation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UninitOutcome {
    /// Whether any configured/fixed legacy namespace remains untouched.
    pub legacy_index_present: bool,
}

/// Typed failures from the destructive uninit lifecycle.
#[derive(Debug, Error)]
pub enum UninitError {
    #[error("uninit is not authorized for index state {status}")]
    StateRejected { status: ExtractionStatus },
    #[error(transparent)]
    Lease(#[from] IndexLeaseError),
    #[error(transparent)]
    LeaseValidation(#[from] IndexLeaseValidationError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Publish(#[from] StatePublishError),
    #[error("failed to publish uninitialized tombstone {path}: {source}")]
    Tombstone {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("refusing to remove non-file v2 lifecycle child {path}")]
    UnsupportedChild { path: PathBuf },
    #[error("failed to remove v2 lifecycle child {path}: {source}")]
    RemoveChild {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("uninit interrupted after {step}: {source}")]
    Interrupted {
        step: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UninitCheckpoint {
    BeforeAuthorization,
    StatePublished,
    TombstoneEnsured,
    DatabaseRemoved,
    WalRemoved,
    ShmRemoved,
    ConfigRemoved,
    ExtensionConfigRemoved,
    DaemonPidRemoved,
    DaemonLogRemoved,
    DaemonSocketRemoved,
}

impl UninitCheckpoint {
    fn label(self) -> &'static str {
        match self {
            Self::BeforeAuthorization => "write authorization",
            Self::StatePublished => "uninitialized state publication",
            Self::TombstoneEnsured => "tombstone creation",
            Self::DatabaseRemoved => "database removal",
            Self::WalRemoved => "WAL removal",
            Self::ShmRemoved => "SHM removal",
            Self::ConfigRemoved => "config removal",
            Self::ExtensionConfigRemoved => "extension config removal",
            Self::DaemonPidRemoved => "daemon pid removal",
            Self::DaemonLogRemoved => "daemon log removal",
            Self::DaemonSocketRemoved => "daemon socket removal",
        }
    }
}

trait UninitFault {
    fn after(&mut self, checkpoint: UninitCheckpoint) -> io::Result<()>;
}

struct NoFault;

impl UninitFault for NoFault {
    fn after(&mut self, _checkpoint: UninitCheckpoint) -> io::Result<()> {
        Ok(())
    }
}

/// Start or continue `uninit --force` under one existing exclusive lease.
///
/// This never creates or repairs a namespace lock. The pre-probe is nonmutating;
/// Store reclassifies under the acquired lease before the first mutation.
pub fn uninit_index(
    paths: &IndexPaths,
    deadline: Instant,
    cancelled: impl FnMut() -> bool,
) -> Result<UninitOutcome, UninitError> {
    uninit_index_with(paths, deadline, cancelled, &mut NoFault)
}

fn uninit_index_with(
    paths: &IndexPaths,
    deadline: Instant,
    cancelled: impl FnMut() -> bool,
    fault: &mut impl UninitFault,
) -> Result<UninitOutcome, UninitError> {
    let visible = Store::extraction_status(paths);
    if !matches!(
        visible,
        ExtractionStatus::Current
            | ExtractionStatus::Building { .. }
            | ExtractionStatus::Uninitialized
    ) {
        return Err(UninitError::StateRejected { status: visible });
    }

    let lease = IndexLease::acquire_exclusive_existing(paths, deadline, cancelled)?;
    checkpoint(fault, UninitCheckpoint::BeforeAuthorization)?;
    let authorization =
        match Store::open_for_write(paths, lease.clone(), StoreWritePurpose::UninitContinuation)? {
            StoreWriteOpen::UninitContinuation(authorization) => authorization,
            other => unreachable!("uninit purpose returned unexpected Store open: {other:?}"),
        };
    if authorization.purpose() != StoreWritePurpose::UninitContinuation
        || !matches!(
            authorization.status(),
            ExtractionStatus::Current
                | ExtractionStatus::Building { .. }
                | ExtractionStatus::Uninitialized
        )
    {
        return Err(UninitError::StateRejected {
            status: authorization.status().clone(),
        });
    }

    publish_index_state(paths, &lease, StatePhase::Uninitialized)?;
    checkpoint(fault, UninitCheckpoint::StatePublished)?;
    ensure_tombstone(paths, &lease)?;
    checkpoint(fault, UninitCheckpoint::TombstoneEnsured)?;

    let db = paths.current_db();
    remove_child(paths, &lease, &db)?;
    checkpoint(fault, UninitCheckpoint::DatabaseRemoved)?;
    let wal = sqlite_sidecar_path(&db, "-wal");
    remove_child(paths, &lease, &wal)?;
    checkpoint(fault, UninitCheckpoint::WalRemoved)?;
    let shm = sqlite_sidecar_path(&db, "-shm");
    remove_child(paths, &lease, &shm)?;
    checkpoint(fault, UninitCheckpoint::ShmRemoved)?;

    for (path, boundary) in [
        (paths.config_toml(), UninitCheckpoint::ConfigRemoved),
        (
            paths.extension_config(),
            UninitCheckpoint::ExtensionConfigRemoved,
        ),
        (paths.daemon_pid(), UninitCheckpoint::DaemonPidRemoved),
        (paths.daemon_log(), UninitCheckpoint::DaemonLogRemoved),
        (paths.daemon_socket(), UninitCheckpoint::DaemonSocketRemoved),
    ] {
        remove_child(paths, &lease, &path)?;
        checkpoint(fault, boundary)?;
    }

    drop(authorization);
    drop(lease);
    Ok(UninitOutcome {
        legacy_index_present: paths.legacy_roots().iter().any(|path| path.exists()),
    })
}

/// Append SQLite's sidecar suffix to the native database pathname without a
/// Unicode rendering round-trip. Both Unix byte paths and Windows wide paths
/// remain lossless.
fn sqlite_sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    let mut native = db.as_os_str().to_os_string();
    native.push(suffix);
    PathBuf::from(native)
}

fn checkpoint(
    fault: &mut impl UninitFault,
    checkpoint: UninitCheckpoint,
) -> Result<(), UninitError> {
    fault
        .after(checkpoint)
        .map_err(|source| UninitError::Interrupted {
            step: checkpoint.label(),
            source,
        })
}

fn ensure_tombstone(paths: &IndexPaths, lease: &IndexLease) -> Result<(), UninitError> {
    lease.validate_exclusive(paths)?;
    let path = paths.tombstone();
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => return Ok(()),
        Ok(_) => return Err(UninitError::UnsupportedChild { path }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(UninitError::Tombstone { path, source }),
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| UninitError::Tombstone {
            path: path.clone(),
            source,
        })?;
    file.write_all(TOMBSTONE_BYTES)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|source| UninitError::Tombstone { path, source })
}

fn remove_child(paths: &IndexPaths, lease: &IndexLease, path: &Path) -> Result<(), UninitError> {
    lease.validate_exclusive(paths)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Err(UninitError::UnsupportedChild {
            path: path.to_path_buf(),
        }),
        Ok(_) => std::fs::remove_file(path).map_err(|source| UninitError::RemoveChild {
            path: path.to_path_buf(),
            source,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(UninitError::RemoveChild {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    use crate::{CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, checksum_hex, classify};

    struct TempProject(PathBuf);

    impl TempProject {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "codegraph-uninit-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock after epoch")
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("create uninit test project");
            Self(path.canonicalize().expect("canonicalize test project"))
        }

        fn paths(&self) -> IndexPaths {
            IndexPaths::resolve(&self.0, None).expect("resolve uninit paths")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    fn db_artifact(paths: &IndexPaths, suffix: &str) -> PathBuf {
        sqlite_sidecar_path(&paths.current_db(), suffix)
    }

    fn stage_building_with_all_residue(paths: &IndexPaths) {
        let lease = IndexLease::create_exclusive(paths, deadline(), || false)
            .expect("create Building namespace");
        publish_index_state(paths, &lease, StatePhase::Building).expect("publish Building fixture");
        for (path, bytes) in [
            (paths.current_db(), b"db residue".as_slice()),
            (db_artifact(paths, "-wal"), b"wal residue".as_slice()),
            (db_artifact(paths, "-shm"), b"shm residue".as_slice()),
            (paths.config_toml(), b"config residue".as_slice()),
            (paths.extension_config(), b"extension residue".as_slice()),
            (paths.daemon_pid(), b"pid residue".as_slice()),
            (paths.daemon_log(), b"log residue".as_slice()),
            (paths.daemon_socket(), b"socket residue".as_slice()),
        ] {
            std::fs::write(path, bytes).expect("write uninit residue fixture");
        }
        drop(lease);
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum SnapshotEntry {
        Directory,
        File(Vec<u8>),
        Symlink(PathBuf),
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, SnapshotEntry>) {
            let mut children = std::fs::read_dir(dir)
                .unwrap_or_else(|error| panic!("snapshot read_dir {}: {error}", dir.display()))
                .map(|entry| {
                    entry
                        .unwrap_or_else(|error| {
                            panic!("snapshot entry in {}: {error}", dir.display())
                        })
                        .path()
                })
                .collect::<Vec<_>>();
            children.sort();
            for path in children {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or_else(|error| panic!("snapshot strip {}: {error}", path.display()))
                    .to_path_buf();
                let metadata = std::fs::symlink_metadata(&path).unwrap_or_else(|error| {
                    panic!("snapshot metadata {}: {error}", path.display())
                });
                let ty = metadata.file_type();
                let entry = if ty.is_dir() {
                    SnapshotEntry::Directory
                } else if ty.is_file() {
                    SnapshotEntry::File(std::fs::read(&path).unwrap_or_else(|error| {
                        panic!("snapshot read {}: {error}", path.display())
                    }))
                } else if ty.is_symlink() {
                    SnapshotEntry::Symlink(std::fs::read_link(&path).unwrap_or_else(|error| {
                        panic!("snapshot read_link {}: {error}", path.display())
                    }))
                } else {
                    panic!("snapshot unsupported entry kind: {}", path.display());
                };
                assert!(out.insert(relative, entry).is_none());
                if ty.is_dir() {
                    walk(root, &path, out);
                }
            }
        }

        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn assert_unchanged(
        before: &BTreeMap<PathBuf, SnapshotEntry>,
        after: &BTreeMap<PathBuf, SnapshotEntry>,
    ) {
        let changed = before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(changed.is_empty(), "filesystem changed at {changed:?}");
    }

    struct FailAfter(UninitCheckpoint);

    impl UninitFault for FailAfter {
        fn after(&mut self, checkpoint: UninitCheckpoint) -> io::Result<()> {
            if checkpoint == self.0 {
                Err(io::Error::other("injected uninit interruption"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn interrupted_uninit_state_slot_is_recoverable_not_corrupt_fault_matrix() {
        for (boundary, expected_step, tombstone_present, removed_count) in [
            (
                UninitCheckpoint::StatePublished,
                "uninitialized state publication",
                false,
                0,
            ),
            (
                UninitCheckpoint::TombstoneEnsured,
                "tombstone creation",
                true,
                0,
            ),
            (
                UninitCheckpoint::DatabaseRemoved,
                "database removal",
                true,
                1,
            ),
            (UninitCheckpoint::WalRemoved, "WAL removal", true, 2),
            (UninitCheckpoint::ShmRemoved, "SHM removal", true, 3),
            (UninitCheckpoint::ConfigRemoved, "config removal", true, 4),
            (
                UninitCheckpoint::ExtensionConfigRemoved,
                "extension config removal",
                true,
                5,
            ),
            (
                UninitCheckpoint::DaemonPidRemoved,
                "daemon pid removal",
                true,
                6,
            ),
            (
                UninitCheckpoint::DaemonLogRemoved,
                "daemon log removal",
                true,
                7,
            ),
            (
                UninitCheckpoint::DaemonSocketRemoved,
                "daemon socket removal",
                true,
                8,
            ),
        ] {
            let project = TempProject::new("fault-matrix");
            let paths = project.paths();
            stage_building_with_all_residue(&paths);
            let legacy = project.0.join(".codegraph");
            std::fs::create_dir(&legacy).expect("create legacy namespace");
            std::fs::write(legacy.join("legacy.bin"), b"legacy bytes")
                .expect("write legacy proof bytes");
            let legacy_before = snapshot(&legacy);

            let error = uninit_index_with(&paths, deadline(), || false, &mut FailAfter(boundary))
                .expect_err("fault checkpoint must interrupt uninit");
            match error {
                UninitError::Interrupted { step, source } => {
                    assert_eq!(step, expected_step, "boundary={boundary:?}");
                    assert_eq!(source.kind(), io::ErrorKind::Other);
                }
                other => panic!("unexpected fault result at {boundary:?}: {other:?}"),
            }
            assert_eq!(
                Store::extraction_status(&paths),
                ExtractionStatus::Uninitialized,
                "boundary={boundary:?} must leave authenticated interrupted-uninit state"
            );
            assert!(paths.current_root().is_dir(), "boundary={boundary:?}");
            assert!(paths.permanent_lock().is_file());
            assert!(paths.state_slots().iter().all(|slot| slot.is_file()));
            assert_eq!(
                paths.tombstone().is_file(),
                tombstone_present,
                "unexpected tombstone frontier at {boundary:?}"
            );
            let artifacts = [
                paths.current_db(),
                db_artifact(&paths, "-wal"),
                db_artifact(&paths, "-shm"),
                paths.config_toml(),
                paths.extension_config(),
                paths.daemon_pid(),
                paths.daemon_log(),
                paths.daemon_socket(),
            ];
            for (index, artifact) in artifacts.iter().enumerate() {
                assert_eq!(
                    artifact.exists(),
                    index >= removed_count,
                    "unexpected deletion frontier at {boundary:?}: artifact #{index} {}",
                    artifact.display()
                );
            }
            assert_unchanged(&legacy_before, &snapshot(&legacy));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_database_sidecars_are_removed_without_touching_lossy_lookalikes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let outer = TempProject::new("non-utf8-sidecar");
        let project_path = outer.0.join(OsString::from_vec(b"project-\xff".to_vec()));
        std::fs::create_dir(&project_path).expect("create non-UTF-8 project");
        let project = TempProject(project_path.canonicalize().expect("canonicalize project"));
        let paths = project.paths();
        stage_building_with_all_residue(&paths);

        let native_wal = db_artifact(&paths, "-wal");
        let native_shm = db_artifact(&paths, "-shm");
        let lossy_wal = PathBuf::from(format!("{}-wal", paths.current_db().display()));
        assert_ne!(native_wal, lossy_wal, "fixture must expose lossy rendering");
        std::fs::create_dir_all(lossy_wal.parent().expect("lossy WAL parent"))
            .expect("create lossy-lookalike parent");
        std::fs::write(&lossy_wal, b"must survive").expect("write lossy-lookalike WAL");

        uninit_index(&paths, deadline(), || false).expect("uninit non-UTF-8 namespace");

        assert!(!native_wal.exists(), "true native WAL must be removed");
        assert!(!native_shm.exists(), "true native SHM must be removed");
        assert_eq!(
            std::fs::read(&lossy_wal).expect("read preserved lossy lookalike"),
            b"must survive"
        );
        assert_eq!(
            Store::extraction_status(&paths),
            ExtractionStatus::Uninitialized
        );
    }

    #[test]
    fn every_db_sidecar_tombstone_residue_combination_is_uninitialized() {
        for bits in 0_u8..16 {
            let project = TempProject::new("residue-matrix");
            let paths = project.paths();
            let lease = IndexLease::create_exclusive(&paths, deadline(), || false)
                .expect("create residue namespace");
            publish_index_state(&paths, &lease, StatePhase::Building).expect("publish Building");
            publish_index_state(&paths, &lease, StatePhase::Uninitialized)
                .expect("publish Uninitialized");
            drop(lease);

            for (bit, path) in [
                (0, paths.current_db()),
                (1, db_artifact(&paths, "-wal")),
                (2, db_artifact(&paths, "-shm")),
                (3, paths.tombstone()),
            ] {
                if bits & (1 << bit) != 0 {
                    std::fs::write(path, format!("residue bit {bit}"))
                        .expect("write residue combination");
                }
            }

            assert_eq!(
                Store::extraction_status(&paths),
                ExtractionStatus::Uninitialized,
                "bits={bits:04b}"
            );
            let status = Store::open_for_status(&paths, deadline(), || false)
                .expect("Uninitialized residue is typed status data");
            assert_eq!(status.status, Some(ExtractionStatus::Uninitialized));
            let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
                .expect("acquire continuation lease");
            assert!(matches!(
                Store::open_for_write(&paths, lease, StoreWritePurpose::UninitContinuation,),
                Ok(StoreWriteOpen::UninitContinuation(_))
            ));
        }
    }

    #[test]
    fn repeated_uninit_uses_monotonic_publication_and_preserves_both_slots() {
        let project = TempProject::new("continuation");
        let paths = project.paths();
        stage_building_with_all_residue(&paths);

        uninit_index(&paths, deadline(), || false).expect("first uninit pass");
        let first = classify(&paths);
        let first_authority = first
            .authoritative()
            .expect("first uninit has authority")
            .clone();
        let first_bytes =
            std::fs::read(&first_authority.path).expect("read first authoritative slot");
        std::fs::write(paths.current_db(), b"continued residue")
            .expect("stage continuation residue");

        uninit_index(&paths, deadline(), || false).expect("continued uninit pass");
        let second = classify(&paths);
        let second_authority = second.authoritative().expect("second uninit has authority");
        assert_eq!(second.status(), &ExtractionStatus::Uninitialized);
        assert_eq!(
            second_authority.record.sequence,
            first_authority.record.sequence + 1
        );
        assert_eq!(
            std::fs::read(&first_authority.path).expect("old authority remains readable"),
            first_bytes
        );
        assert!(paths.state_slots().iter().all(|slot| slot.is_file()));
        assert!(!paths.current_db().exists());
    }

    fn write_future_uninitialized(paths: &IndexPaths) {
        let sequence = 0;
        let protocol = CURRENT_STORAGE_PROTOCOL + 1;
        let extraction = CURRENT_EXTRACTION_VERSION + 1;
        let phase = "uninitialized";
        let checksum = checksum_hex(
            sequence,
            protocol,
            extraction,
            phase,
            paths.project_identity(),
        );
        std::fs::write(
            &paths.state_slots()[0],
            serde_json::to_vec(&serde_json::json!({
                "sequence": sequence,
                "storageProtocol": protocol,
                "extractionVersion": extraction,
                "phase": phase,
                "projectIdentity": paths.project_identity(),
                "checksum": checksum,
            }))
            .expect("serialize Future fixture"),
        )
        .expect("write Future fixture");
    }

    #[test]
    fn future_corrupt_owner_mismatch_and_missing_lock_are_byte_nonmutating() {
        for fixture in ["future", "corrupt", "owner-mismatch"] {
            let project = TempProject::new("refused");
            let paths = project.paths();
            let lease = IndexLease::create_exclusive(&paths, deadline(), || false)
                .expect("create refusal namespace");
            match fixture {
                "future" => write_future_uninitialized(&paths),
                "corrupt" => std::fs::write(&paths.state_slots()[0], b"not-json")
                    .expect("write Corrupt fixture"),
                "owner-mismatch" => {
                    let sequence = 0;
                    let owner = "0".repeat(64);
                    let checksum = checksum_hex(
                        sequence,
                        CURRENT_STORAGE_PROTOCOL,
                        CURRENT_EXTRACTION_VERSION,
                        "uninitialized",
                        &owner,
                    );
                    std::fs::write(
                        &paths.state_slots()[0],
                        serde_json::to_vec(&serde_json::json!({
                            "sequence": sequence,
                            "storageProtocol": CURRENT_STORAGE_PROTOCOL,
                            "extractionVersion": CURRENT_EXTRACTION_VERSION,
                            "phase": "uninitialized",
                            "projectIdentity": owner,
                            "checksum": checksum,
                        }))
                        .expect("serialize owner-mismatch fixture"),
                    )
                    .expect("write owner-mismatch fixture");
                }
                _ => unreachable!(),
            }
            drop(lease);
            let before = snapshot(&project.0);
            assert!(matches!(
                uninit_index(&paths, deadline(), || false),
                Err(UninitError::StateRejected { .. })
            ));
            assert_unchanged(&before, &snapshot(&project.0));
        }

        let project = TempProject::new("missing-lock");
        let paths = project.paths();
        let lease = IndexLease::create_exclusive(&paths, deadline(), || false)
            .expect("create missing-lock namespace");
        publish_index_state(&paths, &lease, StatePhase::Building).expect("publish Building");
        publish_index_state(&paths, &lease, StatePhase::Uninitialized)
            .expect("publish Uninitialized");
        drop(lease);
        std::fs::remove_file(paths.permanent_lock()).expect("remove permanent lock fixture");
        let before = snapshot(&project.0);
        assert!(matches!(
            uninit_index(&paths, deadline(), || false),
            Err(UninitError::Lease(IndexLeaseError::LockNotFound { .. }))
        ));
        assert_unchanged(&before, &snapshot(&project.0));
    }

    #[test]
    fn nonmutation_snapshot_detects_equal_length_replacement() {
        let project = TempProject::new("snapshot-self-test");
        let path = project.0.join("same-length.bin");
        std::fs::write(&path, b"AAAA").expect("write snapshot fixture");
        let before = snapshot(&project.0);
        std::fs::write(&path, b"BBBB").expect("replace fixture at equal length");
        let after = snapshot(&project.0);
        assert!(
            std::panic::catch_unwind(|| assert_unchanged(&before, &after)).is_err(),
            "snapshot oracle must reject equal-length byte replacement"
        );
    }
}
