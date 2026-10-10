//! Upgrading an index the previous release wrote.
//!
//! `tests/fixtures/upgrade_v0_54/codegraph.db` is the schema-8 database the
//! released v0.54.0 binary (extraction version 21) wrote for the mini corpus.
//! Each test plants it in a fresh project namespace, stages the state slot that
//! release publishes (`phase=current`, extraction version 21), and upgrades it
//! through one of the two paths a user hits: `codegraph sync` from the shell, and
//! the catch-up sync a daemon or MCP session runs when it first opens the
//! project. Both must classify the namespace as outdated, rebuild it from source
//! exactly once, and leave a current schema-9 index whose canonical graph equals
//! a fresh `init` of the same tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};
use codegraph_core::IndexPaths;
use codegraph_store::migrations::CURRENT_SCHEMA_VERSION;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, ExtractionStatus, Store, checksum_hex,
};

/// Extraction version the v0.54.0 release published.
const V0_54_EXTRACTION_VERSION: u64 = 21;

#[test]
fn cli_sync_upgrades_a_v0_54_index_to_the_current_schema_and_extraction() {
    let dir = TestDir::new("cli");
    let project = plant_v0_54_index(&dir);

    let status = cli(&["status", path_str(&project), "--json"]);
    assert!(
        status.ok,
        "status on a v0.54.0 index failed: {}",
        status.stderr
    );
    let json: serde_json::Value = serde_json::from_str(&status.stdout).expect("status JSON");
    assert_eq!(
        json["extractionStatus"], "outdated",
        "a v0.54.0 index must read as outdated: {}",
        status.stdout
    );

    let sync = cli(&["sync", path_str(&project)]);
    assert!(
        sync.ok,
        "sync over a v0.54.0 index failed:\nstdout:\n{}\nstderr:\n{}",
        sync.stdout, sync.stderr
    );

    assert_upgraded_like_a_fresh_init(&project, "cli");
}

#[test]
fn catch_up_sync_upgrades_a_v0_54_index_to_the_current_schema_and_extraction() {
    let dir = TestDir::new("catch-up");
    let project = plant_v0_54_index(&dir);

    // The catch-up a daemon or MCP session runs when it first opens a project.
    codegraph_watch::sync_project_once(&project).expect("catch-up sync over a v0.54.0 index");

    assert_upgraded_like_a_fresh_init(&project, "catch-up");
}

/// Copy the mini corpus into `dir`, create a namespace with the current binary,
/// then replace its database and state slot with what v0.54.0 left behind.
fn plant_v0_54_index(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("project");
    copy_tree(&mini_fixture(), &project);
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init failed: {}", init.stderr);

    let paths = index_paths(&project);
    let db = paths.current_db();
    for sidecar in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{sidecar}", db.display())));
    }
    fs::copy(v0_54_database(), &db).expect("plant the v0.54.0 database");
    stage_state_slot(
        &paths,
        2,
        CURRENT_STORAGE_PROTOCOL,
        V0_54_EXTRACTION_VERSION,
        "current",
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Outdated {
            built: V0_54_EXTRACTION_VERSION
        },
        "the planted namespace must classify as outdated before the upgrade"
    );
    project
}

fn assert_upgraded_like_a_fresh_init(project: &Path, label: &str) {
    let paths = index_paths(project);
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Current,
        "the upgraded namespace must be current"
    );
    let conn = rusqlite::Connection::open_with_flags(
        format!("file:{}?immutable=1", paths.current_db().display()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open upgraded database");
    let schema: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get(0)
        })
        .expect("schema version");
    assert_eq!(schema, CURRENT_SCHEMA_VERSION, "schema must be current");
    let stamp: String = conn
        .query_row(
            "SELECT value FROM project_metadata WHERE key = 'indexed_with_extraction_version'",
            [],
            |row| row.get(0),
        )
        .expect("extraction stamp");
    assert_eq!(stamp, CURRENT_EXTRACTION_VERSION.to_string());
    drop(conn);

    let peer_dir = TestDir::new(&format!("{label}-fresh"));
    let peer = peer_dir.path().join("project");
    copy_tree(&mini_fixture(), &peer);
    let init = cli(&["init", path_str(&peer)]);
    assert!(init.ok, "fresh init failed: {}", init.stderr);

    let upgraded = canonicalize_db(&paths.current_db()).expect("canonicalize upgraded index");
    let fresh =
        canonicalize_db(&index_paths(&peer).current_db()).expect("canonicalize fresh index");
    if let Err(report) = diff_canonical(&fresh, &upgraded, None) {
        panic!("the upgraded index differs from a fresh init:\n{report}");
    }
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn v0_54_database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade_v0_54/codegraph.db")
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
/// `batch_m_outdated_migration.rs` stages protocol records.
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
            "codegraph-upgrade-v054-{label}-{}-{}",
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
