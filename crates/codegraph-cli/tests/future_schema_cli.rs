//! An index a newer codegraph wrote (OD-19), through the commands a user runs:
//! `status` reports it, reads, `sync` and `init` refuse it without changing a
//! byte, MCP answers with the refusal, a daemon does not start on it, and
//! `uninit --force` followed by `init` discards and rebuilds it.

mod upgrade_support;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use codegraph_core::IndexPaths;
use codegraph_store::migrations::CURRENT_SCHEMA_VERSION;
use codegraph_store::{ExtractionStatus, Store};
use serde_json::{Value, json};
use upgrade_support::*;

const NEWER: i64 = CURRENT_SCHEMA_VERSION + 1;

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", db.display()))
}

/// The mini corpus indexed by this binary.
fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("project");
    copy_tree(&mini_fixture(), &project);
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init failed: {}", init.stderr);
    project
}

/// Record schema `NEWER` in the project's database, checkpointed.
fn record_newer_schema(paths: &IndexPaths) {
    let db = paths.current_db();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO schema_versions (version, applied_at, description) VALUES (?1, 0, 'newer')",
        [NEWER],
    )
    .unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(sidecar(&db, suffix));
    }
}

/// Record schema `NEWER` in the database's log only, as a newer binary killed
/// before its checkpoint leaves it.
fn record_newer_schema_in_the_log_only(paths: &IndexPaths) {
    let db = paths.current_db();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    conn.execute(
        "INSERT INTO schema_versions (version, applied_at, description) VALUES (?1, 0, 'newer')",
        [NEWER],
    )
    .unwrap();
    let main = fs::read(&db).unwrap();
    let wal = fs::read(sidecar(&db, "-wal")).unwrap();
    drop(conn);
    fs::write(&db, main).unwrap();
    fs::write(sidecar(&db, "-wal"), wal).unwrap();
    let _ = fs::remove_file(sidecar(&db, "-shm"));
}

/// The database, its sidecars and the state slots, byte for byte (`None` when
/// absent). The daemon's own log, pid and socket files are not part of it.
fn index_bytes(paths: &IndexPaths) -> BTreeMap<String, Option<Vec<u8>>> {
    let db = paths.current_db();
    let mut files = BTreeMap::new();
    for (label, path) in [
        ("db", db.clone()),
        ("wal", sidecar(&db, "-wal")),
        ("shm", sidecar(&db, "-shm")),
        ("slot0", paths.state_slots()[0].clone()),
        ("slot1", paths.state_slots()[1].clone()),
        ("tombstone", paths.tombstone()),
    ] {
        files.insert(label.to_string(), fs::read(path).ok());
    }
    files
}

fn assert_unchanged(paths: &IndexPaths, before: &BTreeMap<String, Option<Vec<u8>>>, what: &str) {
    let after = index_bytes(paths);
    for (label, bytes) in before {
        assert!(
            after.get(label) == Some(bytes),
            "{what} changed the index's {label}"
        );
    }
}

#[test]
fn status_reports_an_index_a_newer_codegraph_wrote() {
    let dir = TestDir::new("future-schema-status");
    let project = indexed_project(&dir);
    record_newer_schema(&index_paths(&project));

    let status = cli(&["status", path_str(&project), "--json"]);
    assert!(status.ok, "status failed: {}", status.stderr);
    let json: Value = serde_json::from_str(&status.stdout).expect("status JSON");
    assert_eq!(json["initialized"], false, "{}", status.stdout);
    assert_eq!(json["extractionStatus"], "current", "{}", status.stdout);
    let detail = json["extractionStatusDetail"].as_str().unwrap_or("");
    assert!(
        detail.contains(&format!("schema {NEWER}")) && detail.contains("uninit --force"),
        "{detail}"
    );
    assert_eq!(json["recoveryCommand"], Value::Null);

    let human = cli(&["status", path_str(&project)]);
    assert!(human.ok, "status failed: {}", human.stderr);
    assert!(
        human.stdout.contains(&format!(
            "State:   current (schema {NEWER} is newer than this binary's {CURRENT_SCHEMA_VERSION})"
        )),
        "{}",
        human.stdout
    );
    assert!(
        human.stdout.contains("codegraph uninit --force"),
        "{}",
        human.stdout
    );
}

