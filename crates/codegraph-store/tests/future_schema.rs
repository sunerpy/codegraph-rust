//! An index a newer codegraph wrote (OD-19): its `schema_versions` records a
//! schema above this binary's. Every open that would read, write or replace
//! the database refuses before it changes a byte, including a version that is
//! only in the write-ahead log; `uninit` still removes it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::migrations::CURRENT_SCHEMA_VERSION;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, EXTRACTION_VERSION_KEY, ExtractionStatus,
    IndexLease, RebuildError, RebuildKind, Store, StoreError, StoreWriteOpen, StoreWritePurpose,
    begin_full_rebuild, checksum_hex, resume_full_rebuild, uninit_index,
};
use rusqlite::Connection;
use serde_json::json;

const NEWER: i64 = CURRENT_SCHEMA_VERSION + 1;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempProject(PathBuf);

impl TempProject {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codegraph-future-schema-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("create temp project {}: {error}", path.display()));
        Self(path.canonicalize().expect("canonical temp project"))
    }

    fn paths(&self) -> IndexPaths {
        IndexPaths::resolve(&self.0, None).expect("resolve test paths")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn state_bytes(phase: &str, owner: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "sequence": 1,
        "storageProtocol": CURRENT_STORAGE_PROTOCOL,
        "extractionVersion": CURRENT_EXTRACTION_VERSION,
        "phase": phase,
        "projectIdentity": owner,
        "checksum": checksum_hex(1, CURRENT_STORAGE_PROTOCOL, CURRENT_EXTRACTION_VERSION, phase, owner),
    }))
    .expect("serialize state fixture")
}

fn write_state(paths: &IndexPaths, phase: &str) {
    std::fs::write(
        &paths.state_slots()[0],
        state_bytes(phase, paths.project_identity()),
    )
    .expect("write state fixture");
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut path = db.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

/// A current-format database at `db` (this binary's schema, the current
/// extraction stamp), closed and without sidecars.
fn create_database(db: &Path) {
    let store = Store::open(db).expect("create database fixture");
    store
        .set_project_metadata(
            EXTRACTION_VERSION_KEY,
            &CURRENT_EXTRACTION_VERSION.to_string(),
        )
        .expect("stamp database fixture");
    store
        .restore_default_pragmas()
        .expect("checkpoint database fixture");
    drop(store);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar(db, suffix));
    }
}

/// Record schema `NEWER` in `db`, checkpointed into the main file.
fn record_newer_schema(db: &Path) {
    let conn = Connection::open(db).expect("open database fixture");
    conn.execute(
        "INSERT INTO schema_versions (version, applied_at, description) VALUES (?1, 0, 'newer')",
        [NEWER],
    )
    .expect("record the newer schema");
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar(db, suffix));
    }
}

/// Record schema `NEWER` in `db`'s write-ahead log only, as a newer binary
/// killed before its checkpoint leaves it: the main file still records this
/// binary's schema.
fn record_newer_schema_in_the_log_only(db: &Path) {
    let conn = Connection::open(db).expect("open database fixture");
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .expect("keep the log");
    conn.execute(
        "INSERT INTO schema_versions (version, applied_at, description) VALUES (?1, 0, 'newer')",
        [NEWER],
    )
    .expect("record the newer schema");
    let main = std::fs::read(db).expect("read main file");
    let wal = std::fs::read(sidecar(db, "-wal")).expect("read log");
    drop(conn);
    std::fs::write(db, main).expect("restore main file");
    std::fs::write(sidecar(db, "-wal"), wal).expect("restore log");
    let _ = std::fs::remove_file(sidecar(db, "-shm"));
}

/// Every file under the index root: its length and a digest of its bytes, so a
/// failed comparison names the changed files instead of printing them.
fn snapshot(paths: &IndexPaths) -> BTreeMap<PathBuf, (usize, u64)> {
    use std::hash::{Hash, Hasher};
    let mut files = BTreeMap::new();
    let mut pending = vec![paths.current_root().to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.expect("read index root entry");
            let path = entry.path();
            if entry.file_type().expect("entry type").is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("read index file");
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut hasher);
                files.insert(path, (bytes.len(), hasher.finish()));
            }
        }
    }
    files
}

