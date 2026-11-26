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

// ---- W7-29 red seeds: a directory moved in converges (real backend, real sync) ----
//
// Appended to crates/codegraph-watch/tests/event_loss.rs, which CI runs on
// Linux (make check), Windows (the workspace tests) and macOS (the
// "event-loss convergence" step), so every backend runs them.

/// A watcher on `root` whose completed syncs record their trigger paths, after
/// a canary edit proved the backend is reporting.
#[allow(clippy::field_reassign_with_default)] // WatchOptions has private test hooks.
fn w729_live_watcher(root: &Path) -> ProjectWatcher {
    let synced: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&synced);
    let mut options = WatchOptions::default();
    options.debounce = Duration::from_millis(200);
    options.on_sync_complete = Some(Arc::new(move |outcome| {
        sink.lock().unwrap().extend(outcome.trigger_paths);
    }));
    let watcher = ProjectWatcher::start(root, options)
        .expect("start the watcher")
        .expect("watching is enabled for a temporary project");
    let canary_deadline = Instant::now() + Duration::from_secs(30);
    let mut round = 0usize;
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
        round += 1;
        fs::write(
            root.join("src/canary.ts"),
            format!("export const canary = {round};\n"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(250));
    }
    watcher
}

/// Wait until the index holds no pending change, or fail naming what is left.
fn w729_converge(root: &Path, what: &str) {
    let deadline = Instant::now() + CONVERGE_WITHIN;
    let mut last = None;
    loop {
        if let Some(now) = pending(root) {
            if now.is_empty() {
                return;
            }
            last = Some(now);
        }
        assert!(
            Instant::now() < deadline,
            "{what}: the watcher left pending changes {CONVERGE_WITHIN:?} later: {:?}",
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
}

fn w729_indexed(root: &Path) -> Vec<String> {
    let paths = IndexPaths::resolve(root, std::env::var("CODEGRAPH_DIR").ok().as_deref()).unwrap();
    let store = Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
        .expect("open the converged index");
    store
        .all_files()
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .filter(|path| path != "src/canary.ts")
        .collect()
}

/// R6: a directory of three sources moved in from outside the project is
/// indexed by the watcher alone: no event names its files on any backend.
#[test]
fn a_directory_moved_in_converges() {
    let root = TestRoot::new("moved-in");
    let staging = TestRoot::new("moved-in-staging");
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("src/main.ts"),
        "import { helper } from './goods/a';\nexport function main(): number { return helper(); }\n",
    )
    .unwrap();
    sync_project_once(root.path()).expect("build the index");
    assert_eq!(pending(root.path()), Some(PendingChanges::default()));
    let goods = staging.path().join("goods");
    fs::create_dir_all(goods.join("nested")).unwrap();
    fs::write(
        goods.join("a.ts"),
        "export function helper(): number { return 1; }\n",
    )
    .unwrap();
    fs::write(goods.join("b.ts"), "export const b = 2;\n").unwrap();
    fs::write(goods.join("nested/c.ts"), "export const c = 3;\n").unwrap();

    let watcher = w729_live_watcher(root.path());
    fs::rename(&goods, root.path().join("src/goods")).unwrap();
    w729_converge(root.path(), "a directory of three files moved in");
    watcher.stop();
    assert_eq!(
        w729_indexed(root.path()),
        vec![
            "src/goods/a.ts".to_string(),
            "src/goods/b.ts".to_string(),
            "src/goods/nested/c.ts".to_string(),
            "src/main.ts".to_string(),
        ]
    );
}

/// R7: a directory of 600 sources moved in, past the 500-path ceiling,
/// converges through one full reconcile.
#[test]
fn a_directory_of_600_files_moved_in_converges() {
    let root = TestRoot::new("moved-in-bulk");
    let staging = TestRoot::new("moved-in-bulk-staging");
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/main.ts"), "export const main = 0;\n").unwrap();
    sync_project_once(root.path()).expect("build the index");
    let bulk = staging.path().join("bulk");
    fs::create_dir_all(&bulk).unwrap();
    for n in 0..FILES {
        fs::write(bulk.join(format!("m{n:03}.ts")), source(n)).unwrap();
    }

    let watcher = w729_live_watcher(root.path());
    fs::rename(&bulk, root.path().join("src/bulk")).unwrap();
    w729_converge(root.path(), "a directory of 600 files moved in");
    watcher.stop();
    let indexed = w729_indexed(root.path());
    assert_eq!(
        indexed.len(),
        FILES + 1,
        "{:?}",
        &indexed[..indexed.len().min(5)]
    );
}
