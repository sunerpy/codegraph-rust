//! T9 end-to-end: dead-client sweep via the client-hello pid protocol.
//!
//! A client that announces a host pid via the optional `{"hostPid":N}` hello is
//! reaped by the daemon's periodic sweep once that pid dies — its session is
//! force-closed (socket shutdown → reader EOF → `SessionGuard` drop →
//! `active_count` decrements). A client that sends NO hello is never swept and
//! keeps being served. With `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS=200` the sweep
//! fires fast; we poll pid liveness / daemon exit against a clear deadline.
//!
//! The daemon is detached + logs to `.codegraph/daemon.log`, so lifecycle is
//! asserted via the daemon pid (idle-exit once no live clients remain) — exactly
//! the observable surface `daemon_idle.rs` uses.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
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
            "codegraph-daemon-sweep-{label}-{}-{}",
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

/// Spawn the detached daemon with a SHORT sweep + idle window so the test is
/// fast. `Command` snapshots env at spawn time, so the daemon inherits these.
fn spawn_sweep_daemon(project: &Path) {
    std::env::set_var("CODEGRAPH_DAEMON_CLIENT_SWEEP_MS", "200");
    std::env::set_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "500");
    std::env::set_var("CODEGRAPH_WATCH_DEBOUNCE_MS", "100");
    spawn_detached_daemon(&bin(), project).expect("spawn_detached_daemon");
    std::env::remove_var("CODEGRAPH_DAEMON_CLIENT_SWEEP_MS");
    std::env::remove_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS");
}

fn open_stream(socket: &Path) -> Option<Stream> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    Stream::connect(name).ok()
}

/// Connect, read the daemon hello, then SEND a client-hello carrying `host_pid`.
/// Returns the live stream so the connection stays open.
fn connect_with_hello(socket: &Path, host_pid: u32) -> Option<Stream> {
    let stream = open_stream(socket)?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut w = &stream;
    writeln!(w, "{{\"hostPid\":{host_pid}}}").ok()?;
    w.flush().ok()?;
    Some(stream)
}

/// Connect + read hello but send NO client-hello (a normal client).
fn connect_no_hello(socket: &Path) -> Option<Stream> {
    let stream = open_stream(socket)?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    Some(stream)
}

fn read_pid_from_hello(socket: &Path) -> Option<u32> {
    let stream = open_stream(socket)?;
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

/// A throwaway child whose pid we hand to the daemon as the client's host pid,
/// then kill to make the peer "dead" for the sweep.
fn spawn_throwaway_child() -> Child {
    Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn sleep child")
}

/// HAPPY: a client that announced a now-dead host pid is reaped by the sweep.
/// Observable: with the dead client swept and no other live client, the daemon
/// idle-exits (its `active_count` dropped to 0 so the idle path can fire).
#[test]
fn dead_client_is_swept_and_daemon_idle_exits() {
    let (_dir, project) = indexed_project("dead");
    let socket = daemon_socket_path(&project);

    spawn_sweep_daemon(&project);
    let daemon_pid = poll_for_daemon_pid(&socket, Duration::from_millis(3000))
        .expect("daemon socket + hello pid within poll window");

    // A throwaway child supplies the host pid the client announces.
    let mut child = spawn_throwaway_child();
    let child_pid = child.id();

    // Connect a client that announces the (still-live) child's pid, then kill
    // the child so the sweep sees a dead peer and reaps the session.
    let client = connect_with_hello(&socket, child_pid).expect("client connects + sends hello");
    std::thread::sleep(Duration::from_millis(100));
    kill_pid(child_pid);
    let _ = child.wait();

    // Hold the client socket open: it must NOT be what keeps the daemon alive —
    // the sweep should force-close the (dead-pid) session regardless.
    // The daemon should idle-exit within sweep + idle window + margin.
    let exited = wait_until_gone(daemon_pid, Duration::from_millis(3000));

    // TEARDOWN before asserting.
    drop(client);
    if !exited {
        kill_pid(daemon_pid);
        let _ = wait_until_gone(daemon_pid, Duration::from_secs(5));
    }
    kill_pid(child_pid);
    unlock_project(&project);

    assert!(
        exited,
        "daemon pid {daemon_pid} must idle-exit after the dead-pid client is swept"
    );
}

/// FAILURE-GUARD: a client that sent NO hello is never swept. Held open across
/// several sweep windows, the daemon must stay alive (the connection is active
/// and the client has no known pid to reap).
#[test]
fn client_without_hello_is_never_swept() {
    let (_dir, project) = indexed_project("nohello");
    let socket = daemon_socket_path(&project);

    spawn_sweep_daemon(&project);
    let daemon_pid = poll_for_daemon_pid(&socket, Duration::from_millis(3000))
        .expect("daemon socket + hello pid within poll window");

    // A normal client that sends no client-hello; keep it open across many sweeps.
    let client = connect_no_hello(&socket).expect("no-hello client connects");

    // Well past several sweep windows (200ms) + the idle window (500ms): the
    // daemon must NOT exit because this active client is never swept.
    std::thread::sleep(Duration::from_millis(1500));
    let alive = is_process_alive(daemon_pid);

    // TEARDOWN.
    drop(client);
    kill_pid(daemon_pid);
    let gone = wait_until_gone(daemon_pid, Duration::from_secs(5));
    unlock_project(&project);

    assert!(
        alive,
        "daemon pid {daemon_pid} must stay alive while a no-hello client is connected (never swept)"
    );
    assert!(gone, "daemon pid {daemon_pid} must be dead after teardown");
}