fn stage_current(project: &TempProject) -> IndexPaths {
    let paths = project.paths();
    let lease =
        IndexLease::create_exclusive(&paths, deadline(), || false).expect("create namespace");
    create_database(&paths.current_db());
    write_state(&paths, "current");
    drop(lease);
    paths
}

/// A `FutureSchema` refusal for `NEWER`, offering `uninit --force` exactly
/// when `discard` (the states uninit accepts).
fn assert_future_schema(error: &StoreError, discard: bool) {
    match error {
        StoreError::FutureSchema {
            found,
            supported,
            discard_with_uninit,
            ..
        } => {
            assert_eq!(*found, NEWER);
            assert_eq!(*supported, CURRENT_SCHEMA_VERSION);
            assert_eq!(*discard_with_uninit, discard, "{error}");
            assert_eq!(error.to_string().contains("uninit"), discard, "{error}");
        }
        other => panic!("expected FutureSchema, got {other:?}"),
    }
}

/// An unreadable version says nothing was changed and where to look next.
fn assert_unreadable_message(error: &StoreError) {
    let text = error.to_string();
    assert!(
        text.contains("nothing was changed") && text.contains("codegraph status"),
        "{text}"
    );
}

fn assert_rebuild_refused(
    result: Result<codegraph_store::FullRebuild, RebuildError>,
    discard: bool,
) {
    match result {
        Err(RebuildError::Store(error)) => assert_future_schema(&error, discard),
        Err(other) => panic!("expected a FutureSchema refusal, got {other:?}"),
        Ok(_) => panic!("the rebuild replaced a newer codegraph's index"),
    }
}

#[test]
fn reads_and_writes_refuse_a_newer_schema_without_changing_a_byte() {
    let project = TempProject::new("current");
    let paths = stage_current(&project);
    record_newer_schema(&paths.current_db());
    let before = snapshot(&paths);

    let Err(error) = Store::open_for_read(&paths, deadline(), || false) else {
        panic!("a read opened a newer codegraph's index");
    };
    assert_future_schema(&error, true);
    let Err(error) = Store::open_for_status(&paths, deadline(), || false) else {
        panic!("status opened a newer codegraph's index");
    };
    assert_future_schema(&error, true);
    for purpose in [
        StoreWritePurpose::CurrentMutation,
        StoreWritePurpose::IncrementalSync,
        StoreWritePurpose::FullRebuild,
    ] {
        let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
            .expect("acquire the namespace");
        let error = Store::open_for_write(&paths, lease, purpose)
            .err()
            .unwrap_or_else(|| panic!("{purpose} opened a newer codegraph's index"));
        assert_future_schema(&error, true);
    }
    assert_eq!(snapshot(&paths), before, "a refused open changed the index");
}

#[test]
fn an_unreadable_schema_version_fails_closed() {
    let project = TempProject::new("unreadable");
    let paths = stage_current(&project);
    let conn = Connection::open(paths.current_db()).unwrap();
    conn.execute_batch(
        "DROP TABLE schema_versions;
         CREATE TABLE schema_versions (version TEXT, applied_at INTEGER, description TEXT);
         INSERT INTO schema_versions VALUES ('ten', 0, 'not a number');
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar(&paths.current_db(), suffix));
    }
    let before = snapshot(&paths);
    match Store::open_for_read(&paths, deadline(), || false) {
        Err(error @ StoreError::ReadSchemaVersion { .. }) => assert_unreadable_message(&error),
        other => panic!("expected ReadSchemaVersion, got {:?}", other.map(|_| ())),
    }
    assert_eq!(snapshot(&paths), before);
}

