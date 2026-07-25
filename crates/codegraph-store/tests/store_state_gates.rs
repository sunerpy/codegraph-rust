//! Public contract for lease-retaining, state-gated Store opens.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, EXTRACTION_VERSION_KEY,
    ExtractionStampIssue, ExtractionStatus, IndexLease, IndexLeaseValidationError, Store,
    StoreError, StoreWriteOpen, StoreWritePurpose, checksum_hex,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::json;

const CHILD_ACTION: &str = "CODEGRAPH_STORE_GATE_CHILD_ACTION";
const CHILD_PROJECT: &str = "CODEGRAPH_STORE_GATE_CHILD_PROJECT";
const CHILD_WAIT: Duration = Duration::from_secs(5);
const SHORT_DEADLINE: Duration = Duration::from_millis(80);
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempProject(PathBuf);

impl TempProject {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codegraph-store-state-gates-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("create temp project {}: {error}", path.display()));
        Self(path.canonicalize().expect("canonical temp project"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn paths(&self) -> IndexPaths {
        IndexPaths::resolve(self.path(), None).expect("resolve Store test paths")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("remove temp project {}: {error}", self.0.display()));
    }
}

fn deadline_after(duration: Duration) -> Instant {
    Instant::now()
        .checked_add(duration)
        .expect("Store test deadline")
}

fn deadline() -> Instant {
    deadline_after(CHILD_WAIT)
}

fn create_namespace(paths: &IndexPaths) -> IndexLease {
    IndexLease::create_exclusive(paths, deadline(), || false)
        .expect("create permanent Store test namespace")
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
    .expect("serialize Store state fixture")
}

fn write_state(paths: &IndexPaths, storage_protocol: u64, extraction_version: u64, phase: &str) {
    std::fs::write(
        &paths.state_slots()[0],
        wire_bytes(
            1,
            storage_protocol,
            extraction_version,
            phase,
            paths.project_identity(),
        ),
    )
    .expect("write Store state fixture");
}

fn create_database(paths: &IndexPaths, stamp: Option<&str>) {
    let store = Store::open(&paths.current_db()).expect("create Store SQLite fixture");
    if let Some(stamp) = stamp {
        store
            .set_project_metadata(EXTRACTION_VERSION_KEY, stamp)
            .expect("write extraction stamp fixture");
    }
    store
        .restore_default_pragmas()
        .expect("checkpoint Store SQLite fixture");
    drop(store);
    remove_sidecars(paths);
}

fn remove_sidecars(paths: &IndexPaths) {
    let db = paths.current_db();
    for path in [
        PathBuf::from(format!("{}-wal", db.display())),
        PathBuf::from(format!("{}-shm", db.display())),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove fixture sidecar {}: {error}", path.display()),
        }
    }
}

fn stage_current(project: &TempProject, stamp: Option<&str>) -> IndexPaths {
    let paths = project.paths();
    let lease = create_namespace(&paths);
    create_database(&paths, stamp);
    write_state(
        &paths,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
    );
    drop(lease);
    paths
}

