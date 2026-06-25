//! End-to-end test for T8: daemon idle-linger + max-idle backstop.
//!
//! The daemon lives in a SEPARATE detached process, so we assert lifecycle via
//! pid liveness (`is_process_alive`) at timed checkpoints, not via stdout (it is
//! detached + logs to `.codegraph/daemon.log`). With a multi-second
//! `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` the daemon should LINGER while a client is
//! connected and shortly after the LAST client disconnects, then EXIT once the
//! idle window elapses with zero clients. De-flake by polling pid liveness
//! against a clear deadline AND by keeping the "still alive" observation a tiny
//! fraction of the idle window so CI scheduling jitter cannot flip the edge.

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
            "codegraph-daemon-idle-{label}-{}-{}",
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

/// Spawn the detached daemon with a SHORT idle timeout so the test runs fast.
/// `Command` (inside `spawn_detached_daemon`) snapshots env at spawn time, so
/// setting it here is inherited by the daemon process.
fn spawn_idle_daemon(project: &Path, idle_ms: &str) {
    std::env::set_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", idle_ms);
    std::env::set_var("CODEGRAPH_WATCH_DEBOUNCE_MS", "100");
    spawn_detached_daemon(&bin(), project).expect("spawn_detached_daemon");
    std::env::remove_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS");
}

/// Connect a client, read+discard the hello line, and return the live stream so
/// the connection stays open (an active client).
fn connect_client(socket: &Path) -> Option<Stream> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    let stream = Stream::connect(name).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    Some(stream)
}

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

fn poll_for_daemon_pid(socket: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if socket.exists() {
            if let Some(pid) = read_pid_from_hello(socket) {
                return Some(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
}

// A SIGKILL'd child of this long-lived test harness lingers as a zombie until
// the harness reaps it; signal-0 liveness reports a zombie as "alive". For
// teardown a zombie is functionally dead, so read /proc state and treat `Z`
// (zombie) the same as gone.
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

/// HAPPY: with a 2000ms idle timeout, the daemon stays alive right after the last
/// client disconnects (< idle) and idle-exits once the window elapses (> idle).
#[test]
fn daemon_idle_exits_after_last_client() {
    let (_dir, project) = indexed_project("exits");
    let socket = daemon_socket_path(&project);

    spawn_idle_daemon(&project, "2000");
    let pid = poll_for_daemon_pid(&socket, Duration::from_millis(3000))
        .expect("daemon socket + hello pid within poll window");

    // Connect ONE client, then disconnect — this bumps last_active on both edges.
    {
        let client = connect_client(&socket).expect("client connects");
        std::thread::sleep(Duration::from_millis(50));
        drop(client);
    }

    // At ~200ms after disconnect (≪ 2000ms idle) the daemon must still be alive.
    // 200ms is a tiny fraction (10%) of the idle window, so even heavy CI
    // scheduling jitter cannot stretch this observation past idle-exit.
    std::thread::sleep(Duration::from_millis(200));
    let alive_before_idle = is_process_alive(pid);

    // GENEROUS deadline, not a tight margin: `wait_until_gone` polls every 20ms and
    // returns the instant the process is gone, so the happy path still finishes in
    // ~2-2.5s — a 10s ceiling costs nothing on a quiet machine but stops a loaded
    // CI runner's slow idle-sweep + teardown from false-negativing (de-flake).
    let exited = wait_until_gone(pid, Duration::from_secs(10));

    // TEARDOWN before asserting so a failing assert never leaks the process.
    if !exited {
        kill_pid(pid);
        let _ = wait_until_gone(pid, Duration::from_secs(5));
    }
    unlock_project(&project);

    assert!(
        alive_before_idle,
        "daemon pid {pid} must still be alive ~200ms after last disconnect (< idle window)"
    );
    assert!(
        exited,
        "daemon pid {pid} must idle-exit within idle window + margin after last client left"
    );
}

/// FAILURE-GUARD: a connected, held-open client prevents idle-exit — the daemon
/// stays alive well past the idle window while a client is active.
#[test]
fn daemon_stays_alive_with_active_client() {
    let (_dir, project) = indexed_project("active");
    let socket = daemon_socket_path(&project);

    spawn_idle_daemon(&project, "500");
    let pid = poll_for_daemon_pid(&socket, Duration::from_millis(3000))
        .expect("daemon socket + hello pid within poll window");

    // Keep TWO clients connected across the whole idle window.
    let client_a = connect_client(&socket).expect("first client connects");
    let client_b = connect_client(&socket).expect("second client connects");

    // Past the idle window (500ms) + generous margin, the daemon must NOT exit
    // because clients are still active.
    std::thread::sleep(Duration::from_millis(1200));
    let alive_with_clients = is_process_alive(pid);

    drop(client_a);
    drop(client_b);

    // TEARDOWN: stop the daemon and confirm no leak.
    kill_pid(pid);
    let gone = wait_until_gone(pid, Duration::from_secs(5));
    unlock_project(&project);

    assert!(
        alive_with_clients,
        "daemon pid {pid} must stay alive past the idle window while clients are active"
    );
    assert!(
        gone,
        "daemon pid {pid} must be dead after teardown (no leaked process)"
    );
}
