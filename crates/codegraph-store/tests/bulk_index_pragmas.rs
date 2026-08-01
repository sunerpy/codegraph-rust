use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_store::Store;

/// Monotonic per-process serial that makes every [`TestDir`] name unique.
///
/// A wall-clock timestamp alone is NOT sufficient. `SystemTime::now()` has
/// nanosecond resolution on Linux but is only updated at the system timer tick on
/// Windows (~15.6 ms), so two of this target's tests — run concurrently in
/// threads of ONE process, hence one pid — can observe the SAME `as_nanos()`
/// value and derive the same directory name, making the second `create_dir` fail
/// with `ERROR_ALREADY_EXISTS`. The serial cannot collide by construction.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

const CHILD_CASE: &str = "CODEGRAPH_TEST_BULK_PRAGMA_CASE";
const WAL_VALVE_ENV: &str = "CODEGRAPH_WAL_VALVE_MB";

#[test]
fn set_bulk_index_pragmas_drops_synchronous_to_off() {
    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");

    let store = Store::open(&db_path).unwrap();
    assert_eq!(read_synchronous(&store), 1);

    store.set_bulk_index_pragmas().unwrap();
    assert_eq!(read_synchronous(&store), 0);
}

#[test]
fn restore_default_pragmas_returns_to_normal_on_same_connection() {
    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");

    let store = Store::open(&db_path).unwrap();
    store.set_bulk_index_pragmas().unwrap();
    assert_eq!(read_synchronous(&store), 0);

    store.restore_default_pragmas().unwrap();
    assert_eq!(read_synchronous(&store), 1);
}

// Error-path proof: a guard that restores on Drop must leave the DB at NORMAL even
// when the indexing body bails out early. The guard is dropped before `store`, so
// its restore runs on its own connection without WAL contention, mirroring the CLI
// BulkIndexPragmaGuard ordering.
#[test]
fn guard_restores_normal_on_early_return_path() {
    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");

    fn simulated_index(db_path: &Path) -> Result<(), ()> {
        let guard = RestoreGuard {
            db_path: db_path.to_path_buf(),
        };
        let store = Store::open(db_path).unwrap();
        store.set_bulk_index_pragmas().unwrap();
        assert_eq!(read_synchronous(&store), 0);
        drop(store);
        // Force the error path before any explicit restore line could run.
        drop(guard);
        Err(())
    }

    assert!(simulated_index(&db_path).is_err());

    let reopened = Store::open(&db_path).unwrap();
    let sync = read_synchronous(&reopened);
    assert!(
        sync == 1 || sync == 2,
        "expected NORMAL durability, got {sync}"
    );
}

struct RestoreGuard {
    db_path: PathBuf,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        let store = Store::open(&self.db_path).unwrap();
        store.restore_default_pragmas().unwrap();
    }
}

fn read_synchronous(store: &Store) -> i64 {
    store
        .connection()
        .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
        .unwrap()
}

fn read_wal_autocheckpoint(store: &Store) -> i64 {
    store
        .connection()
        .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
        .unwrap()
}

fn read_journal_size_limit(store: &Store) -> i64 {
    store
        .connection()
        .query_row("PRAGMA journal_size_limit", [], |row| row.get::<_, i64>(0))
        .unwrap()
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut native = db.as_os_str().to_os_string();
    native.push(suffix);
    PathBuf::from(native)
}

#[test]
fn default_open_sets_journal_size_limit_to_shared_wal_threshold() {
    run_isolated_child("default-journal-size-limit");
}

fn assert_default_journal_size_limit() {
    let dir = TestDir::new();
    let store = Store::open(&dir.path().join("codegraph.db")).unwrap();

    assert_eq!(read_journal_size_limit(&store), 256 * 1024 * 1024);
}

#[test]
fn wal_valve_override_also_sets_journal_size_limit() {
    run_isolated_child("override-journal-size-limit");
}

fn assert_overridden_journal_size_limit() {
    let dir = TestDir::new();
    let store = Store::open(&dir.path().join("codegraph.db")).unwrap();

    assert_eq!(read_journal_size_limit(&store), 1024 * 1024);
}

#[test]
fn journal_size_limit_clips_wal_on_write_after_restart_checkpoint() {
    run_isolated_child("journal-size-limit-reset-clip");
}

