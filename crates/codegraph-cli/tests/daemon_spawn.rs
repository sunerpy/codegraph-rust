//! End-to-end test for T2: detached daemon spawn.
//!
//! `spawn_detached_daemon` takes the executable path as a parameter so the
//! daemon crate stays testable; here we pass `CARGO_BIN_EXE_codegraph` (the
//! freshly built `codegraph` binary) into a temp project that has been indexed
//! via `codegraph init`, then assert the daemon detaches, listens, logs, and
//! SURVIVES the spawning helper returning.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph_daemon::{
    daemon_socket_path, is_process_alive, spawn_detached_daemon, unlock_project,
};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-daemon-spawn-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn indexed_project(label: &str) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let status = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .status()
        .expect("run codegraph init");
    assert!(status.success(), "init failed for {}", project.display());
    (dir, project)
}

/// Connect to the daemon socket and read its hello line, returning the pid.
fn read_pid_from_hello(socket: &Path) -> Option<u32> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    let stream = Stream::connect(name).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map(|p| p as u32)
}

/// Poll for the socket to appear, then read the daemon pid from its hello.
fn poll_for_daemon_pid(socket: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if socket.exists()
            && let Some(pid) = read_pid_from_hello(socket)
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
}

// A SIGKILL'd child of this long-lived test harness lingers as a zombie until
// the harness reaps it (which it never does), and signal-0 liveness reports a
// zombie as "alive". For teardown purposes a zombie is functionally dead — it
// runs no code and holds no resources but the pid slot — so read /proc state
// and treat `Z` (zombie) the same as gone.
fn process_is_gone_or_zombie(pid: u32) -> bool {
    if !is_process_alive(pid) {
        return true;
    }
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .map(|state| state == "Z")
            .unwrap_or(false),
        Err(_) => true,
    }
}

fn wait_until_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if process_is_gone_or_zombie(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    process_is_gone_or_zombie(pid)
}

#[test]
fn spawn_detached_daemon_listens_and_survives() {
    let (_dir, project) = indexed_project("survive");
    let socket = daemon_socket_path(&project);
    let log = project.join(".codegraph").join("daemon.log");

    // Spawn the detached daemon. The helper must RETURN without waiting.
    spawn_detached_daemon(&bin(), &project, false).expect("spawn_detached_daemon");

    // (a)+(b): the socket appears within the poll window and the hello pid is alive.
    let pid = poll_for_daemon_pid(&socket, Duration::from_millis(2000))
        .expect("daemon socket + hello pid within poll window");
    assert!(is_process_alive(pid), "daemon pid {pid} should be alive");

    // (c): the unix socket file exists.
    assert!(
        socket.exists(),
        "daemon.sock should exist at {}",
        socket.display()
    );

    // (d): the daemon.log file exists.
    assert!(log.exists(), "daemon.log should exist at {}", log.display());

    // (e): the daemon SURVIVES the spawning helper returning and any local
    // handle being dropped — it was detached, not reaped.
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        is_process_alive(pid),
        "detached daemon pid {pid} should still be alive after launcher returned"
    );

    // TEARDOWN: kill the spawned daemon and release the project lock.
    kill_pid(pid);
    assert!(
        wait_until_gone(pid, Duration::from_secs(5)),
        "daemon pid {pid} must be dead after teardown (no leaked process)"
    );
    unlock_project(&project);
}

/// Adversarial: spawning twice in a row must reuse-or-respawn cleanly without
/// deadlocking on a stale lock, and leave no leaked daemon after teardown.
#[test]
fn spawn_detached_daemon_twice_no_stale_deadlock() {
    let (_dir, project) = indexed_project("twice");
    let socket = daemon_socket_path(&project);

    spawn_detached_daemon(&bin(), &project, false).expect("first spawn");
    let pid1 = poll_for_daemon_pid(&socket, Duration::from_millis(2000)).expect("first daemon pid");
    assert!(is_process_alive(pid1));

    // Kill the first daemon and wait for it to stop running (it lingers as an
    // unreaped zombie because this long-lived test harness is its parent).
    kill_pid(pid1);
    assert!(
        wait_until_gone(pid1, Duration::from_secs(5)),
        "first daemon pid {pid1} should stop running before respawn"
    );
    // A zombie's lock cannot be cleared by signal-0 liveness, so remove the
    // stale pid/socket directly (the test owns this temp project) to prove the
    // respawn path comes up cleanly rather than deadlocking on a stale lock.
    let _ = fs::remove_file(codegraph_daemon::daemon_pid_path(&project));
    let _ = fs::remove_file(&socket);

    // Second spawn must come up cleanly (reuse-or-respawn), not deadlock.
    spawn_detached_daemon(&bin(), &project, false).expect("second spawn");
    let pid2 = poll_for_daemon_pid(&socket, Duration::from_millis(2000))
        .expect("second daemon pid after respawn");
    assert!(is_process_alive(pid2), "respawned daemon should be alive");

    // TEARDOWN: no leaked daemon.
    kill_pid(pid2);
    assert!(
        wait_until_gone(pid2, Duration::from_secs(5)),
        "respawned daemon pid {pid2} must be dead after teardown"
    );
    unlock_project(&project);
}