#[test]
fn reads_sync_and_init_refuse_a_newer_schema_and_uninit_discards_it() {
    let dir = TestDir::new("future-schema-commands");
    let project = indexed_project(&dir);
    let paths = index_paths(&project);
    record_newer_schema(&paths);
    let before = index_bytes(&paths);

    let search = cli(&["search", "add", "-p", path_str(&project)]);
    assert!(!search.ok, "search read a newer codegraph's index");
    assert!(
        search.stderr.contains("newer CodeGraph") && search.stderr.contains("uninit --force"),
        "{}",
        search.stderr
    );
    assert_unchanged(&paths, &before, "search");

    let sync = cli(&["sync", path_str(&project)]);
    assert!(!sync.ok, "sync wrote a newer codegraph's index");
    assert!(sync.stderr.contains("newer CodeGraph"), "{}", sync.stderr);
    assert_unchanged(&paths, &before, "sync");

    let init = cli(&["init", path_str(&project)]);
    assert!(!init.ok, "init replaced a newer codegraph's index");
    assert!(init.stderr.contains("newer CodeGraph"), "{}", init.stderr);
    assert_unchanged(&paths, &before, "init");

    let uninit = cli(&["uninit", "--force", path_str(&project)]);
    assert!(uninit.ok, "uninit failed: {}", uninit.stderr);
    assert!(!paths.current_db().exists());
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init after uninit failed: {}", init.stderr);
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
}

/// The same index beside the tombstone a newer codegraph's `init` could not
/// remove: `init` refuses it unchanged and names `uninit --force`, which
/// discards it, and `init` then rebuilds it.
#[test]
fn a_tombstoned_index_a_newer_codegraph_wrote_is_discarded_by_uninit() {
    let dir = TestDir::new("future-schema-tombstone");
    let project = indexed_project(&dir);
    let paths = index_paths(&project);
    record_newer_schema(&paths);
    fs::write(paths.tombstone(), b"remove failed").unwrap();
    let before = index_bytes(&paths);

    let init = cli(&["init", path_str(&project)]);
    assert!(!init.ok, "init replaced a newer codegraph's index");
    assert!(
        init.stderr.contains("newer CodeGraph") && init.stderr.contains("uninit --force"),
        "{}",
        init.stderr
    );
    assert_unchanged(&paths, &before, "init");

    let uninit = cli(&["uninit", "--force", path_str(&project)]);
    assert!(uninit.ok, "uninit failed: {}", uninit.stderr);
    assert!(!paths.current_db().exists());
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init after uninit failed: {}", init.stderr);
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    assert!(!paths.tombstone().exists(), "init removed the tombstone");
}

#[test]
fn sync_leaves_a_newer_schema_in_the_log_untouched() {
    let dir = TestDir::new("future-schema-log-sync");
    let project = indexed_project(&dir);
    let paths = index_paths(&project);
    record_newer_schema_in_the_log_only(&paths);
    let before = index_bytes(&paths);
    assert!(before["wal"].is_some() && before["shm"].is_none());

    let sync = cli(&["sync", path_str(&project)]);
    assert!(!sync.ok, "sync wrote a newer codegraph's index");
    assert!(sync.stderr.contains("newer CodeGraph"), "{}", sync.stderr);
    assert_unchanged(&paths, &before, "sync");
}