fn stage_state_without_lock(project: &TempProject, phase: &str) -> IndexPaths {
    let paths = project.paths();
    std::fs::create_dir_all(paths.current_root()).expect("create lockless state namespace");
    write_state(
        &paths,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        phase,
    );
    paths
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureStatus {
    Missing,
    Building,
    Uninitialized,
    Outdated,
    Future,
    Corrupt,
}

fn stage_non_current(paths: &IndexPaths, status: FixtureStatus, trap_db: bool) {
    let lease = create_namespace(paths);
    match status {
        FixtureStatus::Missing => {}
        FixtureStatus::Building => write_state(
            paths,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            "building",
        ),
        FixtureStatus::Uninitialized => write_state(
            paths,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            "uninitialized",
        ),
        FixtureStatus::Outdated => write_state(
            paths,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION - 1,
            "current",
        ),
        FixtureStatus::Future => write_state(
            paths,
            CURRENT_STORAGE_PROTOCOL + 1,
            CURRENT_EXTRACTION_VERSION + 1,
            "future-phase",
        ),
        FixtureStatus::Corrupt => {
            std::fs::write(&paths.state_slots()[0], b"not-json").expect("write corrupt state")
        }
    }
    if trap_db {
        std::fs::write(
            paths.current_db(),
            b"not SQLite; opening this trap is a bug",
        )
        .expect("write non-SQLite trap DB");
    }
    drop(lease);
}

fn expected_status(status: FixtureStatus) -> ExtractionStatus {
    match status {
        FixtureStatus::Missing => ExtractionStatus::Missing,
        FixtureStatus::Building => ExtractionStatus::Building {
            built: CURRENT_EXTRACTION_VERSION,
        },
        FixtureStatus::Uninitialized => ExtractionStatus::Uninitialized,
        FixtureStatus::Outdated => ExtractionStatus::Outdated {
            built: CURRENT_EXTRACTION_VERSION - 1,
        },
        FixtureStatus::Future => ExtractionStatus::Future {
            built: CURRENT_EXTRACTION_VERSION + 1,
        },
        FixtureStatus::Corrupt => {
            panic!("Corrupt fixtures carry a typed reason and compare by variant")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotKind {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, SnapshotKind> {
    fn walk(root: &Path, directory: &Path, out: &mut BTreeMap<PathBuf, SnapshotKind>) {
        let mut paths = std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("snapshot read_dir {}: {error}", directory.display()))
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
            let metadata = std::fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("snapshot metadata {}: {error}", path.display()));
            let file_type = metadata.file_type();
            let kind =
                if file_type.is_dir() {
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

#[test]
fn extraction_status_never_opens_sqlite_or_mutates_bytes() {
    let project = TempProject::new("classifier-boundary");
    let paths = project.paths();
    stage_non_current(&paths, FixtureStatus::Building, true);
    let before = snapshot_tree(project.path());

    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Building {
            built: CURRENT_EXTRACTION_VERSION
        }
    );

    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
}

#[test]
fn read_open_rejects_every_non_current_state_without_opening_trap_sqlite() {
    for status in [
        FixtureStatus::Missing,
        FixtureStatus::Building,
        FixtureStatus::Uninitialized,
        FixtureStatus::Outdated,
        FixtureStatus::Future,
        FixtureStatus::Corrupt,
    ] {
        let project = TempProject::new("read-rejected");
        let paths = project.paths();
        stage_non_current(&paths, status, true);
        let before = snapshot_tree(project.path());

        let error = Store::open_for_read(&paths, deadline(), || false)
            .expect_err("every non-Current state must reject read open");
        match (status, error) {
            (FixtureStatus::Missing, StoreError::MissingStateWithDatabase { path }) => {
                assert_eq!(path, paths.current_db());
            }
            (FixtureStatus::Corrupt, StoreError::StateRejected { status }) => {
                assert!(matches!(status, ExtractionStatus::Corrupt { .. }));
            }
            (fixture, StoreError::StateRejected { status }) => {
                assert_eq!(status, expected_status(fixture));
            }
            (_, other) => panic!("unexpected read rejection: {other:?}"),
        }
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn current_read_is_read_only_retains_shared_lease_and_preserves_db_observation() {
    let project = TempProject::new("current-read");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    let before_tree = snapshot_tree(project.path());

    let store = Store::open_for_read(&paths, deadline(), || false).expect("open Current read");
    assert_eq!(
        store.schema_version().expect("read schema version"),
        codegraph_store::migrations::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        store
            .get_project_metadata(EXTRACTION_VERSION_KEY)
            .expect("read exact metadata through retained read Store"),
        Some(CURRENT_EXTRACTION_VERSION.to_string())
    );
    assert_eq!(
        run_exclusive_probe(project.path(), SHORT_DEADLINE),
        "TIMED_OUT",
        "Store must retain its shared lease for the whole SQLite lifetime"
    );
    assert!(
        store
            .stamp_extraction_version()
            .is_err_and(|error| matches!(error, StoreError::StampNotAuthorized))
    );
    assert_snapshot_unchanged(&before_tree, &snapshot_tree(project.path()));
    drop(store);

    assert_eq!(
        run_exclusive_probe(project.path(), CHILD_WAIT),
        "ACQUIRED",
        "dropping Store must close SQLite and release its final lease owner"
    );
    assert_snapshot_unchanged(&before_tree, &snapshot_tree(project.path()));
}

#[test]
fn current_read_and_status_reject_tombstone_without_mutating_any_artifact() {
    let project = TempProject::new("current-tombstoned");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    std::fs::write(paths.tombstone(), b"interrupted-uninit marker")
        .expect("stage Current+tombstone contradiction");
    let before = snapshot_tree(project.path());

    for error in [
        Store::open_for_read(&paths, deadline(), || false)
            .expect_err("Current+tombstone must reject read"),
        Store::open_for_status(&paths, deadline(), || false)
            .expect_err("Current+tombstone must reject status corroboration"),
    ] {
        assert!(matches!(
            error,
            StoreError::CurrentTombstoned { path } if path == paths.tombstone()
        ));
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn current_read_stamp_failures_are_typed_and_byte_nonmutating() {
    for (stamp, expected) in [
        (None, "missing"),
        (Some("02"), "malformed"),
        (Some("1"), "mismatch"),
    ] {
        let project = TempProject::new("bad-stamp");
        let paths = stage_current(&project, stamp);
        let before_tree = snapshot_tree(project.path());

        let error = Store::open_for_read(&paths, deadline(), || false)
            .expect_err("bad extraction stamp must fail closed");
        assert!(matches!(
            (expected, error),
            (
                "missing",
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Missing,
                    ..
                }
            ) | (
                "malformed",
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Malformed { .. },
                    ..
                }
            ) | (
                "mismatch",
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Mismatch {
                        expected: CURRENT_EXTRACTION_VERSION,
                        found: 1
                    },
                    ..
                }
            )
        ));
        assert_snapshot_unchanged(&before_tree, &snapshot_tree(project.path()));
    }
}

#[test]
fn status_reports_non_current_states_without_opening_sqlite() {
    for status in [
        FixtureStatus::Missing,
        FixtureStatus::Building,
        FixtureStatus::Uninitialized,
        FixtureStatus::Outdated,
        FixtureStatus::Future,
        FixtureStatus::Corrupt,
    ] {
        let project = TempProject::new("status-non-current");
        let paths = project.paths();
        stage_non_current(&paths, status, true);
        let before = snapshot_tree(project.path());

        let result = Store::open_for_status(&paths, deadline(), || false);
        if status == FixtureStatus::Missing {
            assert!(matches!(
                result,
                Err(StoreError::MissingStateWithDatabase { path }) if path == paths.current_db()
            ));
            assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
            continue;
        }
        let opened = result.expect("non-Current state with authenticated residue is typed data");
        assert!(!opened.rebuilding);
        assert!(opened.store().is_none());
        if status == FixtureStatus::Corrupt {
            assert!(matches!(
                opened.status,
                Some(ExtractionStatus::Corrupt { .. })
            ));
        } else {
            assert_eq!(opened.status, Some(expected_status(status)));
        }
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn status_missing_without_a_namespace_is_data_and_creates_nothing() {
    let project = TempProject::new("status-absent");
    let paths = project.paths();
    let before = snapshot_tree(project.path());

    let status = Store::open_for_status(&paths, deadline(), || false)
        .expect("fully absent namespace is Missing status data");

    assert_eq!(status.status, Some(ExtractionStatus::Missing));
    assert!(!status.rebuilding);
    assert!(status.store().is_none());
    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
}

#[test]
fn status_current_uses_the_same_read_only_stamp_corroboration() {
    let project = TempProject::new("status-current");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    let before = snapshot_tree(project.path());
    let opened = Store::open_for_status(&paths, deadline(), || false).expect("Current status");
    assert_eq!(opened.status, Some(ExtractionStatus::Current));
    assert!(!opened.rebuilding);
    assert!(opened.store().is_some());
    drop(opened);
    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));

    for (stamp, expected) in [
        (None, "missing"),
        (Some("02"), "malformed"),
        (Some("1"), "mismatch"),
    ] {
        let project = TempProject::new("status-current-bad-stamp");
        let paths = stage_current(&project, stamp);
        let before = snapshot_tree(project.path());
        let error = Store::open_for_status(&paths, deadline(), || false)
            .expect_err("Current status must corroborate its DB stamp");
        assert!(matches!(
            (expected, error),
            (
                "missing",
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Missing,
                    ..
                }
            ) | (
                "malformed",
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Malformed { .. },
                    ..
                }
            ) | (
                "mismatch",
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Mismatch {
                        expected: CURRENT_EXTRACTION_VERSION,
                        found: 1
                    },
                    ..
                }
            )
        ));
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn busy_status_returns_rebuilding_without_opening_sqlite() {
    let project = TempProject::new("status-busy");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    let holder = Holder::spawn(project.path());
    let before = snapshot_tree(project.path());

    let status = Store::open_for_status(&paths, deadline_after(SHORT_DEADLINE), || false)
        .expect("busy writer is typed status data");

    assert_eq!(status.status, None);
    assert!(status.rebuilding);
    assert!(status.store().is_none());
    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    holder.release();
}

#[test]
fn write_purpose_matrix_is_typed_and_rejected_pairs_are_nonmutating() {
    let cases = [
        (
            FixtureStatus::Missing,
            StoreWritePurpose::CurrentMutation,
            "reject",
        ),
        (
            FixtureStatus::Missing,
            StoreWritePurpose::FullRebuild,
            "rebuild",
        ),
        (
            FixtureStatus::Missing,
            StoreWritePurpose::UninitContinuation,
            "reject",
        ),
        (
            FixtureStatus::Building,
            StoreWritePurpose::CurrentMutation,
            "reject",
        ),
        (
            FixtureStatus::Building,
            StoreWritePurpose::FullRebuild,
            "rebuild",
        ),
        (
            FixtureStatus::Building,
            StoreWritePurpose::UninitContinuation,
            "uninit",
        ),
        (
            FixtureStatus::Outdated,
            StoreWritePurpose::CurrentMutation,
            "reject",
        ),
        (
            FixtureStatus::Outdated,
            StoreWritePurpose::FullRebuild,
            "rebuild",
        ),
        (
            FixtureStatus::Outdated,
            StoreWritePurpose::UninitContinuation,
            "reject",
        ),
        (
            FixtureStatus::Uninitialized,
            StoreWritePurpose::CurrentMutation,
            "reject",
        ),
        (
            FixtureStatus::Uninitialized,
            StoreWritePurpose::FullRebuild,
            "rebuild",
        ),
        (
            FixtureStatus::Uninitialized,
            StoreWritePurpose::UninitContinuation,
            "uninit",
        ),
    ];

    for (status, purpose, expected) in cases {
        let project = TempProject::new("write-purpose");
        let paths = project.paths();
        stage_non_current(&paths, status, false);
        let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
            .expect("acquire write matrix lease");
        let before = snapshot_tree(project.path());
        let result = Store::open_for_write(&paths, lease, purpose);
        match (expected, result) {
            (
                "reject",
                Err(StoreError::WritePurposeRejected {
                    purpose: got,
                    status: got_status,
                }),
            ) => {
                assert_eq!(got, purpose);
                assert_eq!(got_status, expected_status(status));
            }
            ("rebuild", Ok(StoreWriteOpen::FullRebuildRequired(authorization))) => {
                assert_eq!(authorization.purpose(), purpose);
                assert_eq!(authorization.status(), &expected_status(status));
                assert!(authorization.retains_exclusive_lease());
                assert_eq!(
                    run_exclusive_probe(project.path(), SHORT_DEADLINE),
                    "TIMED_OUT"
                );
                drop(authorization);
            }
            ("uninit", Ok(StoreWriteOpen::UninitContinuation(authorization))) => {
                assert_eq!(authorization.purpose(), purpose);
                assert_eq!(authorization.status(), &expected_status(status));
                assert!(authorization.retains_exclusive_lease());
                assert_eq!(
                    run_exclusive_probe(project.path(), SHORT_DEADLINE),
                    "TIMED_OUT"
                );
                drop(authorization);
            }
            (_, other) => panic!("unexpected write matrix outcome: {other:?}"),
        }
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn current_state_accepts_only_current_mutation_or_explicit_full_rebuild() {
    let project = TempProject::new("current-purpose-matrix");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));

    let rebuild_lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
        .expect("acquire Current full-rebuild lease");
    let before = snapshot_tree(project.path());
    let StoreWriteOpen::FullRebuildRequired(authorization) =
        Store::open_for_write(&paths, rebuild_lease, StoreWritePurpose::FullRebuild)
            .expect("Current state allows an explicitly requested full rebuild")
    else {
        panic!("Current full rebuild must return opaque authorization");
    };
    assert_eq!(authorization.status(), &ExtractionStatus::Current);
    assert_eq!(authorization.purpose(), StoreWritePurpose::FullRebuild);
    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    drop(authorization);

    let uninit_lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
        .expect("acquire Current continuation rejection lease");
    let before = snapshot_tree(project.path());
    let StoreWriteOpen::UninitContinuation(authorization) =
        Store::open_for_write(&paths, uninit_lease, StoreWritePurpose::UninitContinuation)
            .expect("Current state authorizes a new uninit under the retained lease")
    else {
        panic!("Current uninit must return opaque authorization");
    };
    assert_eq!(authorization.status(), &ExtractionStatus::Current);
    assert_eq!(
        authorization.purpose(),
        StoreWritePurpose::UninitContinuation
    );
    drop(authorization);
    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
}

#[test]
fn current_full_rebuild_requires_the_same_database_corroboration_as_a_read() {
    for (fixture, stamp, expected) in [
        ("missing-db", None, "missing-db"),
        ("tombstone", Some("2"), "tombstone"),
        ("missing-stamp", None, "missing-stamp"),
        ("malformed-stamp", Some("02"), "malformed-stamp"),
        ("mismatched-stamp", Some("1"), "mismatched-stamp"),
    ] {
        let project = TempProject::new(fixture);
        let paths = if fixture == "missing-db" {
            let paths = project.paths();
            let initial = create_namespace(&paths);
            write_state(
                &paths,
                CURRENT_STORAGE_PROTOCOL,
                CURRENT_EXTRACTION_VERSION,
                "current",
            );
            drop(initial);
            paths
        } else {
            let paths = stage_current(&project, stamp);
            if fixture == "tombstone" {
                std::fs::write(paths.tombstone(), b"interrupted uninit")
                    .expect("stage Current tombstone contradiction");
            }
            paths
        };
        let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
            .expect("acquire inconsistent Current rebuild lease");
        let before = snapshot_tree(project.path());

        let error = Store::open_for_write(&paths, lease, StoreWritePurpose::FullRebuild)
            .expect_err("inconsistent Current artifacts must not authorize a full rebuild");
        let matched = match expected {
            "missing-db" => {
                matches!(
                    &error,
                    StoreError::CurrentDatabaseMissing { path } if path == &paths.current_db()
                )
            }
            "tombstone" => matches!(
                &error,
                StoreError::CurrentTombstoned { path } if path == &paths.tombstone()
            ),
            "missing-stamp" => matches!(
                &error,
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Missing,
                    ..
                }
            ),
            "malformed-stamp" => matches!(
                &error,
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Malformed { .. },
                    ..
                }
            ),
            "mismatched-stamp" => matches!(
                &error,
                StoreError::InvalidExtractionStamp {
                    issue: ExtractionStampIssue::Mismatch {
                        expected: CURRENT_EXTRACTION_VERSION,
                        found: 1
                    },
                    ..
                }
            ),
            other => panic!("unknown Current contradiction fixture {other}"),
        };
        assert!(matched, "unexpected {expected} rejection: {error:?}");
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn current_writer_is_state_gated_retains_lease_and_stamps_the_exact_owned_value() {
    let project = TempProject::new("current-writer");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
        .expect("acquire Current writer lease");
    let opened = Store::open_for_write(&paths, lease, StoreWritePurpose::CurrentMutation)
        .expect("open Current writer");
    let StoreWriteOpen::Current(store) = opened else {
        panic!("Current mutation must return an open Store");
    };
    assert_eq!(
        run_exclusive_probe(project.path(), SHORT_DEADLINE),
        "TIMED_OUT"
    );
    store
        .set_project_metadata(EXTRACTION_VERSION_KEY, "1")
        .expect("stage stale stamp under writer lease");
    store
        .stamp_extraction_version()
        .expect("authorized writer stamps exact current version");
    assert_eq!(
        store
            .get_project_metadata(EXTRACTION_VERSION_KEY)
            .expect("read stamped value"),
        Some(CURRENT_EXTRACTION_VERSION.to_string())
    );
    drop(store);
    assert_eq!(run_exclusive_probe(project.path(), CHILD_WAIT), "ACQUIRED");

    let legacy = Store::open(&project.path().join("legacy.db")).expect("legacy Store fixture");
    assert!(matches!(
        legacy.stamp_extraction_version(),
        Err(StoreError::StampNotAuthorized)
    ));
}

#[test]
fn writer_stamp_revalidates_the_retained_fixed_lock_before_metadata_mutation() {
    let project = TempProject::new("stamp-replaced-lock");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
        .expect("acquire stamp fixture lease");
    let StoreWriteOpen::Current(store) =
        Store::open_for_write(&paths, lease, StoreWritePurpose::CurrentMutation)
            .expect("open stamp fixture writer")
    else {
        panic!("Current mutation must return an open Store");
    };
    let displaced = paths.current_root().join("displaced-before-stamp.lock");
    std::fs::rename(paths.permanent_lock(), &displaced).expect("displace retained lock handle");
    std::fs::write(paths.permanent_lock(), b"replacement before stamp")
        .expect("install replacement before stamp");
    let before = snapshot_tree(project.path());

    let error = store
        .stamp_extraction_version()
        .expect_err("stale retained lease must not authorize metadata stamping");
    assert!(matches!(
        error,
        StoreError::LeaseValidation(IndexLeaseValidationError::PermanentLockChanged { .. })
    ));
    assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    drop(store);
}

#[test]
fn write_open_validates_shared_wrong_parent_and_replaced_lock_before_mutation() {
    let shared_project = TempProject::new("write-shared");
    let shared_paths = stage_current(
        &shared_project,
        Some(&CURRENT_EXTRACTION_VERSION.to_string()),
    );
    let shared = IndexLease::acquire_shared_existing(&shared_paths, deadline(), || false)
        .expect("acquire shared fixture lease");
    let before = snapshot_tree(shared_project.path());
    assert!(matches!(
        Store::open_for_write(&shared_paths, shared, StoreWritePurpose::CurrentMutation),
        Err(StoreError::LeaseValidation(
            IndexLeaseValidationError::SharedLease
        ))
    ));
    assert_snapshot_unchanged(&before, &snapshot_tree(shared_project.path()));

    let other_project = TempProject::new("write-wrong-parent");
    let other_paths = stage_current(
        &other_project,
        Some(&CURRENT_EXTRACTION_VERSION.to_string()),
    );
    let wrong = IndexLease::acquire_exclusive_existing(&shared_paths, deadline(), || false)
        .expect("acquire wrong-parent fixture lease");
    let before = snapshot_tree(other_project.path());
    assert!(matches!(
        Store::open_for_write(&other_paths, wrong, StoreWritePurpose::CurrentMutation),
        Err(StoreError::LeaseValidation(
            IndexLeaseValidationError::WrongDbParent
        ))
    ));
    assert_snapshot_unchanged(&before, &snapshot_tree(other_project.path()));

    let replaced_project = TempProject::new("write-replaced-lock");
    let replaced_paths = stage_current(
        &replaced_project,
        Some(&CURRENT_EXTRACTION_VERSION.to_string()),
    );
    let stale = IndexLease::acquire_exclusive_existing(&replaced_paths, deadline(), || false)
        .expect("acquire soon-stale lease");
    let displaced = replaced_paths.current_root().join("displaced-index.lock");
    std::fs::rename(replaced_paths.permanent_lock(), &displaced).expect("displace held lock");
    std::fs::write(replaced_paths.permanent_lock(), b"replacement lock")
        .expect("install replacement lock");
    let before = snapshot_tree(replaced_project.path());
    assert!(matches!(
        Store::open_for_write(&replaced_paths, stale, StoreWritePurpose::CurrentMutation),
        Err(StoreError::LeaseValidation(
            IndexLeaseValidationError::PermanentLockChanged { .. }
        ))
    ));
    assert_snapshot_unchanged(&before, &snapshot_tree(replaced_project.path()));
    let fresh = IndexLease::acquire_exclusive_existing(&replaced_paths, deadline(), || false)
        .expect("replacement fixed lock is independently acquirable");
    drop(fresh);
}

#[test]
fn future_and_corrupt_writer_opens_are_typed_and_byte_nonmutating_for_every_purpose() {
    for status in [FixtureStatus::Future, FixtureStatus::Corrupt] {
        let project = TempProject::new("write-refused-state");
        let paths = project.paths();
        stage_non_current(&paths, status, true);
        for purpose in [
            StoreWritePurpose::CurrentMutation,
            StoreWritePurpose::FullRebuild,
            StoreWritePurpose::UninitContinuation,
        ] {
            let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
                .expect("acquire refusal fixture lease");
            let before = snapshot_tree(project.path());
            let error = Store::open_for_write(&paths, lease, purpose)
                .expect_err("Future/Corrupt writer open must fail closed");
            match (status, error) {
                (FixtureStatus::Future, StoreError::StateRejected { status }) => {
                    assert_eq!(status, expected_status(FixtureStatus::Future));
                }
                (FixtureStatus::Corrupt, StoreError::StateRejected { status }) => {
                    assert!(matches!(status, ExtractionStatus::Corrupt { .. }));
                }
                (_, other) => panic!("unexpected refused writer error: {other:?}"),
            }
            assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
        }
    }
}

#[test]
fn missing_state_with_any_existing_database_artifact_fails_closed() {
    for suffix in ["", "-wal", "-shm"] {
        let project = TempProject::new("missing-with-artifact");
        let paths = project.paths();
        let initial = create_namespace(&paths);
        drop(initial);
        let db = paths.current_db();
        let artifact = PathBuf::from(format!("{}{suffix}", db.display()));
        std::fs::write(&artifact, b"reserved unknown database artifact")
            .expect("write reserved DB artifact");
        let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
            .expect("acquire Missing fixture lease");
        let before = snapshot_tree(project.path());

        let error = Store::open_for_write(&paths, lease, StoreWritePurpose::FullRebuild)
            .expect_err("Missing plus DB artifact is not a fresh writable namespace");
        assert!(matches!(
            error,
            StoreError::MissingStateWithDatabase { path } if path == artifact
        ));
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

#[test]
fn missing_state_with_database_artifact_rejects_read_and_status_with_or_without_lock() {
    for with_lock in [false, true] {
        for suffix in ["", "-wal", "-shm"] {
            let project = TempProject::new("missing-artifact-read-status");
            let paths = project.paths();
            if with_lock {
                drop(create_namespace(&paths));
            } else {
                std::fs::create_dir_all(paths.current_root())
                    .expect("create lockless artifact namespace");
            }
            let artifact = PathBuf::from(format!("{}{suffix}", paths.current_db().display()));
            std::fs::write(&artifact, b"reserved unknown database artifact")
                .expect("stage unknown database artifact");
            let before = snapshot_tree(project.path());

            for error in [
                Store::open_for_read(&paths, deadline(), || false)
                    .expect_err("Missing artifact must reject read"),
                Store::open_for_status(&paths, deadline(), || false)
                    .expect_err("Missing artifact must reject status"),
            ] {
                assert!(matches!(
                    error,
                    StoreError::MissingStateWithDatabase { path } if path == artifact
                ));
                assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
            }
        }
    }
}

#[test]
fn persisted_state_without_permanent_lock_is_typed_and_byte_nonmutating() {
    for (phase, expected) in [
        ("current", ExtractionStatus::Current),
        (
            "building",
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            },
        ),
        ("uninitialized", ExtractionStatus::Uninitialized),
    ] {
        let project = TempProject::new("state-without-lock");
        let paths = stage_state_without_lock(&project, phase);
        let before = snapshot_tree(project.path());

        for error in [
            Store::open_for_read(&paths, deadline(), || false)
                .expect_err("persisted lockless state must reject read"),
            Store::open_for_status(&paths, deadline(), || false)
                .expect_err("persisted lockless state must reject status"),
        ] {
            assert!(matches!(
                error,
                StoreError::StateWithoutPermanentLock { status, path }
                    if status == expected && path == paths.permanent_lock()
            ));
            assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
        }
    }
}

#[test]
fn current_with_committed_wal_fails_closed_without_ignoring_or_mutating_sidecars() {
    let project = TempProject::new("current-committed-wal");
    let paths = stage_current(&project, Some(&CURRENT_EXTRACTION_VERSION.to_string()));
    run_crashed_current_writer(project.path());
    let wal_path = PathBuf::from(format!("{}-wal", paths.current_db().display()));
    assert!(
        std::fs::metadata(&wal_path)
            .expect("committed WAL exists")
            .len()
            > 0,
        "fixture must carry committed WAL frames"
    );
    let live = Connection::open_with_flags(paths.current_db(), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open live WAL-aware proof connection");
    assert_eq!(
        live.query_row(
            "SELECT value FROM project_metadata WHERE key = 'wal_only_probe'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("live SQLite observes committed WAL data"),
        Some("committed".to_string())
    );
    drop(live);

    let proof = TempProject::new("main-image-proof");
    let main_only = proof.path().join("main-only.db");
    let mut bytes = std::fs::read(paths.current_db()).expect("read checkpointed main DB image");
    assert!(bytes.len() >= 20 && bytes.starts_with(b"SQLite format 3\0"));
    bytes[18] = 1;
    bytes[19] = 1;
    std::fs::write(&main_only, bytes).expect("write private main-only proof image");
    let main_conn = Connection::open_with_flags(&main_only, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open main-only proof image");
    let main_value = main_conn
        .query_row(
            "SELECT value FROM project_metadata WHERE key = 'wal_only_probe'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("query main-only proof image");
    assert_eq!(
        main_value, None,
        "deserializing only main-file bytes would silently miss committed WAL data"
    );

    let before = snapshot_tree(project.path());
    for error in [
        Store::open_for_read(&paths, deadline(), || false)
            .expect_err("Current with committed WAL must fail closed"),
        Store::open_for_status(&paths, deadline(), || false)
            .expect_err("Current status with committed WAL must fail closed"),
    ] {
        assert!(matches!(
            error,
            StoreError::CurrentWithDatabaseSidecar { path } if path == wal_path
        ));
        assert_snapshot_unchanged(&before, &snapshot_tree(project.path()));
    }
}

/// Child-process entry point used by deterministic lock contention tests.
#[test]
fn store_gate_child_process() {
    let Ok(action) = std::env::var(CHILD_ACTION) else {
        return;
    };
    let project = PathBuf::from(std::env::var_os(CHILD_PROJECT).expect("child project env"));
    let paths = IndexPaths::resolve(&project, None).expect("child resolve IndexPaths");
    match action.as_str() {
        "hold-exclusive" => {
            let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
                .expect("holder acquires exclusive lease");
            println!("READY");
            std::io::stdout().flush().expect("flush READY");
            let mut release = [0_u8; 1];
            std::io::stdin()
                .read_exact(&mut release)
                .expect("read release byte");
            drop(lease);
            println!("RELEASED");
            std::io::stdout().flush().expect("flush RELEASED");
        }
        "probe-exclusive" => match IndexLease::acquire_exclusive_existing(
            &paths,
            deadline_after(SHORT_DEADLINE),
            || false,
        ) {
            Ok(lease) => {
                println!("ACQUIRED");
                drop(lease);
            }
            Err(codegraph_store::IndexLeaseError::TimedOut { .. }) => println!("TIMED_OUT"),
            Err(error) => panic!("unexpected child probe error: {error}"),
        },
        "write-current-wal-and-exit" => {
            let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
                .expect("crash fixture acquires exclusive lease");
            let StoreWriteOpen::Current(store) =
                Store::open_for_write(&paths, lease, StoreWritePurpose::CurrentMutation)
                    .expect("crash fixture opens state-gated Current writer")
            else {
                panic!("Current mutation must return an open Store");
            };
            store
                .connection()
                .pragma_update(None, "wal_autocheckpoint", 0)
                .expect("disable WAL autocheckpoint in crash fixture");
            store
                .set_project_metadata("wal_only_probe", "committed")
                .expect("commit WAL-only probe through state-gated writer");
            println!("WAL_COMMITTED");
            std::io::stdout().flush().expect("flush WAL sentinel");
            std::process::exit(91);
        }
        other => panic!("unknown Store gate child action {other}"),
    }
}

struct Holder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    tail: Receiver<String>,
}

impl Holder {
    fn spawn(project: &Path) -> Self {
        let mut child = child_command(project, "hold-exclusive")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn Store lock holder");
        let stdin = child.stdin.take().expect("holder stdin");
        let stdout = child.stdout.take().expect("holder stdout");
        let (ready_tx, ready_rx) = mpsc::channel();
        let (tail_tx, tail_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line).expect("read holder output");
                assert_ne!(read, 0, "holder exited before READY");
                if line.trim() == "READY" {
                    ready_tx.send(line).expect("send holder READY");
                    break;
                }
            }
            let mut tail = String::new();
            reader.read_to_string(&mut tail).expect("read holder tail");
            tail_tx.send(tail).expect("send holder tail");
        });
        let ready = ready_rx
            .recv_timeout(CHILD_WAIT)
            .expect("holder READY before finite deadline");
        assert_eq!(ready.trim(), "READY");
        Self {
            child: Some(child),
            stdin: Some(stdin),
            tail: tail_rx,
        }
    }

    fn release(mut self) {
        let mut stdin = self.stdin.take().expect("holder release stdin");
        stdin.write_all(b"x").expect("signal holder release");
        drop(stdin);
        let child = self.child.as_mut().expect("holder child");
        let status = wait_bounded(child, CHILD_WAIT);
        assert!(status.success(), "holder child failed: {status}");
        let tail = self
            .tail
            .recv_timeout(CHILD_WAIT)
            .expect("holder tail before finite deadline");
        assert!(
            tail.lines().any(|line| line == "RELEASED"),
            "holder emitted no RELEASED sentinel: {tail:?}"
        );
        self.child.take();
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn child_command(project: &Path, action: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg("store_gate_child_process")
        .arg("--nocapture")
        .env(CHILD_ACTION, action)
        .env(CHILD_PROJECT, project);
    command
}

fn run_exclusive_probe(project: &Path, process_bound: Duration) -> String {
    let mut child = child_command(project, "probe-exclusive")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn Store lock probe");
    let mut stdout = child.stdout.take().expect("probe stdout");
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        stdout
            .read_to_string(&mut output)
            .expect("read probe stdout");
        output_tx.send(output).expect("send probe output");
    });
    let status = wait_bounded(
        &mut child,
        process_bound
            .checked_add(CHILD_WAIT)
            .expect("probe process bound"),
    );
    assert!(status.success(), "probe child failed: {status}");
    output_rx
        .recv_timeout(CHILD_WAIT)
        .expect("probe output before finite deadline")
        .lines()
        .find(|line| matches!(*line, "ACQUIRED" | "TIMED_OUT"))
        .unwrap_or_else(|| panic!("probe emitted no result sentinel"))
        .to_string()
}

fn run_crashed_current_writer(project: &Path) {
    let mut child = child_command(project, "write-current-wal-and-exit")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn state-gated WAL crash fixture");
    let mut stdout = child.stdout.take().expect("WAL fixture stdout");
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        stdout
            .read_to_string(&mut output)
            .expect("read WAL fixture stdout");
        output_tx.send(output).expect("send WAL fixture output");
    });
    let status = wait_bounded(&mut child, CHILD_WAIT);
    assert_eq!(
        status.code(),
        Some(91),
        "WAL fixture must terminate without running Rust/SQLite destructors"
    );
    let output = output_rx
        .recv_timeout(CHILD_WAIT)
        .expect("WAL fixture output before finite deadline");
    assert!(
        output.lines().any(|line| line == "WAL_COMMITTED"),
        "WAL fixture emitted no commit sentinel: {output:?}"
    );
}

fn wait_bounded(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = deadline_after(timeout);
    loop {
        if let Some(status) = child.try_wait().expect("poll child status") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child process exceeded finite {timeout:?} bound");
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
