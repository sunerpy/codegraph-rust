//! Migrating a current index one schema behind (W5-09e).
//!
//! `tests/fixtures/upgrade_schema_9/codegraph.db` is the schema-9 database a
//! release build of 3878d52 (extraction version 22, the current one) wrote for
//! the mini corpus. Each test plants it in a fresh project namespace as a
//! `Current` index, edits a file other files depend on, and syncs it through
//! one of the two incremental paths: `codegraph sync` from the shell, and the
//! watcher's sync of the changed paths. Both must migrate the schema to the
//! current one in place, without a rebuild, leave the stored-references marker
//! unwritten (the migration creates `resolved_refs` empty, so the dependents
//! are extracted), and leave a canonical graph equal to a fresh `init` of the
//! edited tree. A full `index` afterwards writes the marker.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};
use codegraph_core::IndexPaths;
use codegraph_store::migrations::CURRENT_SCHEMA_VERSION;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, ExtractionStatus, Store, checksum_hex,
};

/// The edit: one line above everything `src/app.ts` imports from it.
const MATH_EDITED: &str = "// one line down\nexport function add(left: number, right: number): number {\n  return left + right;\n}\n\nexport class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }\n}\n";

#[test]
fn cli_sync_migrates_a_schema_9_index_in_place() {
    let dir = TestDir::new("cli");
    let project = plant_schema_9_index(&dir);
    fs::write(project.join("src/math.ts"), MATH_EDITED).unwrap();

    let sync = cli(&["sync", path_str(&project)]);
    assert!(
        sync.ok,
        "sync over a schema-9 index failed:\nstdout:\n{}\nstderr:\n{}",
        sync.stdout, sync.stderr
    );
    let report = format!("{}{}", sync.stdout, sync.stderr);
    assert!(
        report.contains("1 reindexed, 2 skipped (unchanged)"),
        "an incremental sync, not a rebuild: {report}"
    );

    assert_migrated_like_a_fresh_init(&project, "cli");
    assert_full_index_writes_the_marker(&project);
}

#[test]
fn watcher_sync_migrates_a_schema_9_index_in_place() {
    let dir = TestDir::new("watcher");
    let project = plant_schema_9_index(&dir);
    fs::write(project.join("src/math.ts"), MATH_EDITED).unwrap();

    let db = index_paths(&project).current_db();
    let outcome = codegraph_watch::sync_changed_paths(&project, &db, ["src/math.ts"])
        .expect("watcher sync over a schema-9 index");
    assert_eq!(outcome.files_reindexed, 1, "an incremental sync");
    assert_eq!(outcome.changed_paths, ["src/math.ts"]);

    assert_migrated_like_a_fresh_init(&project, "watcher");
    assert_full_index_writes_the_marker(&project);
}

/// Copy the mini corpus into `dir`, create a namespace with the current binary,
/// then replace its database with the schema-9 one and publish it as a current
/// index of this extraction version.
fn plant_schema_9_index(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("project");
    copy_tree(&mini_fixture(), &project);
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init failed: {}", init.stderr);

    let paths = index_paths(&project);
    let db = paths.current_db();
    for sidecar in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{sidecar}", db.display())));
    }
    fs::copy(schema_9_database(), &db).expect("plant the schema-9 database");
    stage_state_slot(
        &paths,
        2,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Current,
        "the planted namespace must classify as current"
    );
    let conn = open_immutable(&db);
    assert_eq!(schema_version(&conn), 9, "the fixture is a schema-9 index");
    assert!(!has_table(&conn, "resolved_refs"));
    project
}

