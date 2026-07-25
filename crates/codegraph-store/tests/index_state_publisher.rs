//! Public behavioral contract for lease-gated dual-slot state publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, CorruptReason, ExtractionStatus,
    IndexLease, IndexLeaseValidationError, ParentSyncStatus, SlotOutcome, StatePhase,
    StatePublishError, checksum_hex, classify, publish_index_state,
};
use serde_json::json;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempProject(PathBuf);

impl TempProject {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codegraph-index-state-publisher-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir(&path)
            .unwrap_or_else(|err| panic!("create temp project {}: {err}", path.display()));
        Self(path.canonicalize().expect("canonical temp project"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn paths(&self) -> IndexPaths {
        IndexPaths::resolve(self.path(), None).expect("resolve publisher test paths")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|err| panic!("remove temp project {}: {err}", self.0.display()));
    }
}

fn deadline() -> Instant {
    Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("publisher test deadline")
}

fn wire_bytes(
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    owner: &str,
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "sequence": sequence,
        "storageProtocol": storage_protocol,
        "extractionVersion": extraction_version,
        "phase": phase,
        "projectIdentity": owner,
        "checksum": checksum_hex(
            sequence,
            storage_protocol,
            extraction_version,
            phase,
            owner,
        ),
    }))
    .expect("serialize publisher state fixture")
}

fn canonical_published_bytes(sequence: u64, phase: &str, owner: &str) -> Vec<u8> {
    let checksum = checksum_hex(
        sequence,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        phase,
        owner,
    );
    format!(
        "{{\"sequence\":{sequence},\"storageProtocol\":{CURRENT_STORAGE_PROTOCOL},\"extractionVersion\":{CURRENT_EXTRACTION_VERSION},\"phase\":\"{phase}\",\"projectIdentity\":\"{owner}\",\"checksum\":\"{checksum}\"}}"
    )
    .into_bytes()
}

