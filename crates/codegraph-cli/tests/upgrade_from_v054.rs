//! Upgrading an index the previous release wrote.
//!
//! `tests/fixtures/upgrade_v0_54/codegraph.db` is the schema-8 database the
//! released v0.54.0 binary (extraction version 21) wrote for the mini corpus.
//! Each test plants it in a fresh project namespace, stages the state slot that
//! release publishes (`phase=current`, extraction version 21), and upgrades it
//! through the paths a user hits: `codegraph sync` from the shell, the catch-up
//! sync a daemon or MCP session runs when it first opens the project, and a cold
//! `serve --mcp` session in the default daemon mode, whose daemon must upgrade
//! the index before it starts serving. Each must classify the namespace as
//! outdated, rebuild it from source, and leave an index at the current schema
//! whose canonical graph equals a fresh `init` of the same tree.

mod upgrade_support;

use std::fs;
use std::path::{Path, PathBuf};

use codegraph_store::CURRENT_STORAGE_PROTOCOL;
#[cfg(unix)]
use codegraph_store::{ExtractionStatus, Store};
use upgrade_support::*;

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

/// The default daemon mode: a cold `serve --mcp` session answers direct and
/// spawns the project's daemon, which used to refuse an `outdated` index at its
/// startup gate, so nothing ever upgraded it and every tool call failed until a
/// manual `codegraph sync`. The daemon must upgrade the index first, then serve
/// it and keep it live.
#[cfg(unix)]
#[test]
fn a_cold_mcp_session_upgrades_a_v0_54_index_through_its_daemon() {
    let dir = TestDir::new("daemon");
    let project = plant_v0_54_index(&dir);
    let paths = index_paths(&project);
    let mut session = cold::Session::start(&project, dir.path());

    let deadline = std::time::Instant::now() + cold::UPGRADED_WITHIN;
    while Store::extraction_status(&paths) != ExtractionStatus::Current {
        assert!(
            std::time::Instant::now() < deadline,
            "the v0.54.0 index was still {:?} {:?} after the cold session started;\n\
             daemon.log:\n{}\nsession stderr:\n{}",
            Store::extraction_status(&paths),
            cold::UPGRADED_WITHIN,
            fs::read_to_string(paths.daemon_log()).unwrap_or_default(),
            session.stderr()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    cold::wait_for_daemon(&project, &session);
    assert_upgraded_like_a_fresh_init(&project, "daemon");

    let marker = "upgradedDaemonSessionMarker";
    fs::write(
        project.join("src/upgraded.ts"),
        format!("export function {marker}(): number {{\n  return 1;\n}}\n"),
    )
    .unwrap();
    session.wait_for_symbol(marker, "src/upgraded.ts");
    session.close();
    cold::shutdown_daemon(&project);
}

/// A daemon never undoes an older `uninit --force`. That leaves an
/// `uninitialized` slot at the older extraction version, a tombstone, and no
/// database, which the state gate classifies as `uninitialized`, like this
/// version's own uninit; the daemon must not try to upgrade it, must leave all
/// of it alone, and must refuse to start.
#[cfg(unix)]
#[test]
fn a_cold_mcp_session_leaves_an_older_uninit_alone() {
    let dir = TestDir::new("daemon-uninit");
    let project = dir.path().join("project");
    copy_tree(&mini_fixture(), &project);
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init failed: {}", init.stderr);
    let uninit = cli(&["uninit", "--force", path_str(&project)]);
    assert!(uninit.ok, "uninit failed: {}", uninit.stderr);
    let paths = index_paths(&project);
    stage_state_slot(
        &paths,
        3,
        CURRENT_STORAGE_PROTOCOL,
        V0_54_EXTRACTION_VERSION,
        "uninitialized",
    );
    let uninitialized = ExtractionStatus::Uninitialized;
    assert_eq!(Store::extraction_status(&paths), uninitialized);
    assert!(paths.tombstone().is_file(), "uninit leaves its tombstone");
    assert!(!paths.current_db().exists(), "uninit removes the database");
    let slots = state_slot_bytes(&paths);

    let session = cold::Session::start(&project, dir.path());
    let log = cold::wait_for_daemon_log(
        &paths,
        "refusing to start a daemon for",
        "state-gated Store open rejected",
        &session,
    );
    assert!(
        !paths.current_db().exists(),
        "the daemon recreated the database an uninit removed:\n{log}"
    );
    assert!(paths.tombstone().is_file(), "the tombstone must stay");
    assert_eq!(Store::extraction_status(&paths), uninitialized);
    assert_eq!(
        state_slot_bytes(&paths),
        slots,
        "the state slots must not move"
    );
    assert!(
        !cold::daemon_live(&project),
        "no daemon may serve this index"
    );
    assert!(
        log.contains("index state is uninitialized"),
        "the daemon must refuse the uninit as uninitialized:\n{log}"
    );
    assert!(
        !log.contains("upgrading an index an older codegraph built"),
        "the daemon must not try to upgrade an uninit:\n{log}"
    );
    session.close();
}

/// A failed upgrade is a failed migration: the daemon stops before its startup
/// gate, and nothing changes. Here the project config cannot be parsed, so the
/// upgrade fails before it takes the index lease.
#[cfg(unix)]
#[test]
fn a_daemon_whose_upgrade_fails_still_refuses_the_outdated_index() {
    let dir = TestDir::new("daemon-failed-upgrade");
    let project = plant_v0_54_index(&dir);
    let paths = index_paths(&project);
    fs::write(paths.config_toml(), "[indexing\n").unwrap();
    let database = fs::read(paths.current_db()).unwrap();
    let slots = state_slot_bytes(&paths);

    let session = cold::Session::start(&project, dir.path());
    let log = cold::wait_for_daemon_log(
        &paths,
        "refusing to start a daemon for",
        "could not upgrade its outdated index",
        &session,
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Outdated {
            built: V0_54_EXTRACTION_VERSION
        }
    );
    assert_eq!(
        fs::read(paths.current_db()).unwrap(),
        database,
        "a failed upgrade must leave the database as it was"
    );
    assert_eq!(
        state_slot_bytes(&paths),
        slots,
        "the state slots must not move"
    );
    assert!(
        !cold::daemon_live(&project),
        "no daemon may serve this index"
    );
    assert!(
        !log.contains("state-gated Store open"),
        "a failed upgrade must stop the daemon before its startup gate:\n{log}"
    );
    session.close();
}

/// The upgrade a daemon runs rebuilds only a complete older index. Each
/// namespace below is one `codegraph sync` would still migrate.
#[test]
fn the_daemon_upgrade_skips_an_interrupted_older_build() {
    let dir = TestDir::new("skip-building");
    let project = plant_v0_54_index(&dir);
    stage_state_slot(
        &index_paths(&project),
        3,
        CURRENT_STORAGE_PROTOCOL,
        V0_54_EXTRACTION_VERSION,
        "building",
    );
    assert_upgrade_skipped_untouched(&project, "records phase building");
}

#[test]
fn the_daemon_upgrade_skips_an_older_index_with_a_tombstone() {
    let dir = TestDir::new("skip-tombstone");
    let project = plant_v0_54_index(&dir);
    fs::write(index_paths(&project).tombstone(), b"uninitialized\n").unwrap();
    assert_upgrade_skipped_untouched(&project, "tombstone is present");
}

#[test]
fn the_daemon_upgrade_skips_an_older_index_without_its_database() {
    let dir = TestDir::new("skip-missing-db");
    let project = plant_v0_54_index(&dir);
    fs::remove_file(index_paths(&project).current_db()).unwrap();
    assert_upgrade_skipped_untouched(&project, "database is missing");
}

/// Copy the mini corpus into `dir`, create a namespace with the current binary,
/// then replace its database and state slot with what v0.54.0 left behind.
fn plant_v0_54_index(dir: &TestDir) -> PathBuf {
    plant_index(dir, &v0_54_database(), V0_54_EXTRACTION_VERSION)
}

fn v0_54_database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade_v0_54/codegraph.db")
}
