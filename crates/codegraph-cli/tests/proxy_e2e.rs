//! End-to-end test for T7: the local-handshake proxy.
//!
//! Spawns the REAL detached daemon into a temp indexed project (reusing the T2
//! spawn helper + `CARGO_BIN_EXE_codegraph`), then drives `run_proxy` over
//! in-memory pipes with an `initialize` -> `tools/list` -> `tools/call`
//! JSON-RPC sequence and asserts:
//!   (a) `initialize` + `tools/list` are answered LOCALLY (match this build's
//!       static constants),
//!   (b) the daemon hello line NEVER leaks into the host-facing output,
//!   (c) the `tools/call` (codegraph_search) round-trips a real result from the
//!       daemon,
//!   (d) `run_proxy` EXITS on host_in EOF (the whole drive is wrapped in a
//!       watchdog timeout so a hang fails loudly).

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codegraph_daemon::{
    ProxyOutcome, current_ppid, daemon_socket_path, is_process_alive, run_proxy,
    spawn_detached_daemon, unlock_project,
};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};
use serde_json::{Value, json};

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
            "codegraph-proxy-e2e-{label}-{}-{}",
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

fn read_pid_from_hello(socket: &Path) -> Option<u32> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    let stream = Stream::connect(name).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    value.get("pid").and_then(Value::as_u64).map(|p| p as u32)
}

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

fn wait_until_gone(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if process_is_gone_or_zombie(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `Write` sink backed by a shared buffer the test can inspect after
/// `run_proxy` consumes its `W` by value.
#[derive(Clone)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Drive `run_proxy` on a worker thread with the three framed requests, bounded
/// by a hard timeout so a hang fails the test instead of wedging CI.
fn run_proxy_oneshot(
    socket: &Path,
    requests: &str,
    timeout: Duration,
) -> (ProxyOutcome, Vec<String>) {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = SharedSink(Arc::clone(&buf));
    let host_in = Cursor::new(requests.to_string().into_bytes());
    let socket = socket.to_path_buf();
    let ppid = current_ppid();

    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let outcome = run_proxy(&socket, Some(ppid), host_in, sink);
        let _ = tx.send(outcome);
    });

    let outcome = rx
        .recv_timeout(timeout)
        .expect("run_proxy must exit on host_in EOF within the timeout (no hang)")
        .expect("run_proxy returned an error");
    handle.join().expect("proxy thread joins");

    let raw = buf.lock().unwrap().clone();
    let text = String::from_utf8(raw).expect("host output is utf8");
    let lines = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    (outcome, lines)
}

#[test]
fn proxy_local_handshake_and_forwarded_tool_call() {
    let (_dir, project) = indexed_project("handshake");
    let socket = daemon_socket_path(&project);

    spawn_detached_daemon(&bin(), &project, false).expect("spawn detached daemon");
    let pid =
        poll_for_daemon_pid(&socket, Duration::from_millis(3000)).expect("daemon up with hello");
    assert!(is_process_alive(pid), "daemon pid {pid} alive");

    // initialize (id 1) -> tools/list (id 2) -> tools/call codegraph_search (id 3).
    let requests = format!(
        "{}\n{}\n{}\n",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
               "params":{"name":"codegraph_search","arguments":{"query":"add"}}}),
    );

    let (outcome, lines) = run_proxy_oneshot(&socket, &requests, Duration::from_secs(20));
    assert_eq!(
        outcome,
        ProxyOutcome::Proxied,
        "proxy should report Proxied"
    );

    // (b) the daemon hello line must never appear in host output.
    for line in &lines {
        assert!(
            !line.contains("\"socketPath\""),
            "daemon hello leaked into host output: {line}"
        );
        let v: Value = serde_json::from_str(line).expect("each host line is JSON-RPC");
        // The hello carries `codegraph` + `pid` at top level with no jsonrpc id;
        // a real JSON-RPC reply always has an `id`.
        assert!(
            v.get("id").is_some(),
            "non-JSON-RPC line (hello?) leaked: {line}"
        );
    }

    let by_id = |want: i64| -> Value {
        lines
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v.get("id").and_then(Value::as_i64) == Some(want))
            .unwrap_or_else(|| panic!("no response for id {want}: {lines:?}"))
    };

    // (a) initialize answered LOCALLY == this build's static constant.
    let init_resp = by_id(1);
    assert_eq!(
        init_resp["result"],
        codegraph_mcp::initialize_result(),
        "initialize must be answered locally from the static constant"
    );

    // (a) tools/list answered LOCALLY == this build's static visible tools.
    let list_resp = by_id(2);
    assert_eq!(
        list_resp["result"]["tools"],
        codegraph_mcp::schemas::visible_tool_definitions(),
        "tools/list must be answered locally from the static definitions"
    );

    // The suppressed daemon initialize reply must NOT produce a SECOND id:1.
    let id1_count = lines
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v.get("id").and_then(Value::as_i64) == Some(1))
        .count();
    assert_eq!(
        id1_count, 1,
        "the forwarded initialize reply must be suppressed"
    );

    // (c) the tools/call round-trips a real result from the daemon.
    let call_resp = by_id(3);
    assert!(
        call_resp.get("result").is_some(),
        "tools/call must return a result forwarded from the daemon: {call_resp}"
    );
    let content = &call_resp["result"]["content"];
    assert!(
        content.is_array(),
        "forwarded tools/call result has MCP content: {call_resp}"
    );

    // TEARDOWN.
    kill_pid(pid);
    wait_until_gone(pid, Duration::from_secs(5));
    unlock_project(&project);
}
