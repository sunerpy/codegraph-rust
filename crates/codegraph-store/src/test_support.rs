//! Test-only helpers for materializing state-gated index fixtures.

use std::fs::OpenOptions;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use codegraph_core::IndexPaths;

use crate::{
    CURRENT_EXTRACTION_VERSION, EXTRACTION_VERSION_KEY, IndexLease, StatePhase, Store,
    publish_index_state,
};

/// Complete a freshly created or copied SQLite fixture as a readable `Current`
/// namespace.
///
/// Production code must never repair a database artifact that lacks its state
/// protocol. Tests, however, frequently construct the SQLite bytes directly.
/// This feature-gated helper gives those fixtures the same permanent lock,
/// extraction stamp, and monotonic `Building -> Current` state publication that
/// a successful rebuild would leave behind, without weakening any production
/// read gate.
pub fn finalize_current_test_fixture(paths: &IndexPaths) -> Result<()> {
    let db = paths.current_db();
    ensure!(
        db.is_file(),
        "test fixture database does not exist at {}",
        db.display()
    );

    let lock_path = paths.permanent_lock();
    if !lock_path.exists() {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .with_context(|| format!("create test fixture lock {}", lock_path.display()))?;
        lock.sync_all()
            .with_context(|| format!("sync test fixture lock {}", lock_path.display()))?;
    }

    let lease = IndexLease::acquire_exclusive_existing(
        paths,
        Instant::now() + Duration::from_secs(5),
        || false,
    )
    .context("acquire test fixture lease")?;

    let store = Store::open(&db).context("open test fixture database")?;
    store
        .set_project_metadata(
            EXTRACTION_VERSION_KEY,
            &CURRENT_EXTRACTION_VERSION.to_string(),
        )
        .context("stamp test fixture extraction version")?;
    store
        .restore_default_pragmas()
        .context("checkpoint test fixture database")?;
    drop(store);

    publish_index_state(paths, &lease, StatePhase::Building)
        .context("publish test fixture Building state")?;
    publish_index_state(paths, &lease, StatePhase::Current)
        .context("publish test fixture Current state")?;
    Ok(())
}
