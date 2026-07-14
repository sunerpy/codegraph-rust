use std::path::{Path, PathBuf};

use codegraph_store::Store;

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
        store.wal_size_bytes() < 4096,
        "TRUNCATE must shrink the -wal sidecar after a fold"
    );
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let name = format!(
            "codegraph-store-bulk-pragmas-{}-{}",
            std::process::id(),
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
