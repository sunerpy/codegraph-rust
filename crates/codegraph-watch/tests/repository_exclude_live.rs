//! A live watcher sees an edit to `.git/info/exclude` (upstream #1728).
//!
//! The exclude file decides scan membership like the root `.gitignore`, but it
//! sits inside `.git/`, which the watcher never watches as source. Per-directory
//! backends (inotify) therefore watch `.git/info` as a control directory, and
//! recursive backends pass its events through: either way, excluding an indexed
//! directory drops its files from the index, and clearing the rule restores
//! them, without a manual sync.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_core::IndexPaths;
use codegraph_store::Store;
use codegraph_watch::{ProjectWatcher, WatchOptions, sync_project_once};

/// Upper bound for the watcher to apply one exclude edit on a loaded runner.
const APPLY_WITHIN: Duration = Duration::from_secs(60);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "codegraph-watch-exclude-{label}-{}-{nonce}",
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

/// The indexed paths, or `None` while a sync holds the index.
fn indexed(root: &Path) -> Option<Vec<String>> {
    let paths = IndexPaths::resolve(root, std::env::var("CODEGRAPH_DIR").ok().as_deref())
        .expect("resolve index paths");
    let store = Store::open_for_read(&paths, Instant::now() + Duration::from_millis(500), || {
        false
    })
    .ok()?;
    Some(
        store
            .all_files()
            .ok()?
            .into_iter()
            .map(|file| file.path)
            .filter(|path| path != "src/canary.ts")
            .collect(),
    )
}

fn wait_for_index(root: &Path, want: &[&str], step: &str) {
    let deadline = Instant::now() + APPLY_WITHIN;
    let mut last = None;
    loop {
        if let Some(now) = indexed(root) {
            if now == want {
                return;
            }
            last = Some(now);
        }
        assert!(
            Instant::now() < deadline,
            "{step}: the index still held {last:?} {APPLY_WITHIN:?} later, want {want:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[allow(clippy::field_reassign_with_default)] // WatchOptions has private test hooks.
fn editing_the_repository_exclude_file_rescopes_the_live_index() {
    let root = TestRoot::new("live");
    fs::create_dir_all(root.path().join(".git/info")).unwrap();
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::create_dir_all(root.path().join("generated")).unwrap();
    fs::write(root.path().join("src/keep.ts"), "export const keep = 1;\n").unwrap();
    fs::write(
        root.path().join("generated/drop.ts"),
        "export const drop = 1;\n",
    )
    .unwrap();
    sync_project_once(root.path()).expect("build the index");
    assert_eq!(
        indexed(root.path()).as_deref(),
        Some(&["generated/drop.ts".to_string(), "src/keep.ts".to_string()][..])
    );

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
            root.path().join("src/canary.ts"),
            format!("export const canary = {canary_round};\n"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(250));
    }

    fs::write(root.path().join(".git/info/exclude"), "generated/\n").unwrap();
    wait_for_index(root.path(), &["src/keep.ts"], "after excluding generated/");

    fs::write(
        root.path().join(".git/info/exclude"),
        "# nothing excluded\n",
    )
    .unwrap();
    wait_for_index(
        root.path(),
        &["generated/drop.ts", "src/keep.ts"],
        "after clearing the exclude rule",
    );
    watcher.stop();
}
