#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codegraph_watch::{ProjectWatcher, WatchOptions};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

const RECREATE_CYCLES: usize = 96;
const EVENT_SETTLE: Duration = Duration::from_millis(30);
const MAX_HANDLE_DELTA: u32 = 24;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "codegraph-watch-handle-stability-{}-{nonce}",
            process::id()
        ));
        fs::create_dir_all(&path).expect("create watcher test root");
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

fn process_handle_count() -> u32 {
    let mut count = 0;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    assert_ne!(ok, 0, "GetProcessHandleCount failed");
    count
}

#[test]
#[allow(clippy::field_reassign_with_default)] // WatchOptions has a private test hook.
fn recreating_one_directory_keeps_windows_handle_count_bounded() {
    let root = TestRoot::new();
    let recreated = root.path().join("recreated");
    let baseline = process_handle_count();
    let mut options = WatchOptions::default();
    options.debounce = Duration::from_millis(50);
    let watcher = ProjectWatcher::start(root.path(), options)
        .expect("start project watcher")
        .expect("watching should be enabled for a temporary project");

    for _ in 0..RECREATE_CYCLES {
        fs::create_dir(&recreated).expect("create watched directory");
        thread::sleep(EVENT_SETTLE);
        fs::remove_dir(&recreated).expect("remove watched directory");
        thread::sleep(EVENT_SETTLE);
    }

    thread::sleep(Duration::from_millis(250));
    watcher.stop();
    let final_count = process_handle_count();
    let delta = final_count.saturating_sub(baseline);

    assert!(
        delta <= MAX_HANDLE_DELTA,
        "recreating one directory {RECREATE_CYCLES} times leaked handles: \
         baseline={baseline}, final={final_count}, delta={delta}, max={MAX_HANDLE_DELTA}"
    );
}
