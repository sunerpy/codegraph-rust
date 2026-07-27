//! A daemon killed with an open SQLite connection leaves an un-checkpointed
//! `-wal` behind. The strict `Current` startup gate demands sidecar-freedom, so
//! before this recovery existed that residue refused EVERY later daemon start
//! until `codegraph init` was re-run.
//!
//! These tests plant a REAL un-checkpointed write-ahead log — a child process
//! writes a row with `wal_autocheckpoint=0` and then dies without closing SQLite
//! — and drive the PRODUCTION daemon-start path over it. Zero-byte sidecar files
//! would prove nothing, so the planted log is asserted non-empty and the planted
//! row is asserted absent from the main database file before startup.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use codegraph_daemon::{
    daemon_pid_path, daemon_socket_path, is_process_alive, spawn_detached_daemon, unlock_project,
};
use codegraph_store::Store;
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};

/// Env var carrying the database a child process must plant a live write-ahead
/// log into before dying.
const PLANT_WAL_DB: &str = "CODEGRAPH_TEST_PLANT_WAL_DB";

/// The metadata key the planted row uses. It exists ONLY in the write-ahead log
/// until something folds that log back into the main database file.
const WAL_ONLY_KEY: &str = "stale_wal_recovery_probe";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create fixture copy dir");
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("fixture dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy fixture file");
        }
    }
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-stale-wal-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create test dir");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn indexed_project(label: &str) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.0.join("mini");
    copy_tree(
        &workspace_root().join("crates/codegraph-bench/fixtures/mini"),
        &project,
    );
    let status = Command::new(bin())
        .args(["init", project.to_str().expect("utf8 project path")])
        .status()
        .expect("run codegraph init");
    assert!(status.success(), "init failed for {}", project.display());
    (dir, project)
}

fn index_paths(project: &Path) -> codegraph_core::IndexPaths {
    codegraph_core::IndexPaths::resolve(project, None).expect("resolve v2 index paths")
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut native = db.as_os_str().to_os_string();
    native.push(suffix);
    PathBuf::from(native)
}

