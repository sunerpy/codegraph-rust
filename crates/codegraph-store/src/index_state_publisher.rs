//! Lease-gated, crash-safe publication of the dual fixed index-state slots.
//!
//! Publication always validates an exact exclusive [`IndexLease`], consumes the
//! accepted read-only classifier under that lease, and preserves the selected
//! authoritative slot. The successor is written to a same-directory
//! create-new temporary file, flushed, synchronized, and renamed over only the
//! older or missing inactive slot. Temporary files are outside the fixed-slot
//! scanner by construction.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::IndexPaths;
use serde::Serialize;
use thiserror::Error;

use crate::file_identity::is_regular;
use crate::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, ExtractionStatus, IndexLease,
    IndexLeaseValidationError, SlotOutcome, StatePhase, StateSlotRecord, checksum_hex, classify,
};

const TEMP_PREFIX: &str = ".codegraph-index-state-publisher-v2-";
const TEMP_SUFFIX: &str = ".tmp";
const TEMP_CREATE_ATTEMPTS: u8 = 64;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Whether the publication's parent-directory durability barrier was available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentSyncStatus {
    /// The parent directory was synchronized after the final rename.
    Synced,
    /// The platform/filesystem explicitly reported that directory sync is unsupported.
    Unsupported,
}

/// The fully validated state made visible by one publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedState {
    /// Fixed slot index that received the new record.
    pub slot: u8,
    /// Canonical record written to that slot.
    pub record: StateSlotRecord,
    /// Result of the attempted parent-directory durability barrier.
    pub parent_sync: ParentSyncStatus,
}

/// The filesystem operation that failed during publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatePublishOperation {
    /// Creating a fresh same-directory temporary file.
    CreateTemp,
    /// Writing all canonical JSON bytes to the temporary file.
    WriteTemp,
    /// Flushing userspace buffers for the temporary file.
    FlushTemp,
    /// Synchronizing the temporary file's data and metadata.
    SyncTemp,
    /// Removing the older inactive fixed slot before replacement.
    PrepareInactiveSlot,
    /// Renaming the valid temporary file into the inactive fixed slot.
    RenameTemp,
    /// Synchronizing the parent directory after the rename.
    SyncParent,
}

impl fmt::Display for StatePublishOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CreateTemp => "create publisher temporary file",
            Self::WriteTemp => "write publisher temporary file",
            Self::FlushTemp => "flush publisher temporary file",
            Self::SyncTemp => "synchronize publisher temporary file",
            Self::PrepareInactiveSlot => "prepare inactive state slot",
            Self::RenameTemp => "rename publisher temporary file",
            Self::SyncParent => "synchronize state-slot parent directory",
        })
    }
}

/// Typed failures from state publication.
#[derive(Debug, Error)]
pub enum StatePublishError {
    /// The supplied capability is shared or belongs to another namespace.
    #[error(transparent)]
    Lease(#[from] IndexLeaseValidationError),
    /// The under-lease classifier refused mutation of this namespace.
    #[error("index state publication refused for {status}")]
    Refused {
        /// Exact accepted-classifier result that caused the refusal.
        status: ExtractionStatus,
    },
    /// The requested phase is not a valid lifecycle successor of the accepted
    /// under-lease classification.
    #[error("cannot publish phase {requested} from index state {current}")]
    InvalidTransition {
        /// Exact accepted-classifier status before publication.
        current: ExtractionStatus,
        /// Requested successor phase.
        requested: StatePhase,
    },
    /// The accepted classifier returned an internally inconsistent authority.
    #[error("index state classifier invariant failed: {detail}")]
    ClassifierInvariant {
        /// Stable invariant description.
        detail: &'static str,
    },
    /// Canonical typed JSON serialization failed.
    #[error("cannot serialize canonical index state: {source}")]
    Serialize {
        /// Serialization failure.
        source: serde_json::Error,
    },
    /// A bounded create-new retry could not find an unused publisher temp name.
    #[error(
        "cannot allocate a create-new index state temp under {parent} after {attempts} attempts"
    )]
    TempNameExhausted {
        /// State-slot parent directory.
        parent: PathBuf,
        /// Number of bounded attempts.
        attempts: u8,
    },
    /// A publication I/O operation failed.
    #[error("cannot {operation} at {path}: {source}")]
    Io {
        /// Operation that failed.
        operation: StatePublishOperation,
        /// Exact affected path.
        path: PathBuf,
        /// Operating-system error.
        source: io::Error,
    },
}

