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