/// Plant a genuinely un-checkpointed write-ahead log by re-invoking THIS test
/// binary as a child that writes one row with WAL auto-checkpointing disabled and
/// then dies without closing SQLite. A real dead process is the point: an
/// in-process leak would keep the connection open and look like a LIVE owner.
fn plant_unckeckpointed_wal(db: &Path) {
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("plant_uncheckpointed_wal_child_process")
        .arg("--nocapture")
        .env(PLANT_WAL_DB, db)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run WAL-planting child");
    assert!(
        !output.status.success(),
        "the planting child must die without closing SQLite, so it never exits successfully"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PLANTED"),
        "the planting child must report a committed row before dying: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The child half of [`plant_unckeckpointed_wal`]. A no-op in an ordinary run.
#[test]
fn plant_uncheckpointed_wal_child_process() {
    let Ok(db) = std::env::var(PLANT_WAL_DB) else {
        return;
    };
    let store = Store::open(Path::new(&db)).expect("open the planted database");
    store
        .connection()
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("disable WAL auto-checkpointing");
    store
        .set_project_metadata(WAL_ONLY_KEY, "1")
        .expect("commit the WAL-only row");
    println!("PLANTED");
    // Die exactly like a SIGKILLed daemon: no SQLite close, no checkpoint, so the
    // committed row stays in the `-wal` sidecar only.
    std::process::abort();
}

/// Read one metadata key from a COPY of the main database file alone, with no
/// sidecar beside it. A value visible here is durably folded into the main file;
/// a value only in the write-ahead log is not.
fn metadata_in_main_file_only(db: &Path, label: &str) -> Option<String> {
    let dir = TestDir::new(label);
    let copy = dir.0.join("main-only.db");
    fs::copy(db, &copy).expect("copy the main database file");
    let store = Store::open(&copy).expect("open the main-file-only copy");
    let value = store
        .get_project_metadata(WAL_ONLY_KEY)
        .expect("read the probe key");
    drop(store);
    value
}

fn read_pid_from_hello(socket: &Path) -> Option<u32> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    let stream = Stream::connect(name).ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()?
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map(|pid| pid as u32)
}

fn poll_for_daemon_pid(socket: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if socket.exists()
            && let Some(pid) = read_pid_from_hello(socket)
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn process_is_gone_or_zombie(pid: u32) -> bool {
    if !is_process_alive(pid) {
        return true;
    }
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .map(|state| state == "Z")
            .unwrap_or(false),
        Err(_) => true,
    }
}

fn kill_and_reap(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if process_is_gone_or_zombie(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon pid {pid} must be dead after teardown");
}

/// A dead owner's leftover, genuinely non-empty write-ahead log no longer bricks
/// the namespace: the daemon starts, and the row that existed ONLY in that log is
/// folded into the main database file instead of being discarded.
#[test]
fn a_dead_owners_uncheckpointed_wal_is_recovered_on_daemon_startup() {
    let (_dir, project) = indexed_project("recover");
    let paths = index_paths(&project);
    let db = paths.current_db();
    let wal = sidecar(&db, "-wal");

    plant_unckeckpointed_wal(&db);

    // The residue is REAL: a non-empty log holding a row the main file lacks.
    let planted = fs::metadata(&wal).expect("planted -wal exists").len();
    assert!(
        planted > 0,
        "the planted -wal must carry bytes; a zero-byte sidecar proves nothing"
    );
    assert_eq!(
        metadata_in_main_file_only(&db, "before"),
        None,
        "the probe row must live ONLY in the write-ahead log before recovery"
    );

    let socket = daemon_socket_path(&project).expect("resolve the rendezvous socket identity");
    spawn_detached_daemon(&bin(), &project, true).expect("spawn a daemon over the stale residue");
    let pid = poll_for_daemon_pid(&socket, Duration::from_secs(5)).unwrap_or_else(|| {
        panic!(
            "a daemon must start over a dead owner's leftover WAL; daemon log:\n{}",
            fs::read_to_string(paths.daemon_log()).unwrap_or_default()
        )
    });

    assert_eq!(
        metadata_in_main_file_only(&db, "after").as_deref(),
        Some("1"),
        "recovery must FOLD the log into the main database file, not discard it"
    );
    assert!(
        paths.permanent_lock().exists(),
        "the permanent index lock must survive recovery"
    );
    assert_eq!(
        Store::extraction_status(&paths),
        codegraph_store::ExtractionStatus::Current,
        "the namespace must still classify as Current"
    );

    kill_and_reap(pid);
    unlock_project(&project);
}

/// The paired fail-closed control: the SAME stale residue in a namespace carrying
/// the uninitialized tombstone is still refused. Recovery never becomes a way
/// around the state protocol.
#[test]
fn the_same_stale_residue_under_a_tombstone_is_still_refused() {
    let (_dir, project) = indexed_project("tombstoned");
    let paths = index_paths(&project);
    let db = paths.current_db();
    let wal = sidecar(&db, "-wal");

    plant_unckeckpointed_wal(&db);
    assert!(
        fs::metadata(&wal).expect("planted -wal exists").len() > 0,
        "the planted -wal must carry bytes"
    );
    fs::write(paths.tombstone(), b"").expect("plant the uninitialized tombstone");

    let output = Command::new(bin())
        .args(["serve", "--mcp", "--path"])
        .arg(&project)
        .env("CODEGRAPH_DAEMON_INTERNAL", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .expect("run the gated daemon start");

    assert!(
        !output.status.success(),
        "a tombstoned namespace must refuse to start a daemon"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to start a daemon"),
        "the refusal must be reported: {stderr}"
    );
    assert!(
        fs::metadata(&wal)
            .expect("the -wal survives a refusal")
            .len()
            > 0,
        "a refused start must fold nothing"
    );
    assert!(
        !daemon_pid_path(&project)
            .expect("resolve the rendezvous pid path")
            .exists(),
        "a refused start must publish no pid record"
    );
    assert!(
        !daemon_socket_path(&project)
            .expect("resolve the rendezvous socket identity")
            .exists(),
        "a refused start must publish no socket"
    );
}