fn assert_migrated_like_a_fresh_init(project: &Path, label: &str) {
    let paths = index_paths(project);
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Current,
        "the migrated namespace must stay current"
    );
    let conn = open_immutable(&paths.current_db());
    assert_eq!(
        schema_version(&conn),
        CURRENT_SCHEMA_VERSION,
        "schema must be current"
    );
    assert!(has_table(&conn, "resolved_refs"), "migration 10 ran");
    assert_eq!(
        marker(&conn),
        None,
        "a migrated index is not trusted for stored references"
    );
    drop(conn);

    let peer_dir = TestDir::new(&format!("{label}-fresh"));
    let peer = peer_dir.path().join("project");
    copy_tree(&mini_fixture(), &peer);
    fs::write(peer.join("src/math.ts"), MATH_EDITED).unwrap();
    let init = cli(&["init", path_str(&peer)]);
    assert!(init.ok, "fresh init failed: {}", init.stderr);

    let mut migrated = canonicalize_db(&paths.current_db()).expect("canonicalize migrated index");
    let fresh =
        canonicalize_db(&index_paths(&peer).current_db()).expect("canonicalize fresh index");
    // A migration creates its objects after the ones already there, so only
    // the order of the schema dump differs from a fresh index's.
    let statements = |schema: &str| {
        let mut statements = schema.split(";\n").map(str::to_string).collect::<Vec<_>>();
        statements.sort();
        statements
    };
    assert_eq!(
        statements(&migrated.schema),
        statements(&fresh.schema),
        "the migrated schema holds a fresh index's objects"
    );
    migrated.schema.clone_from(&fresh.schema);
    if let Err(report) = diff_canonical(&fresh, &migrated, None) {
        panic!("the migrated index differs from a fresh init:\n{report}");
    }
}

fn assert_full_index_writes_the_marker(project: &Path) {
    let index = cli(&["index", path_str(project)]);
    assert!(index.ok, "index failed: {}", index.stderr);
    let conn = open_immutable(&index_paths(project).current_db());
    assert!(
        marker(&conn).is_some_and(|marker| marker.starts_with("1:")),
        "a full index writes the marker"
    );
}

/// The stored-references marker (`codegraph_watch::resolved_refs::VERSION_KEY`).
fn marker(conn: &rusqlite::Connection) -> Option<String> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT value FROM project_metadata WHERE key = ?1",
        [codegraph_watch::resolved_refs::VERSION_KEY],
        |row| row.get(0),
    )
    .optional()
    .expect("read the marker")
}

fn schema_version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
        row.get(0)
    })
    .expect("schema version")
}

fn has_table(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .expect("read sqlite_master")
        > 0
}

/// `immutable=1` takes no lock and leaves no `-wal`/`-shm` beside the index.
fn open_immutable(db: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        format!("{}?immutable=1", sqlite_file_uri(db)),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open index")
}

/// `path` as an absolute SQLite `file:` URI: a Windows verbatim prefix (`\\?\`)
/// is dropped and every byte outside the unreserved set is percent-encoded.
fn sqlite_file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn schema_9_database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade_schema_9/codegraph.db")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn index_paths(project: &Path) -> IndexPaths {
    IndexPaths::resolve(project, None).expect("resolve index paths")
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 test path")
}

/// Author one authoritative state slot and remove its companion, the same way
/// `upgrade_from_v054.rs` stages protocol records.
fn stage_state_slot(
    paths: &IndexPaths,
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
) {
    let identity = paths.project_identity();
    let checksum = checksum_hex(
        sequence,
        storage_protocol,
        extraction_version,
        phase,
        identity,
    );
    let body = format!(
        "{{\"sequence\":{sequence},\"storageProtocol\":{storage_protocol},\
         \"extractionVersion\":{extraction_version},\"phase\":\"{phase}\",\
         \"projectIdentity\":\"{identity}\",\"checksum\":\"{checksum}\"}}\n"
    );
    let [slot0, slot1] = paths.state_slots();
    fs::write(&slot0, body).expect("stage authoritative state slot");
    let _ = fs::remove_file(&slot1);
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn cli(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env_remove("CODEGRAPH_DIR")
        .output()
        .expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-upgrade-schema-9-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
