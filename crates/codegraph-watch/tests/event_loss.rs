//! A burst of renames converges through the live watcher (upstream #1397,
//! sunerpy/codegraph-rust#269).
//!
//! 600 indexed files are renamed at once, past the 500-path ceiling above which
//! the watcher diffs the whole project instead of syncing path by path. Whatever
//! the platform's backend does with such a burst (inotify can overflow its
//! queue, FSEvents can coalesce or drop a stream, ReadDirectoryChangesW can lose
//! its buffer), the watcher must leave an index with no pending change: every
//! renamed path indexed and every old path gone.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_core::IndexPaths;
use codegraph_store::Store;
use codegraph_watch::{
    PendingChanges, ProjectWatcher, WatchOptions, pending_project_changes, sync_project_once,
};

const FILES: usize = 600;
/// Upper bound for the watcher to settle the burst on a loaded runner.
const CONVERGE_WITHIN: Duration = Duration::from_secs(90);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "codegraph-watch-event-loss-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test project");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source(n: usize) -> String {
    format!("export function burstMember{n}(): number {{ return {n}; }}\n")
}

/// The index's pending changes, or `None` while a sync holds the index.
fn pending(root: &Path) -> Option<PendingChanges> {
    let paths = IndexPaths::resolve(root, std::env::var("CODEGRAPH_DIR").ok().as_deref())
        .expect("resolve index paths");
    let store = Store::open_for_read(&paths, Instant::now() + Duration::from_millis(500), || {
        false
    })
    .ok()?;
    Some(pending_project_changes(root, &store).expect("pending changes"))
}

#[test]
#[allow(clippy::field_reassign_with_default)] // WatchOptions has private test hooks.
fn renaming_600_files_at_once_converges() {
    let root = TestRoot::new("rename-burst");
    let src = root.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for n in 0..FILES {
        fs::write(src.join(format!("m{n:03}.ts")), source(n)).unwrap();
    }
    sync_project_once(root.path()).expect("build the index");
    assert_eq!(pending(root.path()), Some(PendingChanges::default()));

    let synced: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&synced);
    let mut options = WatchOptions::default();
    options.debounce = Duration::from_millis(200);
    options.on_sync_complete = Some(Arc::new(move |outcome| {
        sink.lock().unwrap().extend(outcome.trigger_paths);
    }));
    let watcher = ProjectWatcher::start(root.path(), options)
        .expect("start the watcher")
        .expect("watching is enabled for a temporary project");

    // The backend is reporting once a canary edit reaches a sync; FSEvents in
    // particular starts delivering a moment after registration.
    let canary_deadline = Instant::now() + Duration::from_secs(30);
    let mut canary_round = 0usize;
    while !synced
        .lock()
        .unwrap()
        .iter()
        .any(|path| path == "src/canary.ts")
    {
        assert!(
            Instant::now() < canary_deadline,
            "the watcher never reported the canary edit"
        );
        canary_round += 1;
        fs::write(
            src.join("canary.ts"),
            format!("export const canary = {canary_round};\n"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(250));
    }

    for n in 0..FILES {
        fs::rename(
            src.join(format!("m{n:03}.ts")),
            src.join(format!("renamed_m{n:03}.ts")),
        )
        .unwrap();
    }

    let deadline = Instant::now() + CONVERGE_WITHIN;
    let mut last = None;
    loop {
        if let Some(now) = pending(root.path()) {
            if now.is_empty() {
                break;
            }
            last = Some(now);
        }
        assert!(
            Instant::now() < deadline,
            "the watcher left pending changes {CONVERGE_WITHIN:?} after the burst: \
             {:?}",
            last.as_ref().map(|left| (
                left.added.len(),
                left.modified.len(),
                left.removed.len(),
                left.added
                    .iter()
                    .chain(&left.removed)
                    .take(5)
                    .collect::<Vec<_>>()
            ))
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    watcher.stop();

    let paths =
        IndexPaths::resolve(root.path(), std::env::var("CODEGRAPH_DIR").ok().as_deref()).unwrap();
    let store = Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
        .expect("open the converged index");
    let files = store
        .all_files()
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .filter(|path| path != "src/canary.ts")
        .collect::<Vec<_>>();
    let expected = (0..FILES)
        .map(|n| format!("src/renamed_m{n:03}.ts"))
        .collect::<Vec<_>>();
    assert_eq!(files, expected, "exactly the renamed files are indexed");
}