/// Publish the current storage/extraction protocol at `phase`.
///
/// The only accepted initial transition is `Missing -> Building`, which
/// deterministically publishes sequence `0` to fixed slot `0`. Every allowed
/// successor uses `checked_add(1)` and the opposite fixed slot. Invalid lifecycle
/// transitions, `Future`, `Corrupt` (including invalid fixed slots, owner
/// mismatch, equal sequence, and exhaustion), and invalid leases fail before any
/// mutation.
pub fn publish_index_state(
    paths: &IndexPaths,
    lease: &IndexLease,
    phase: StatePhase,
) -> Result<PublishedState, StatePublishError> {
    publish_index_state_with(paths, lease, phase, &mut NoFault)
}

#[derive(Serialize)]
struct WireState<'a> {
    sequence: u64,
    #[serde(rename = "storageProtocol")]
    storage_protocol: u64,
    #[serde(rename = "extractionVersion")]
    extraction_version: u64,
    phase: &'static str,
    #[serde(rename = "projectIdentity")]
    project_identity: &'a str,
    checksum: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishCheckpoint {
    TempCreated,
    TempWritten,
    TempFlushed,
    TempSynced,
    InactiveSlotPrepared,
    TempRenamed,
    ParentSyncAttempted,
}

trait FaultInjector {
    fn after(&mut self, checkpoint: PublishCheckpoint) -> io::Result<()>;
}

struct NoFault;

impl FaultInjector for NoFault {
    fn after(&mut self, _checkpoint: PublishCheckpoint) -> io::Result<()> {
        Ok(())
    }
}

