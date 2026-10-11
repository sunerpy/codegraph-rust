//! A daemon whose install was replaced steps aside (upstream #2346).
//!
//! The daemon records the executable it runs when it starts. These tests point
//! that record at a stand-in file, replace or remove the file, and require the
//! daemon to drain and exit within two check intervals, removing its own
//! rendezvous. An untouched install keeps the daemon running.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{
    DaemonHandle, DaemonOptions, InstallIdentity, StartOrAttach, daemon_pid_path, start_or_attach,
};

const CHECK_EVERY: Duration = Duration::from_millis(100);
/// Two check intervals, plus slack for a loaded runner to finish the drain.
const EXIT_WITHIN: Duration = Duration::from_millis(2 * 100 + 1_800);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "codegraph-daemon-install-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("project")).expect("create project");
        Self(path.canonicalize().expect("canonicalize temp dir"))
    }

    fn project(&self) -> PathBuf {
        self.0.join("project")
    }

    fn install(&self) -> PathBuf {
        self.0.join("codegraph-install")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn start_daemon(dir: &TempDir) -> DaemonHandle {
    fs::write(dir.install(), b"the build the daemon started from").expect("write stand-in install");
    let options = DaemonOptions {
        watchdog_interval: Duration::from_millis(10),
        run_mcp: false,
        watch: false,
        install: Some(InstallIdentity::of(&dir.install()).expect("readable stand-in install")),
        install_check: Some(CHECK_EVERY),
        ..DaemonOptions::default()
    };
    match start_or_attach(dir.project(), options).expect("daemon starts") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("a fresh project attached to a daemon"),
    }
}

fn wait_finished(handle: &DaemonHandle, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if handle.is_finished() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle.is_finished()
}

fn assert_rendezvous_removed(project: &Path) {
    let pid_path = daemon_pid_path(project).expect("resolve the rendezvous pid path");
    assert!(
        !pid_path.exists(),
        "an exiting daemon removes its own pid record"
    );
}

#[test]
fn a_replaced_install_makes_the_daemon_exit() {
    let dir = TempDir::new("replaced");
    let handle = start_daemon(&dir);
    assert!(
        !wait_finished(&handle, 3 * CHECK_EVERY),
        "an untouched install keeps the daemon running"
    );

    // An upgrade writes the new build beside the old one and renames it over.
    let staged = dir.0.join("codegraph-install.new");
    fs::write(&staged, b"the build an upgrade installed").expect("stage the upgrade");
    fs::rename(&staged, dir.install()).expect("install the upgrade");

    assert!(
        wait_finished(&handle, EXIT_WITHIN),
        "the daemon kept running {EXIT_WITHIN:?} after its install was replaced"
    );
    handle.stop().expect("the finished daemon joins");
    assert_rendezvous_removed(&dir.project());
}

#[test]
fn a_removed_install_makes_the_daemon_exit() {
    let dir = TempDir::new("removed");
    let handle = start_daemon(&dir);
    fs::remove_file(dir.install()).expect("uninstall");
    assert!(
        wait_finished(&handle, EXIT_WITHIN),
        "the daemon kept running {EXIT_WITHIN:?} after its install was removed"
    );
    handle.stop().expect("the finished daemon joins");
    assert_rendezvous_removed(&dir.project());
}
