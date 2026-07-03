use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{
    DaemonLockInfo, DaemonOptions, StartOrAttach, daemon_pid_path, encode_lock_info,
    start_or_attach, unlock_project,
};

#[test]
fn second_daemon_attaches_to_existing_project_daemon() {
    let project = temp_project("single-instance");
    let options = test_options(None);

    let first = match start_or_attach(&project, options.clone()).expect("first daemon starts") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("first start unexpectedly attached"),
    };

    let second = start_or_attach(&project, options).expect("second daemon attaches");
    match second {
        StartOrAttach::Attached(client) => {
            assert_eq!(client.hello["protocol"], 1);
            assert_eq!(client.hello["pid"], std::process::id());
        }
        StartOrAttach::Started(_) => panic!("second start created a daemon"),
    }

    first.stop().expect("daemon stops");
    assert!(!daemon_pid_path(&project).exists());
    let _ = fs::remove_dir_all(project);
}

#[test]
fn host_pid_watchdog_stops_daemon_after_parent_agent_exits() {
    let project = temp_project("ppid-watchdog");
    // `sleep` is Unix-only; Windows uses PowerShell `Start-Sleep` (always present).
    #[cfg(unix)]
    let mut parent = Command::new("sleep")
        .arg("0.1")
        .spawn()
        .expect("spawn parent");
    #[cfg(windows)]
    let mut parent = Command::new("powershell")
        .args(["-NoProfile", "-Command", "Start-Sleep -Milliseconds 100"])
        .spawn()
        .expect("spawn parent");
    let parent_pid = parent.id();

    let handle =
        match start_or_attach(&project, test_options(Some(parent_pid))).expect("daemon starts") {
            StartOrAttach::Started(handle) => handle,
            StartOrAttach::Attached(_) => panic!("first start unexpectedly attached"),
        };

    parent.wait().expect("parent exits");
    for _ in 0..100 {
        if handle.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert!(
        handle.is_finished(),
        "daemon did not stop after watched pid exited"
    );
    handle.stop().expect("finished daemon joins");
    assert!(!daemon_pid_path(&project).exists());
    let _ = fs::remove_dir_all(project);
}

#[test]
fn unlock_project_removes_stale_daemon_lock() {
    let project = temp_project("unlock-stale");
    let codegraph_dir = project.join(".codegraph");
    fs::create_dir_all(&codegraph_dir).expect("create .codegraph");
    let pid_path = daemon_pid_path(&project);
    let info = DaemonLockInfo {
        pid: 999_999_999,
        version: "test".to_string(),
        socket_path: codegraph_dir.join("daemon.sock"),
        started_at: 1,
    };
    fs::write(&pid_path, encode_lock_info(&info).expect("serialize lock")).expect("write lock");

    assert!(unlock_project(&project));
    assert!(!pid_path.exists());
    let _ = fs::remove_dir_all(project);
}

fn test_options(host_pid: Option<u32>) -> DaemonOptions {
    DaemonOptions {
        host_pid,
        watchdog_interval: Duration::from_millis(10),
        run_mcp: false,
        ..DaemonOptions::default()
    }
}

fn temp_project(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "codegraph-daemon-{name}-{}-{nanos}",
        std::process::id()
    ));
    create_project(&path);
    path
}

fn create_project(path: &Path) {
    fs::create_dir_all(path.join(".codegraph")).expect("create project");
}