fn publish_index_state_with(
    paths: &IndexPaths,
    lease: &IndexLease,
    phase: StatePhase,
    fault: &mut impl FaultInjector,
) -> Result<PublishedState, StatePublishError> {
    // Capability validation and accepted classification must both happen before
    // temp creation or any other mutation.
    lease.validate_exclusive(paths)?;
    let classification = classify(paths);
    if matches!(
        classification.status(),
        ExtractionStatus::Future { .. } | ExtractionStatus::Corrupt { .. }
    ) {
        return Err(StatePublishError::Refused {
            status: classification.status().clone(),
        });
    }
    validate_transition(classification.status(), phase)?;

    let (sequence, target_index) = match classification.status() {
        ExtractionStatus::Missing => {
            if classification.authoritative().is_some() {
                return Err(StatePublishError::ClassifierInvariant {
                    detail: "Missing classification selected an authoritative slot",
                });
            }
            (0, 0_usize)
        }
        ExtractionStatus::Current
        | ExtractionStatus::Building { .. }
        | ExtractionStatus::Uninitialized
        | ExtractionStatus::Outdated { .. } => {
            let authority =
                classification
                    .authoritative()
                    .ok_or(StatePublishError::ClassifierInvariant {
                        detail: "non-Missing publishable classification has no authority",
                    })?;
            let sequence = authority.record.sequence.checked_add(1).ok_or({
                StatePublishError::Refused {
                    status: ExtractionStatus::Corrupt {
                        reason: crate::CorruptReason::SequenceExhausted {
                            sequence: authority.record.sequence,
                        },
                    },
                }
            })?;
            let authority_index = usize::from(authority.index);
            if authority_index >= 2 {
                return Err(StatePublishError::ClassifierInvariant {
                    detail: "authoritative fixed-slot index is outside the slot pair",
                });
            }
            (sequence, 1 - authority_index)
        }
        ExtractionStatus::Future { .. } | ExtractionStatus::Corrupt { .. } => unreachable!(),
    };

    match classification.slot(target_index) {
        SlotOutcome::Absent => {}
        SlotOutcome::Valid(record) => {
            if record.sequence >= sequence {
                return Err(StatePublishError::ClassifierInvariant {
                    detail: "inactive slot is not older than the successor sequence",
                });
            }
        }
        SlotOutcome::FutureProtocol(_) | SlotOutcome::Invalid(_) => {
            return Err(StatePublishError::ClassifierInvariant {
                detail: "publishable classification contains a future or invalid inactive slot",
            });
        }
    }

    let owner = paths.project_identity();
    let phase_wire = phase.as_wire();
    let checksum = checksum_hex(
        sequence,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        phase_wire,
        owner,
    );
    let bytes = serde_json::to_vec(&WireState {
        sequence,
        storage_protocol: CURRENT_STORAGE_PROTOCOL,
        extraction_version: CURRENT_EXTRACTION_VERSION,
        phase: phase_wire,
        project_identity: owner,
        checksum: &checksum,
    })
    .map_err(|source| StatePublishError::Serialize { source })?;

    let slots = paths.state_slots();
    let target = &slots[target_index];
    let parent = target
        .parent()
        .ok_or(StatePublishError::ClassifierInvariant {
            detail: "fixed state slot has no parent directory",
        })?;
    if parent != paths.current_root() || slots[1 - target_index].parent() != Some(parent) {
        return Err(StatePublishError::ClassifierInvariant {
            detail: "fixed state slots do not share IndexPaths::current_root",
        });
    }

    let (temp_path, mut temp) = create_temp(parent)?;
    checkpoint(
        fault,
        PublishCheckpoint::TempCreated,
        StatePublishOperation::CreateTemp,
        &temp_path,
    )?;

    temp.write_all(&bytes)
        .map_err(|source| io_error(StatePublishOperation::WriteTemp, &temp_path, source))?;
    checkpoint(
        fault,
        PublishCheckpoint::TempWritten,
        StatePublishOperation::WriteTemp,
        &temp_path,
    )?;
    temp.flush()
        .map_err(|source| io_error(StatePublishOperation::FlushTemp, &temp_path, source))?;
    checkpoint(
        fault,
        PublishCheckpoint::TempFlushed,
        StatePublishOperation::FlushTemp,
        &temp_path,
    )?;
    temp.sync_all()
        .map_err(|source| io_error(StatePublishOperation::SyncTemp, &temp_path, source))?;
    checkpoint(
        fault,
        PublishCheckpoint::TempSynced,
        StatePublishOperation::SyncTemp,
        &temp_path,
    )?;
    drop(temp);

    match std::fs::symlink_metadata(target) {
        Ok(metadata) => {
            if !is_regular(&metadata) {
                return Err(StatePublishError::ClassifierInvariant {
                    detail: "inactive fixed slot changed after under-lease classification",
                });
            }
            std::fs::remove_file(target).map_err(|source| {
                io_error(StatePublishOperation::PrepareInactiveSlot, target, source)
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(io_error(
                StatePublishOperation::PrepareInactiveSlot,
                target,
                source,
            ));
        }
    }
    checkpoint(
        fault,
        PublishCheckpoint::InactiveSlotPrepared,
        StatePublishOperation::PrepareInactiveSlot,
        target,
    )?;

    std::fs::rename(&temp_path, target)
        .map_err(|source| io_error(StatePublishOperation::RenameTemp, target, source))?;
    checkpoint(
        fault,
        PublishCheckpoint::TempRenamed,
        StatePublishOperation::RenameTemp,
        target,
    )?;

    let parent_sync = sync_parent(parent)?;
    checkpoint(
        fault,
        PublishCheckpoint::ParentSyncAttempted,
        StatePublishOperation::SyncParent,
        parent,
    )?;

    let record = StateSlotRecord {
        sequence,
        storage_protocol: CURRENT_STORAGE_PROTOCOL,
        extraction_version: CURRENT_EXTRACTION_VERSION,
        phase: Some(phase),
        phase_raw: phase_wire.to_string(),
        project_identity: owner.to_string(),
        checksum,
    };
    Ok(PublishedState {
        slot: u8::try_from(target_index).expect("fixed slot index fits u8"),
        record,
        parent_sync,
    })
}

fn checkpoint(
    fault: &mut impl FaultInjector,
    checkpoint: PublishCheckpoint,
    operation: StatePublishOperation,
    path: &Path,
) -> Result<(), StatePublishError> {
    fault
        .after(checkpoint)
        .map_err(|source| io_error(operation, path, source))
}

fn io_error(operation: StatePublishOperation, path: &Path, source: io::Error) -> StatePublishError {
    StatePublishError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn create_temp(parent: &Path) -> Result<(PathBuf, File), StatePublishError> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "{TEMP_PREFIX}{}-{serial:016x}{TEMP_SUFFIX}",
            std::process::id()
        );
        let path = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .truncate(false)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(io_error(StatePublishOperation::CreateTemp, &path, source));
            }
        }
    }
    Err(StatePublishError::TempNameExhausted {
        parent: parent.to_path_buf(),
        attempts: TEMP_CREATE_ATTEMPTS,
    })
}