fn write_wire(
    path: &Path,
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    owner: &str,
) {
    std::fs::write(
        path,
        wire_bytes(sequence, storage_protocol, extraction_version, phase, owner),
    )
    .unwrap_or_else(|err| panic!("write publisher fixture {}: {err}", path.display()));
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotKind {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, SnapshotKind> {
    fn walk(root: &Path, directory: &Path, out: &mut BTreeMap<PathBuf, SnapshotKind>) {
        let entries = std::fs::read_dir(directory)
            .unwrap_or_else(|err| panic!("snapshot read_dir {}: {err}", directory.display()));
        let mut paths = entries
            .map(|entry| {
                entry
                    .unwrap_or_else(|err| {
                        panic!("snapshot entry in {}: {err}", directory.display())
                    })
                    .path()
            })
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let relative = path
                .strip_prefix(root)
                .unwrap_or_else(|err| panic!("snapshot strip {}: {err}", path.display()))
                .to_path_buf();
            let metadata = std::fs::symlink_metadata(&path)
                .unwrap_or_else(|err| panic!("snapshot metadata {}: {err}", path.display()));
            let file_type = metadata.file_type();
            let kind = if file_type.is_dir() {
                SnapshotKind::Directory
            } else if file_type.is_file() {
                SnapshotKind::File(
                    std::fs::read(&path)
                        .unwrap_or_else(|err| panic!("snapshot read {}: {err}", path.display())),
                )
            } else if file_type.is_symlink() {
                SnapshotKind::Symlink(
                    std::fs::read_link(&path).unwrap_or_else(|err| {
                        panic!("snapshot read_link {}: {err}", path.display())
                    }),
                )
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

    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn assert_snapshot_unchanged(
    before: &BTreeMap<PathBuf, SnapshotKind>,
    after: &BTreeMap<PathBuf, SnapshotKind>,
) {
    let changed = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(changed.is_empty(), "filesystem changed at {changed:?}");
}

fn assert_rejected_without_mutation(
    project: &TempProject,
    invoke: impl FnOnce() -> Result<codegraph_store::PublishedState, StatePublishError>,
    expected: impl FnOnce(&StatePublishError) -> bool,
) {
    let before = snapshot_tree(project.path());
    let error = invoke().expect_err("publication must be rejected");
    assert!(expected(&error), "unexpected publisher error: {error:?}");
    let after = snapshot_tree(project.path());
    assert_snapshot_unchanged(&before, &after);
}

#[test]
fn first_publication_from_absent_state_writes_canonical_slot_zero_sequence_zero() {
    let project = TempProject::new("initial-red");
    let paths = project.paths();
    let lease =
        IndexLease::create_exclusive(&paths, deadline(), || false).expect("create namespace lease");
    assert_eq!(classify(&paths).status(), &ExtractionStatus::Missing);

    let published = publish_index_state(&paths, &lease, StatePhase::Building)
        .expect("initial state publication must succeed");

    assert_eq!(published.slot, 0);
    assert_eq!(published.record.sequence, 0);
    assert_eq!(published.record.storage_protocol, CURRENT_STORAGE_PROTOCOL);
    assert_eq!(
        published.record.extraction_version,
        CURRENT_EXTRACTION_VERSION
    );
    assert_eq!(published.record.phase, Some(StatePhase::Building));
    assert_eq!(published.record.project_identity, paths.project_identity());
    assert!(matches!(
        published.parent_sync,
        ParentSyncStatus::Synced | ParentSyncStatus::Unsupported
    ));
    assert!(paths.state_slots()[0].is_file());
    assert!(!paths.state_slots()[1].exists());
    assert_eq!(
        std::fs::read(&paths.state_slots()[0]).expect("read canonical first slot"),
        canonical_published_bytes(0, "building", paths.project_identity())
    );
    assert_eq!(
        classify(&paths).status(),
        &ExtractionStatus::Building {
            built: CURRENT_EXTRACTION_VERSION
        }
    );
}

#[test]
fn all_current_protocol_phases_publish_monotonically_and_replace_only_the_older_slot() {
    let project = TempProject::new("phase-cycle");
    let paths = project.paths();
    let lease =
        IndexLease::create_exclusive(&paths, deadline(), || false).expect("create namespace lease");

    let building = publish_index_state(&paths, &lease, StatePhase::Building).unwrap();
    let building_bytes = std::fs::read(&paths.state_slots()[0]).unwrap();
    let current = publish_index_state(&paths, &lease, StatePhase::Current).unwrap();
    let current_bytes = std::fs::read(&paths.state_slots()[1]).unwrap();
    assert_eq!(
        std::fs::read(&paths.state_slots()[0]).unwrap(),
        building_bytes
    );

    let uninitialized = publish_index_state(&paths, &lease, StatePhase::Uninitialized).unwrap();
    assert_eq!(building.slot, 0);
    assert_eq!(current.slot, 1);
    assert_eq!(uninitialized.slot, 0);
    assert_eq!(
        [
            building.record.sequence,
            current.record.sequence,
            uninitialized.record.sequence,
        ],
        [0, 1, 2]
    );
    assert_eq!(
        std::fs::read(&paths.state_slots()[1]).unwrap(),
        current_bytes,
        "third publication must preserve the current authoritative slot"
    );
    assert_eq!(classify(&paths).status(), &ExtractionStatus::Uninitialized);
}

#[derive(Debug, Clone, Copy)]
enum PriorStatus {
    Missing,
    Outdated,
    Uninitialized,
    Current,
}

fn stage_prior_status(paths: &IndexPaths, lease: &IndexLease, prior: PriorStatus) {
    match prior {
        PriorStatus::Missing => {}
        PriorStatus::Outdated => write_wire(
            &paths.state_slots()[0],
            4,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION - 1,
            "current",
            paths.project_identity(),
        ),
        PriorStatus::Uninitialized => {
            publish_index_state(paths, lease, StatePhase::Building).unwrap();
            publish_index_state(paths, lease, StatePhase::Uninitialized).unwrap();
        }
        PriorStatus::Current => {
            publish_index_state(paths, lease, StatePhase::Building).unwrap();
            publish_index_state(paths, lease, StatePhase::Current).unwrap();
        }
    }
}

fn expected_prior_status(prior: PriorStatus) -> ExtractionStatus {
    match prior {
        PriorStatus::Missing => ExtractionStatus::Missing,
        PriorStatus::Outdated => ExtractionStatus::Outdated {
            built: CURRENT_EXTRACTION_VERSION - 1,
        },
        PriorStatus::Uninitialized => ExtractionStatus::Uninitialized,
        PriorStatus::Current => ExtractionStatus::Current,
    }
}

#[test]
fn invalid_lifecycle_transitions_are_byte_nonmutating() {
    let cases = [
        (PriorStatus::Missing, StatePhase::Current),
        (PriorStatus::Missing, StatePhase::Uninitialized),
        (PriorStatus::Outdated, StatePhase::Current),
        (PriorStatus::Outdated, StatePhase::Uninitialized),
        (PriorStatus::Uninitialized, StatePhase::Current),
        (PriorStatus::Current, StatePhase::Current),
    ];

    for (prior, requested) in cases {
        let project = TempProject::new("invalid-transition");
        let paths = project.paths();
        let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
        stage_prior_status(&paths, &lease, prior);
        assert_rejected_without_mutation(
            &project,
            || publish_index_state(&paths, &lease, requested),
            |error| {
                matches!(
                    error,
                    StatePublishError::InvalidTransition {
                        current,
                        requested: actual,
                    } if current == &expected_prior_status(prior) && *actual == requested
                )
            },
        );
    }
}

#[test]
fn shared_and_wrong_parent_capabilities_are_typed_and_byte_nonmutating() {
    let shared_project = TempProject::new("shared-capability");
    let shared_paths = shared_project.paths();
    let initial = IndexLease::create_exclusive(&shared_paths, deadline(), || false).unwrap();
    publish_index_state(&shared_paths, &initial, StatePhase::Building).unwrap();
    drop(initial);
    let shared = IndexLease::acquire_shared_existing(&shared_paths, deadline(), || false).unwrap();
    assert_rejected_without_mutation(
        &shared_project,
        || publish_index_state(&shared_paths, &shared, StatePhase::Building),
        |error| {
            matches!(
                error,
                StatePublishError::Lease(IndexLeaseValidationError::SharedLease)
            )
        },
    );
    drop(shared);

    let other_project = TempProject::new("wrong-parent-capability");
    let other_paths = other_project.paths();
    let other_lease = IndexLease::create_exclusive(&other_paths, deadline(), || false).unwrap();
    publish_index_state(&other_paths, &other_lease, StatePhase::Building).unwrap();
    let wrong_lease =
        IndexLease::acquire_exclusive_existing(&shared_paths, deadline(), || false).unwrap();
    assert_rejected_without_mutation(
        &other_project,
        || publish_index_state(&other_paths, &wrong_lease, StatePhase::Building),
        |error| {
            matches!(
                error,
                StatePublishError::Lease(IndexLeaseValidationError::WrongDbParent)
            )
        },
    );
}

#[test]
fn replaced_permanent_lock_invalidates_publisher_capability_before_state_mutation() {
    let project = TempProject::new("replaced-lock");
    let paths = project.paths();
    let stale = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    publish_index_state(&paths, &stale, StatePhase::Building).unwrap();

    let displaced = paths.current_root().join("displaced-index.lock");
    std::fs::rename(paths.permanent_lock(), &displaced).expect("displace locked handle");
    std::fs::write(paths.permanent_lock(), b"replacement lock").expect("install replacement lock");

    assert_rejected_without_mutation(
        &project,
        || publish_index_state(&paths, &stale, StatePhase::Current),
        |error| {
            matches!(
                error,
                StatePublishError::Lease(
                    IndexLeaseValidationError::PermanentLockChanged { path }
                ) if path == &paths.permanent_lock()
            )
        },
    );

    let fresh = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
        .expect("replacement lock is independent of the stale capability");
    drop(fresh);
    drop(stale);
}

#[test]
fn missing_permanent_lock_invalidates_publisher_capability_before_state_mutation() {
    let project = TempProject::new("missing-lock-after-acquire");
    let paths = project.paths();
    let stale = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    publish_index_state(&paths, &stale, StatePhase::Building).unwrap();

    let displaced = paths.current_root().join("displaced-index.lock");
    std::fs::rename(paths.permanent_lock(), &displaced).expect("displace locked handle");

    assert_rejected_without_mutation(
        &project,
        || publish_index_state(&paths, &stale, StatePhase::Current),
        |error| {
            matches!(
                error,
                StatePublishError::Lease(
                    IndexLeaseValidationError::PermanentLockChanged { path }
                ) if path == &paths.permanent_lock()
            )
        },
    );
}

#[test]
fn non_regular_permanent_lock_invalidates_publisher_capability_before_state_mutation() {
    let project = TempProject::new("non-regular-lock-after-acquire");
    let paths = project.paths();
    let stale = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    publish_index_state(&paths, &stale, StatePhase::Building).unwrap();

    let displaced = paths.current_root().join("displaced-index.lock");
    std::fs::rename(paths.permanent_lock(), &displaced).expect("displace locked handle");
    std::fs::create_dir(paths.permanent_lock()).expect("install directory at fixed lock path");

    assert_rejected_without_mutation(
        &project,
        || publish_index_state(&paths, &stale, StatePhase::Current),
        |error| {
            matches!(
                error,
                StatePublishError::Lease(
                    IndexLeaseValidationError::PermanentLockChanged { path }
                ) if path == &paths.permanent_lock()
            )
        },
    );
}

#[cfg(unix)]
#[test]
fn aliased_permanent_lock_invalidates_publisher_capability_before_state_mutation() {
    use std::os::unix::fs::symlink;

    let project = TempProject::new("aliased-lock-after-acquire");
    let paths = project.paths();
    let stale = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    publish_index_state(&paths, &stale, StatePhase::Building).unwrap();

    let displaced = paths.current_root().join("displaced-index.lock");
    let external = project.path().join("external.lock");
    std::fs::rename(paths.permanent_lock(), &displaced).expect("displace locked handle");
    std::fs::write(&external, b"external lock target").expect("write external lock target");
    symlink(&external, paths.permanent_lock()).expect("install alias at fixed lock path");

    assert_rejected_without_mutation(
        &project,
        || publish_index_state(&paths, &stale, StatePhase::Current),
        |error| {
            matches!(
                error,
                StatePublishError::Lease(
                    IndexLeaseValidationError::PermanentLockChanged { path }
                ) if path == &paths.permanent_lock()
            )
        },
    );
    assert_eq!(std::fs::read(&external).unwrap(), b"external lock target");
}

#[test]
fn invalid_fixed_slot_equal_sequence_and_exhaustion_are_refused_byte_nonmutating() {
    for case in ["invalid", "equal", "exhausted"] {
        let project = TempProject::new(case);
        let paths = project.paths();
        let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
        match case {
            "invalid" => {
                write_wire(
                    &paths.state_slots()[0],
                    3,
                    2,
                    2,
                    "current",
                    paths.project_identity(),
                );
                std::fs::write(&paths.state_slots()[1], b"not json").unwrap();
            }
            "equal" => {
                write_wire(
                    &paths.state_slots()[0],
                    3,
                    2,
                    2,
                    "building",
                    paths.project_identity(),
                );
                write_wire(
                    &paths.state_slots()[1],
                    3,
                    2,
                    2,
                    "current",
                    paths.project_identity(),
                );
            }
            "exhausted" => write_wire(
                &paths.state_slots()[0],
                u64::MAX,
                2,
                2,
                "current",
                paths.project_identity(),
            ),
            _ => unreachable!(),
        }

        assert_rejected_without_mutation(
            &project,
            || publish_index_state(&paths, &lease, StatePhase::Uninitialized),
            |error| {
                matches!(
                    (case, error),
                    (
                        "invalid",
                        StatePublishError::Refused {
                            status: ExtractionStatus::Corrupt {
                                reason: CorruptReason::MalformedJson { slot: 1, .. },
                            },
                        },
                    ) | (
                        "equal",
                        StatePublishError::Refused {
                            status: ExtractionStatus::Corrupt {
                                reason: CorruptReason::EqualSequence { sequence: 3, .. },
                            },
                        },
                    ) | (
                        "exhausted",
                        StatePublishError::Refused {
                            status: ExtractionStatus::Corrupt {
                                reason: CorruptReason::SequenceExhausted { sequence: u64::MAX },
                            },
                        },
                    )
                )
            },
        );
    }
}

#[test]
fn owner_mismatch_is_typed_and_byte_nonmutating() {
    let project = TempProject::new("owner-mismatch");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    let other_owner = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write_wire(
        &paths.state_slots()[0],
        1,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        other_owner,
    );

    assert_rejected_without_mutation(
        &project,
        || publish_index_state(&paths, &lease, StatePhase::Building),
        |error| {
            matches!(
                error,
                StatePublishError::Refused {
                    status: ExtractionStatus::Corrupt {
                        reason: CorruptReason::OwnerMismatch { slot: 0, .. }
                    }
                }
            )
        },
    );
}

#[test]
fn future_v3_slot_dominates_current_companion_and_is_never_replaced() {
    let project = TempProject::new("future-v3");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    write_wire(
        &paths.state_slots()[0],
        90,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        paths.project_identity(),
    );
    write_wire(
        &paths.state_slots()[1],
        91,
        3,
        99,
        "v3-replacement",
        paths.project_identity(),
    );
    assert_eq!(
        classify(&paths).status(),
        &ExtractionStatus::Future { built: 99 }
    );

    assert_rejected_without_mutation(
        &project,
        || publish_index_state(&paths, &lease, StatePhase::Uninitialized),
        |error| {
            matches!(
                error,
                StatePublishError::Refused {
                    status: ExtractionStatus::Future { built: 99 }
                }
            )
        },
    );
}

#[test]
fn missing_inactive_slot_is_recreated_while_all_preexisting_temps_remain_untouched() {
    let project = TempProject::new("missing-inactive");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    publish_index_state(&paths, &lease, StatePhase::Building).unwrap();
    publish_index_state(&paths, &lease, StatePhase::Current).unwrap();
    std::fs::remove_file(&paths.state_slots()[0]).expect("remove inactive fixture slot");
    assert!(matches!(classify(&paths).slot(0), SlotOutcome::Absent));
    let orphan = paths
        .current_root()
        .join(".codegraph-index-state-publisher-v2-999-0000000000000001.tmp");
    std::fs::write(&orphan, b"orphan").unwrap();
    let orphan_directory = paths
        .current_root()
        .join(".codegraph-index-state-publisher-v2-998-0000000000000002.tmp");
    std::fs::create_dir(&orphan_directory).unwrap();
    let directory_marker = orphan_directory.join("keep.bin");
    std::fs::write(&directory_marker, b"directory marker").unwrap();
    let unrelated = paths
        .current_root()
        .join(".codegraph-index-state-publisher-v2-not-owned.tmp");
    std::fs::write(&unrelated, b"unrelated").unwrap();
    assert_eq!(classify(&paths).status(), &ExtractionStatus::Current);

    let published = publish_index_state(&paths, &lease, StatePhase::Building).unwrap();
    assert_eq!(published.slot, 0);
    assert_eq!(std::fs::read(&orphan).unwrap(), b"orphan");
    assert_eq!(
        std::fs::read(&directory_marker).unwrap(),
        b"directory marker"
    );
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
    assert_eq!(
        classify(&paths).status(),
        &ExtractionStatus::Building {
            built: CURRENT_EXTRACTION_VERSION
        }
    );
}

#[test]
fn future_or_corrupt_classification_preserves_preexisting_temp_like_entries() {
    for case in ["future", "corrupt"] {
        let project = TempProject::new(case);
        let paths = project.paths();
        let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
        if case == "future" {
            write_wire(
                &paths.state_slots()[0],
                1,
                3,
                3,
                "future",
                paths.project_identity(),
            );
        } else {
            std::fs::write(&paths.state_slots()[0], b"invalid").unwrap();
        }
        let preexisting = paths
            .current_root()
            .join(".codegraph-index-state-publisher-v2-999-0000000000000001.tmp");
        std::fs::write(&preexisting, b"preexisting").unwrap();

        for requested in [
            StatePhase::Building,
            StatePhase::Current,
            StatePhase::Uninitialized,
        ] {
            assert_rejected_without_mutation(
                &project,
                || publish_index_state(&paths, &lease, requested),
                |error| matches!(error, StatePublishError::Refused { .. }),
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn temp_symlinks_and_unrelated_names_survive_successful_publication() {
    use std::os::unix::fs::symlink;

    let project = TempProject::new("temp-alias");
    let outside = TempProject::new("temp-alias-outside");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    publish_index_state(&paths, &lease, StatePhase::Building).unwrap();
    let target = outside.path().join("target.bin");
    std::fs::write(&target, b"external bytes").unwrap();
    let alias = paths
        .current_root()
        .join(".codegraph-index-state-publisher-v2-999-0000000000000001.tmp");
    symlink(&target, &alias).unwrap();
    let unrelated = paths
        .current_root()
        .join(".codegraph-index-state-publisher-v2-not-owned.tmp");
    std::fs::write(&unrelated, b"unrelated").unwrap();

    publish_index_state(&paths, &lease, StatePhase::Building).unwrap();
    assert!(
        std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"external bytes");
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
}