/// A daemon's catch-up runs `codegraph_watch::sync_project_once`: it returns
/// the refusal and changes no byte, whether the newer version is checkpointed
/// or only in the log.
#[test]
fn the_catch_up_sync_refuses_a_newer_schema() {
    type Record = fn(&IndexPaths);
    let record: [(&str, Record); 2] = [
        ("checkpointed", record_newer_schema),
        ("log", record_newer_schema_in_the_log_only),
    ];
    for (label, record) in record {
        let dir = TestDir::new(&format!("future-schema-catch-up-{label}"));
        let project = indexed_project(&dir);
        let paths = index_paths(&project);
        record(&paths);
        let before = index_bytes(&paths);
        let error = codegraph_watch::sync_project_once(&project)
            .err()
            .unwrap_or_else(|| panic!("{label}: the catch-up wrote a newer codegraph's index"));
        assert!(
            format!("{error:#}").contains("newer CodeGraph"),
            "{label}: {error:#}"
        );
        assert_unchanged(&paths, &before, "the catch-up");
    }
}

/// The MCP server answers a tool call with the refusal (direct mode).
#[test]
fn an_mcp_tool_call_answers_with_the_refusal() {
    let dir = TestDir::new("future-schema-mcp");
    let project = indexed_project(&dir);
    let paths = index_paths(&project);
    record_newer_schema(&paths);
    let before = index_bytes(&paths);

    let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(["serve", "--mcp", "--path"])
        .arg(&project)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env(
            "CODEGRAPH_MCP_REGISTRY_DIR",
            dir.path().join("mcp-registry"),
        )
        .env_remove("CODEGRAPH_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |value: Value| {
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    };
    let mut read_id = |want: i64| -> Value {
        loop {
            let mut line = String::new();
            assert!(
                stdout.read_line(&mut line).unwrap() > 0,
                "serve closed stdout"
            );
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if value.get("id").and_then(Value::as_i64) == Some(want) {
                return value;
            }
        }
    };
    send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": { "name": "future-schema", "version": "0" } }
    }));
    read_id(1);
    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "codegraph_search", "arguments": { "query": "add" } }
    }));
    let response = read_id(2).to_string();
    assert!(response.contains("newer CodeGraph"), "{response}");
    let _ = child.kill();
    let _ = child.wait();
    assert_unchanged(&paths, &before, "the MCP session");
}

/// A cold MCP session in the default daemon mode on `project`: wait until the
/// project's daemon refuses to start, then return a tool call's answer.
#[cfg(unix)]
fn cold_session_answer(dir: &TestDir, project: &Path, paths: &IndexPaths) -> String {
    let mut session = cold::Session::start(project, dir.path());
    let log = cold::wait_for_daemon_log(paths, "refusing to start a daemon for", "", &session);
    assert!(
        !cold::daemon_live(project),
        "no daemon may serve it:\n{log}"
    );
    let answer = session.search("add");
    session.close();
    answer
}

/// In the default daemon mode the project's daemon does not start on a newer
/// codegraph's index, and a tool call answers with the refusal.
#[cfg(unix)]
#[test]
fn a_cold_mcp_session_answers_with_the_refusal() {
    let dir = TestDir::new("future-schema-cold");
    let project = indexed_project(&dir);
    let paths = index_paths(&project);
    record_newer_schema(&paths);
    let before = index_bytes(&paths);

    let answer = cold_session_answer(&dir, &project, &paths);
    assert!(answer.contains("newer CodeGraph"), "{answer}");
    assert_unchanged(&paths, &before, "the cold session");
}

/// The same when the newer schema is only in the log: the daemon folds nothing
/// into the index, and a read refuses the live log as it refuses any.
#[cfg(unix)]
#[test]
fn a_daemon_does_not_start_on_a_newer_schema_in_the_log() {
    let dir = TestDir::new("future-schema-log-daemon");
    let project = indexed_project(&dir);
    let paths = index_paths(&project);
    record_newer_schema_in_the_log_only(&paths);
    let before = index_bytes(&paths);

    let answer = cold_session_answer(&dir, &project, &paths);
    assert!(answer.contains("Failed to open project"), "{answer}");
    assert_unchanged(&paths, &before, "the daemon start");
}