#[test]
fn an_explicit_init_refuses_a_newer_schema() {
    let project = TempProject::new("init");
    let paths = stage_current(&project);
    record_newer_schema(&paths.current_db());
    let before = snapshot(&paths);
    assert_rebuild_refused(
        begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false),
        true,
    );
    assert_eq!(snapshot(&paths), before);
}

#[test]
fn a_copied_project_is_refused_before_its_state_slots_are_removed() {
    let project = TempProject::new("copied");
    let paths = stage_current(&project);
    record_newer_schema(&paths.current_db());
    let other = TempProject::new("copied-from");
    let owner = other.paths().project_identity().to_string();
    std::fs::write(&paths.state_slots()[0], state_bytes("current", &owner)).unwrap();
    assert!(matches!(
        Store::extraction_status(&paths),
        ExtractionStatus::Corrupt { .. }
    ));
    let before = snapshot(&paths);
    assert_rebuild_refused(
        begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false),
        false,
    );
    assert_eq!(
        snapshot(&paths),
        before,
        "the copied project's slots must stay"
    );
}

#[test]
fn a_lockless_root_is_refused_before_it_gets_a_lock() {
    let project = TempProject::new("lockless");
    let paths = project.paths();
    std::fs::create_dir_all(paths.current_root()).unwrap();
    create_database(&paths.current_db());
    record_newer_schema(&paths.current_db());
    assert!(!paths.permanent_lock().exists());
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Missing);
    let before = snapshot(&paths);
    assert_rebuild_refused(
        begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false),
        false,
    );
    assert!(
        !paths.permanent_lock().exists(),
        "the refusal must come before the lock is created"
    );
    assert_eq!(snapshot(&paths), before);
}

#[test]
fn a_newer_schema_only_in_the_log_is_refused_by_init_and_by_a_sync_migration() {
    // Missing, with the lock: an explicit init.
    let project = TempProject::new("log-missing");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    create_database(&paths.current_db());
    drop(lease);
    record_newer_schema_in_the_log_only(&paths.current_db());
    let before = snapshot(&paths);
    assert_rebuild_refused(
        begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false),
        false,
    );
    assert_eq!(snapshot(&paths), before);

    // An interrupted build: the migration an incremental sync escalates to.
    let project = TempProject::new("log-building");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    create_database(&paths.current_db());
    write_state(&paths, "building");
    drop(lease);
    record_newer_schema_in_the_log_only(&paths.current_db());
    let before = snapshot(&paths);
    let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false).unwrap();
    let authorization =
        match Store::open_for_write(&paths, lease, StoreWritePurpose::IncrementalSync) {
            Ok(StoreWriteOpen::FullRebuildRequired(authorization)) => authorization,
            Ok(other) => panic!("expected a migration, got {other:?}"),
            Err(error) => panic!("expected a migration, got {error:?}"),
        };
    assert_rebuild_refused(resume_full_rebuild(&paths, authorization), true);
    assert_eq!(
        snapshot(&paths),
        before,
        "`building` must not be republished"
    );
}

#[test]
fn garbage_in_a_missing_namespace_is_still_replaced() {
    let project = TempProject::new("garbage");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    std::fs::write(paths.current_db(), [7u8; 4096]).unwrap();
    drop(lease);
    let rebuild = begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false)
        .expect("a file no codegraph wrote is replaced as before");
    assert!(!paths.current_db().exists(), "the rebuild removed it");
    drop(rebuild);
}

#[test]
fn a_damaged_database_in_a_missing_namespace_is_refused() {
    let project = TempProject::new("damaged");
    let paths = project.paths();
    let lease = IndexLease::create_exclusive(&paths, deadline(), || false).unwrap();
    create_database(&paths.current_db());
    drop(lease);
    let db = paths.current_db();
    let mut bytes = std::fs::read(&db).unwrap();
    let page_size = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
    // Every page after the first: the schema survives, the tables do not.
    for page in bytes.chunks_mut(page_size).skip(1) {
        page[..8].fill(0xff);
    }
    std::fs::write(&db, &bytes).unwrap();
    let before = snapshot(&paths);
    match begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false) {
        Err(RebuildError::Store(error @ StoreError::ReadSchemaVersion { .. })) => {
            assert_unreadable_message(&error)
        }
        Err(other) => panic!("expected ReadSchemaVersion, got {other:?}"),
        Ok(_) => panic!("the rebuild replaced a database it could not read"),
    }
    assert_eq!(snapshot(&paths), before);
}