fn validate_transition(
    current: &ExtractionStatus,
    requested: StatePhase,
) -> Result<(), StatePublishError> {
    let allowed = matches!(
        (current, requested),
        (ExtractionStatus::Missing, StatePhase::Building)
            | (ExtractionStatus::Outdated { .. }, StatePhase::Building)
            | (
                ExtractionStatus::Uninitialized,
                StatePhase::Building | StatePhase::Uninitialized
            )
            | (
                ExtractionStatus::Current,
                StatePhase::Building | StatePhase::Uninitialized
            )
            | (
                ExtractionStatus::Building { .. },
                StatePhase::Building | StatePhase::Current | StatePhase::Uninitialized
            )
    );
    if allowed {
        Ok(())
    } else {
        Err(StatePublishError::InvalidTransition {
            current: current.clone(),
            requested,
        })
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<ParentSyncStatus, StatePublishError> {
    let directory = File::open(parent)
        .map_err(|source| io_error(StatePublishOperation::SyncParent, parent, source))?;
    match directory.sync_all() {
        Ok(()) => Ok(ParentSyncStatus::Synced),
        Err(error) if directory_sync_unsupported(&error) => Ok(ParentSyncStatus::Unsupported),
        Err(source) => Err(io_error(StatePublishOperation::SyncParent, parent, source)),
    }
}

#[cfg(windows)]
fn sync_parent(parent: &Path) -> Result<ParentSyncStatus, StatePublishError> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(parent)
        .map_err(|source| io_error(StatePublishOperation::SyncParent, parent, source))?;
    match directory.sync_all() {
        Ok(()) => Ok(ParentSyncStatus::Synced),
        Err(error) if directory_sync_unsupported(&error) => Ok(ParentSyncStatus::Unsupported),
        Err(source) => Err(io_error(StatePublishOperation::SyncParent, parent, source)),
    }
}

#[cfg(not(any(unix, windows)))]
fn sync_parent(_parent: &Path) -> Result<ParentSyncStatus, StatePublishError> {
    Ok(ParentSyncStatus::Unsupported)
}

#[cfg(any(unix, windows))]
fn directory_sync_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        // EINVAL and EOPNOTSUPP/ENOTSUP are the conventional reports for a
        // filesystem that cannot synchronize directory entries.
        matches!(error.raw_os_error(), Some(22 | 95))
    }
    #[cfg(windows)]
    {
        // ERROR_INVALID_FUNCTION, ERROR_INVALID_HANDLE, ERROR_NOT_SUPPORTED,
        // and ERROR_INVALID_PARAMETER explicitly mean this handle/filesystem
        // does not expose a directory flush primitive. Access errors are NOT
        // downgraded to unsupported.
        matches!(error.raw_os_error(), Some(1 | 6 | 50 | 87))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    struct TempProject(PathBuf);

    impl TempProject {
        fn new(label: &str) -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "codegraph-state-publisher-unit-{label}-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create publisher unit project");
            Self(
                path.canonicalize()
                    .expect("canonical publisher unit project"),
            )
        }

        fn paths(&self) -> IndexPaths {
            IndexPaths::resolve(&self.0, None).expect("resolve publisher unit paths")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove publisher unit project");
        }
    }

    fn deadline() -> Instant {
        Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("publisher unit deadline")
    }

    struct FailAfter(PublishCheckpoint);

    impl FaultInjector for FailAfter {
        fn after(&mut self, checkpoint: PublishCheckpoint) -> io::Result<()> {
            if checkpoint == self.0 {
                Err(io::Error::other(format!(
                    "injected crash after {checkpoint:?}"
                )))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum PriorStatus {
        Outdated,
        Building,
        Current,
        Uninitialized,
    }

    fn stage_prior(paths: &IndexPaths, lease: &IndexLease, prior: PriorStatus) -> Vec<u8> {
        match prior {
            PriorStatus::Outdated => {
                let sequence = 1;
                let extraction_version = CURRENT_EXTRACTION_VERSION - 1;
                let phase = StatePhase::Current;
                let owner = paths.project_identity();
                let checksum = checksum_hex(
                    sequence,
                    CURRENT_STORAGE_PROTOCOL,
                    extraction_version,
                    phase.as_wire(),
                    owner,
                );
                let bytes = serde_json::to_vec(&WireState {
                    sequence,
                    storage_protocol: CURRENT_STORAGE_PROTOCOL,
                    extraction_version,
                    phase: phase.as_wire(),
                    project_identity: owner,
                    checksum: &checksum,
                })
                .expect("serialize outdated prior state");
                std::fs::write(&paths.state_slots()[1], &bytes)
                    .expect("write outdated prior state");
            }
            PriorStatus::Building => {
                publish_index_state(paths, lease, StatePhase::Building)
                    .expect("publish first building fixture state");
                publish_index_state(paths, lease, StatePhase::Building)
                    .expect("publish authoritative building fixture state");
            }
            PriorStatus::Current => {
                publish_index_state(paths, lease, StatePhase::Building)
                    .expect("publish building fixture state");
                publish_index_state(paths, lease, StatePhase::Current)
                    .expect("publish current fixture state");
            }
            PriorStatus::Uninitialized => {
                publish_index_state(paths, lease, StatePhase::Building)
                    .expect("publish building fixture state");
                publish_index_state(paths, lease, StatePhase::Uninitialized)
                    .expect("publish uninitialized fixture state");
            }
        }
        std::fs::read(&paths.state_slots()[1]).expect("read prior authoritative bytes")
    }

    fn expected_phase_status(phase: StatePhase) -> ExtractionStatus {
        match phase {
            StatePhase::Building => ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            },
            StatePhase::Current => ExtractionStatus::Current,
            StatePhase::Uninitialized => ExtractionStatus::Uninitialized,
        }
    }

    fn expected_prior_status(prior: PriorStatus) -> ExtractionStatus {
        match prior {
            PriorStatus::Outdated => ExtractionStatus::Outdated {
                built: CURRENT_EXTRACTION_VERSION - 1,
            },
            PriorStatus::Building => expected_phase_status(StatePhase::Building),
            PriorStatus::Current => expected_phase_status(StatePhase::Current),
            PriorStatus::Uninitialized => expected_phase_status(StatePhase::Uninitialized),
        }
    }

    #[test]
    fn transition_validator_covers_every_status_and_requested_phase_pair() {
        let statuses = [
            ExtractionStatus::Missing,
            ExtractionStatus::Outdated { built: 1 },
            ExtractionStatus::Uninitialized,
            ExtractionStatus::Current,
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            },
            ExtractionStatus::Future { built: 99 },
            ExtractionStatus::Corrupt {
                reason: crate::CorruptReason::SequenceExhausted { sequence: u64::MAX },
            },
        ];
        let phases = [
            StatePhase::Building,
            StatePhase::Current,
            StatePhase::Uninitialized,
        ];

        for current in statuses {
            for requested in phases {
                let expected = matches!(
                    (&current, requested),
                    (ExtractionStatus::Missing, StatePhase::Building)
                        | (ExtractionStatus::Outdated { .. }, StatePhase::Building)
                        | (
                            ExtractionStatus::Uninitialized,
                            StatePhase::Building | StatePhase::Uninitialized
                        )
                        | (
                            ExtractionStatus::Current,
                            StatePhase::Building | StatePhase::Uninitialized
                        )
                        | (ExtractionStatus::Building { .. }, _)
                );
                let result = validate_transition(&current, requested);
                assert_eq!(
                    result.is_ok(),
                    expected,
                    "current={current:?}, requested={requested:?}, result={result:?}"
                );
                if !expected {
                    assert!(matches!(
                        result,
                        Err(StatePublishError::InvalidTransition {
                            current: actual_current,
                            requested: actual_requested,
                        }) if actual_current == current && actual_requested == requested
                    ));
                }
            }
        }
    }

    #[test]
    fn every_publication_checkpoint_preserves_old_authority_or_exposes_valid_successor() {
        let allowed = [
            (PriorStatus::Outdated, StatePhase::Building),
            (PriorStatus::Uninitialized, StatePhase::Building),
            (PriorStatus::Uninitialized, StatePhase::Uninitialized),
            (PriorStatus::Current, StatePhase::Building),
            (PriorStatus::Current, StatePhase::Uninitialized),
            (PriorStatus::Building, StatePhase::Building),
            (PriorStatus::Building, StatePhase::Current),
            (PriorStatus::Building, StatePhase::Uninitialized),
        ];
        let checkpoints = [
            PublishCheckpoint::TempCreated,
            PublishCheckpoint::TempWritten,
            PublishCheckpoint::TempFlushed,
            PublishCheckpoint::TempSynced,
            PublishCheckpoint::InactiveSlotPrepared,
            PublishCheckpoint::TempRenamed,
            PublishCheckpoint::ParentSyncAttempted,
        ];

        for (prior, next_phase) in allowed {
            for checkpoint in checkpoints {
                let project = TempProject::new("fault-matrix");
                let paths = project.paths();
                let lease = IndexLease::create_exclusive(&paths, deadline(), || false)
                    .expect("create fault-matrix lease");
                let authority_bytes = stage_prior(&paths, &lease, prior);

                let error = publish_index_state_with(
                    &paths,
                    &lease,
                    next_phase,
                    &mut FailAfter(checkpoint),
                )
                .expect_err("fault checkpoint must interrupt publication");
                assert!(matches!(error, StatePublishError::Io { .. }));
                assert_eq!(
                    std::fs::read(&paths.state_slots()[1]).expect("old authority survives"),
                    authority_bytes,
                    "prior={prior:?}, next={next_phase:?}, fault={checkpoint:?}"
                );

                let observed = classify(&paths);
                let old = expected_prior_status(prior);
                let new = expected_phase_status(next_phase);
                assert!(
                    observed.status() == &old || observed.status() == &new,
                    "prior={prior:?}, next={next_phase:?}, fault={checkpoint:?}, observed={observed:?}"
                );
                if matches!(
                    checkpoint,
                    PublishCheckpoint::TempRenamed | PublishCheckpoint::ParentSyncAttempted
                ) {
                    assert_eq!(observed.status(), &new);
                } else {
                    assert_eq!(observed.status(), &old);
                }
            }
        }
    }

    #[test]
    fn every_initial_publication_checkpoint_leaves_missing_or_exposes_a_valid_first_slot() {
        let checkpoints = [
            PublishCheckpoint::TempCreated,
            PublishCheckpoint::TempWritten,
            PublishCheckpoint::TempFlushed,
            PublishCheckpoint::TempSynced,
            PublishCheckpoint::InactiveSlotPrepared,
            PublishCheckpoint::TempRenamed,
            PublishCheckpoint::ParentSyncAttempted,
        ];

        for checkpoint in checkpoints {
            let project = TempProject::new("initial-fault-matrix");
            let paths = project.paths();
            let lease = IndexLease::create_exclusive(&paths, deadline(), || false)
                .expect("create initial-fault lease");

            let error = publish_index_state_with(
                &paths,
                &lease,
                StatePhase::Building,
                &mut FailAfter(checkpoint),
            )
            .expect_err("initial fault checkpoint must interrupt publication");
            assert!(matches!(error, StatePublishError::Io { .. }));
            let observed = classify(&paths);
            if matches!(
                checkpoint,
                PublishCheckpoint::TempRenamed | PublishCheckpoint::ParentSyncAttempted
            ) {
                assert_eq!(
                    observed.status(),
                    &expected_phase_status(StatePhase::Building)
                );
                assert_eq!(
                    observed
                        .authoritative()
                        .expect("valid initial authority")
                        .record
                        .sequence,
                    0
                );
            } else {
                assert_eq!(observed.status(), &ExtractionStatus::Missing);
            }
        }
    }
}