fn assert_journal_size_limit_reset_clip() {
    const LIMIT_BYTES: u64 = 1024 * 1024;

    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");
    let store = Store::open(&db_path).unwrap();

    store
        .connection()
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    store
        .connection()
        .execute_batch("CREATE TABLE wal_cap_probe(id INTEGER PRIMARY KEY, payload TEXT NOT NULL);")
        .unwrap();
    let payload = "x".repeat(8192);
    let tx = store.connection().unchecked_transaction().unwrap();
    for id in 1..=400 {
        tx.execute(
            "INSERT INTO wal_cap_probe(id, payload) VALUES (?, ?)",
            rusqlite::params![id, &payload],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let wal = sidecar(&db_path, "-wal");
    let before_restart = std::fs::metadata(&wal).unwrap().len();
    assert!(
        before_restart > LIMIT_BYTES,
        "fixture must create a WAL over the configured cap, got {before_restart}"
    );

    store
        .connection()
        .pragma_update(None, "wal_checkpoint", "RESTART")
        .unwrap();
    let after_restart = std::fs::metadata(&wal).unwrap().len();
    assert_eq!(
        after_restart, before_restart,
        "RESTART establishes the reset point but must not clip the WAL itself"
    );

    store
        .connection()
        .execute(
            "INSERT INTO wal_cap_probe(id, payload) VALUES (401, 'reset')",
            [],
        )
        .unwrap();
    let after_resetting_write = std::fs::metadata(&wal).unwrap().len();
    assert!(
        after_resetting_write <= LIMIT_BYTES,
        "the first resetting write must clip the WAL to {LIMIT_BYTES} bytes, got {after_resetting_write}"
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM wal_cap_probe", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        401,
        "all rows must remain readable after the WAL reset"
    );
}

// #1231: with WAL deferral ON (default), bulk index sets wal_autocheckpoint=0 so
// SQLite stops re-writing hot pages into the main DB on a 1000-page cadence.
#[test]
fn set_bulk_index_pragmas_disables_wal_autocheckpoint_by_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: guarded by ENV_LOCK so the opt-out test's env set/remove can't race.
    unsafe { std::env::remove_var("CODEGRAPH_NO_WAL_DEFER") };
    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");

    let store = Store::open(&db_path).unwrap();
    assert_eq!(read_wal_autocheckpoint(&store), 1000, "default before bulk");

    store.set_bulk_index_pragmas().unwrap();
    assert_eq!(
        read_wal_autocheckpoint(&store),
        0,
        "bulk index must defer WAL autocheckpoint"
    );
}

// #1231 opt-out: CODEGRAPH_NO_WAL_DEFER=1 keeps SQLite's default autocheckpoint.
// The env-mutation window is serialized on a process-global lock so parallel
// tests cannot observe a half-set env.
#[test]
fn no_wal_defer_env_keeps_default_autocheckpoint() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");
    // SAFETY: guarded by ENV_LOCK.
    unsafe { std::env::set_var("CODEGRAPH_NO_WAL_DEFER", "1") };
    let store = Store::open(&db_path).unwrap();
    store.set_bulk_index_pragmas().unwrap();
    let checkpoint = read_wal_autocheckpoint(&store);
    // SAFETY: guarded by ENV_LOCK.
    unsafe { std::env::remove_var("CODEGRAPH_NO_WAL_DEFER") };
    assert_eq!(
        checkpoint, 1000,
        "opt-out must leave the default autocheckpoint interval"
    );
}

// #1231 valve: checkpoint_wal_if_over is a no-op under the threshold and folds
// the WAL (returns true) once it grows past it. With autocheckpoint deferred, a
// batch of writes grows the -wal file, so a tiny threshold trips the fold.
#[test]
fn checkpoint_wal_if_over_folds_only_past_threshold() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: guarded by ENV_LOCK so the opt-out test's env set/remove can't race.
    unsafe { std::env::remove_var("CODEGRAPH_NO_WAL_DEFER") };
    let dir = TestDir::new();
    let db_path = dir.path().join("codegraph.db");
    let store = Store::open(&db_path).unwrap();
    store.set_bulk_index_pragmas().unwrap();

    store
        .connection()
        .execute_batch("CREATE TABLE t(x TEXT);")
        .unwrap();
    let big = "x".repeat(4096);
    for _ in 0..64 {
        store
            .connection()
            .execute("INSERT INTO t(x) VALUES (?)", [&big])
            .unwrap();
    }

    // A threshold far above the WAL size is a no-op.
    assert!(
        !store.checkpoint_wal_if_over(u64::MAX).unwrap(),
        "must not fold when under threshold"
    );
    // A zero threshold folds the (non-empty) WAL back and truncates it.
    let folded = store.checkpoint_wal_if_over(0).unwrap();
    assert!(folded, "must fold when WAL exceeds threshold");
    assert!(
        store.wal_size_bytes().unwrap() < 4096,
        "TRUNCATE must shrink the -wal sidecar after a fold"
    );
}

fn run_isolated_child(case: &str) {
    let mut command = Command::new(std::env::current_exe().expect("current test binary"));
    command
        .arg("--exact")
        .arg("bulk_index_pragmas_child_process")
        .arg("--nocapture")
        .env(CHILD_CASE, case)
        .env_remove(WAL_VALVE_ENV);
    match case {
        "override-journal-size-limit" | "journal-size-limit-reset-clip" => {
            command.env(WAL_VALVE_ENV, "1");
        }
        _ => {}
    }
    let output = command.output().expect("run isolated pragma child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated pragma child `{case}` failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains(&format!("CASE_PASSED {case}")),
        "isolated pragma child `{case}` did not execute its assertions: stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn bulk_index_pragmas_child_process() {
    let Ok(case) = std::env::var(CHILD_CASE) else {
        return;
    };
    match case.as_str() {
        "default-journal-size-limit" => assert_default_journal_size_limit(),
        "override-journal-size-limit" => assert_overridden_journal_size_limit(),
        "journal-size-limit-reset-clip" => assert_journal_size_limit_reset_clip(),
        other => panic!("unknown isolated pragma child case: {other}"),
    }
    println!("CASE_PASSED {case}");
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let name = format!(
            "codegraph-store-bulk-pragmas-{}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