/// A `Current` index beside the tombstone an `init` could not remove, with a
/// newer schema: `init` refuses it unchanged and offers `uninit --force`, and
/// that command then discards it.
#[test]
fn a_tombstoned_newer_schema_index_is_refused_then_discarded_by_uninit() {
    let project = TempProject::new("current-tombstone");
    let paths = stage_current(&project);
    record_newer_schema(&paths.current_db());
    std::fs::write(paths.tombstone(), b"remove failed").expect("stage the tombstone");
    let before = snapshot(&paths);

    // The newer schema takes precedence over the tombstone, whose own
    // recovery (`init`) would refuse it.
    let Err(error) = Store::open_for_read(&paths, deadline(), || false) else {
        panic!("a read opened a tombstoned newer codegraph's index");
    };
    assert_future_schema(&error, true);
    let Err(error) = Store::open_for_status(&paths, deadline(), || false) else {
        panic!("status opened a tombstoned newer codegraph's index");
    };
    assert_future_schema(&error, true);
    assert_rebuild_refused(
        begin_full_rebuild(&paths, RebuildKind::ExplicitInit, deadline(), || false),
        true,
    );
    assert_eq!(snapshot(&paths), before, "the refusal changed the index");

    uninit_index(&paths, deadline(), || false).expect("uninit --force discards it");
    assert!(!paths.current_db().exists());
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Uninitialized
    );
}

#[test]
fn uninit_still_removes_a_newer_schema_index() {
    for phase in ["current", "building"] {
        let project = TempProject::new(&format!("uninit-{phase}"));
        let paths = stage_current(&project);
        record_newer_schema(&paths.current_db());
        write_state(&paths, phase);
        uninit_index(&paths, deadline(), || false)
            .unwrap_or_else(|error| panic!("uninit of a {phase} newer index: {error}"));
        assert!(
            !paths.current_db().exists(),
            "uninit removed the {phase} database"
        );
    }
}

/// A newer schema only in the log of a `Current` index: the writers that
/// tolerate a live log (an incremental sync, the daemon's recovery of a dead
/// owner's log) refuse before their writable open, so no `-shm` appears and no
/// checkpoint folds the log; a read refuses the log as today.
#[test]
fn a_current_index_with_a_newer_schema_only_in_its_log_is_never_touched() {
    let project = TempProject::new("current-log");
    let paths = stage_current(&project);
    record_newer_schema_in_the_log_only(&paths.current_db());
    let before = snapshot(&paths);
    assert!(
        !sidecar(&paths.current_db(), "-shm").exists(),
        "the fixture starts without -shm"
    );

    let lease = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false).unwrap();
    let Err(error) = Store::open_for_write(&paths, lease, StoreWritePurpose::IncrementalSync)
    else {
        panic!("an incremental sync opened a newer codegraph's index");
    };
    assert_future_schema(&error, true);
    assert_eq!(snapshot(&paths), before, "the sync touched the index");

    let Err(error) = Store::recover_stale_current_sidecars(&paths, deadline(), || false) else {
        panic!("the recovery folded a newer codegraph's log");
    };
    assert_future_schema(&error, true);
    assert_eq!(snapshot(&paths), before, "the recovery touched the index");

    assert!(Store::open_for_read(&paths, deadline(), || false).is_err());
    assert_eq!(snapshot(&paths), before, "the read touched the index");

    uninit_index(&paths, deadline(), || false).expect("uninit still removes it");
    assert!(!paths.current_db().exists());
}
