//! Cold-start MCP handshake latency guard (fix/cli-cold-start-handshake-race).
//!
//! Regression guard for the cold-start handshake race: opencode marked
//! codegraph `failed` because a COLD `serve --mcp` used to poll+proxy a
//! freshly-spawned shared daemon before answering `initialize`, and that
//! spawn→poll→proxy prelude blew past the MCP init timeout.
//!
//! The fix makes the COLD `SpawnOrProxy` path fire-and-forget the shared daemon
//! (`spawn_shared_daemon_best_effort`) and then serve THIS session DIRECT
//! immediately — so the handshake is answered fast, without blocking on daemon
//! socket readiness. This test exercises the real cold path (NO
//! `CODEGRAPH_NO_DAEMON`, so `serve_spawn_or_proxy` is not forced to direct
//! mode) and asserts:
//!
//!   (a) the `initialize` response arrives within a STRICT bound (< 2s),
//!   (b) the `tools/list` response arrives within the same strict bound and its
//!       tool list CONTAINS `codegraph_search` AND `codegraph_explore`.
//!
//! The 2s bound is the whole point of the fix: because the cold path no longer
//! blocks on the daemon prelude, the handshake must be near-instant. It is also
//! the same bound `catch_up.rs` uses for its forced-direct first-call latency,
//! so it is a proven, non-flaky ceiling on a loaded box.
//!
//! Teardown reaps BOTH the serve child (RAII `ServeProcess`) AND the shared
//! daemon the cold path fire-and-forget spawned: we read the daemon pid from
//! `<project>/.codegraph/daemon.pid` and kill it, so no orphan `codegraph`
//! daemon process leaks.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Strict handshake latency bound. The cold path no longer blocks on the daemon
/// spawn→poll→proxy prelude, so both `initialize` and `tools/list` must return
/// near-instantly; 2s is the same proven ceiling `catch_up.rs` uses.
const HANDSHAKE_BOUND: Duration = Duration::from_secs(2);

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
            "codegraph-cli-coldstart-{label}-{}-{}",
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

fn cli(args: &[&str]) -> (String, String, bool) {
    let output = Command::new(bin())
        .args(args)
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// A live COLD `serve --mcp` child driven over line-delimited JSON-RPC. Killed +
/// reaped on drop so no orphan serve process leaks. The `BufReader` is created
/// ONCE and held for the process lifetime (re-creating it per read would drop
/// buffered bytes). Modeled on `catch_up.rs`'s `ServeProcess`, but WITHOUT
/// `CODEGRAPH_NO_DAEMON` so it exercises the real cold `SpawnOrProxy` path.
struct ServeProcess {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl ServeProcess {
    fn spawn(project: &Path) -> Self {
        let mut cmd = Command::new(bin());
        cmd.arg("serve")
            .arg("--mcp")
            .arg("--path")
            .arg(project)
            // COLD start: NO CODEGRAPH_NO_DAEMON — we want serve_spawn_or_proxy
            // to take the real cold path (fire-and-forget daemon + serve direct),
            // NOT forced-direct mode. `--no-watch` keeps the fire-and-forget
            // daemon watcher-free so teardown is clean; it does not change the
            // cold decision (daemon still spawns, this session still serves
            // direct).
            .arg("--no-watch")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn serve --mcp (cold)");
        let stdin = child.stdin.take().expect("serve stdin");
        let stdout = child.stdout.take().expect("serve stdout");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
        }
    }

    fn send(&mut self, line: &str) {
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.write_all(b"\n").expect("write newline");
        self.stdin.flush().expect("flush request");
    }

    /// Read line-delimited JSON-RPC responses until one carries `want_id`,
    /// skipping notifications / blank lines. Returns `None` on EOF or `deadline`.
    fn read_response(&mut self, want_id: u64, deadline: Instant) -> Option<serde_json::Value> {
        loop {
            if Instant::now() > deadline {
                return None;
            }
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
                        && value.get("id").and_then(serde_json::Value::as_u64) == Some(want_id)
                    {
                        return Some(value);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the live daemon pid recorded under `<project>/.codegraph/daemon.pid`, if
/// any. The cold path fire-and-forget spawns this daemon; we reap it in teardown
/// so no orphan `codegraph` daemon process survives the test.
fn recorded_daemon_pid(project: &Path) -> Option<u32> {
    let pid_path = codegraph_daemon::daemon_pid_path(project);
    let raw = fs::read_to_string(&pid_path).ok()?;
    codegraph_daemon::decode_lock_info(&raw)
        .filter(|info| info.pid > 0)
        .map(|info| info.pid)
}

/// Poll for the fire-and-forget daemon to record a live pid, up to `timeout`.
fn poll_for_daemon_pid(project: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(pid) = recorded_daemon_pid(project)
            && codegraph_daemon::is_process_alive(pid)
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    recorded_daemon_pid(project)
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
}

#[test]
fn cold_start_serve_answers_handshake_within_bound() {
    let dir = TestDir::new("handshake");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    // Index the project so the served graph is ready (no daemon involved yet).
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", project.to_str().unwrap()]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    // COLD start: no shared daemon is live yet, so serve_spawn_or_proxy takes
    // the cold path (fire-and-forget daemon spawn + serve THIS session direct).
    let mut serve = ServeProcess::spawn(&project);

    // (a) initialize must return within the strict bound.
    serve.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"cold-start-test","version":"0"}}}"#);
    let init_started = Instant::now();
    let init = serve
        .read_response(1, init_started + HANDSHAKE_BOUND)
        .expect("initialize must respond within the cold-start handshake bound");
    let init_latency = init_started.elapsed();
    assert!(
        init_latency < HANDSHAKE_BOUND,
        "cold-start initialize must not block on the daemon prelude; took {init_latency:?}"
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        serde_json::json!("codegraph"),
        "cold-start serve must identify as codegraph: {init}"
    );

    // MCP spec: send the initialized notification after initialize.
    serve.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // (b) tools/list must return within the strict bound AND expose the
    // codegraph tools (registered without a server prefix in the MCP engine).
    serve.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list_started = Instant::now();
    let list = serve
        .read_response(2, list_started + HANDSHAKE_BOUND)
        .expect("tools/list must respond within the cold-start handshake bound");
    let list_latency = list_started.elapsed();
    assert!(
        list_latency < HANDSHAKE_BOUND,
        "cold-start tools/list must not block on the daemon prelude; took {list_latency:?}"
    );

    let names: Vec<String> = list["result"]["tools"]
        .as_array()
        .expect("tools/list result must carry a tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    assert!(
        names.iter().any(|n| n == "codegraph_search"),
        "tools/list must include codegraph_search; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "codegraph_explore"),
        "tools/list must include codegraph_explore; got {names:?}"
    );

    // TEARDOWN: reap the fire-and-forget shared daemon the cold path spawned so
    // no orphan `codegraph` daemon process leaks. The daemon binds its socket +
    // writes its pid asynchronously; poll briefly for it, then kill by pid.
    if let Some(daemon_pid) = poll_for_daemon_pid(&project, Duration::from_secs(3)) {
        kill_pid(daemon_pid);
    }
    // The serve child is killed+reaped by ServeProcess::drop; the temp project
    // tree (incl. .codegraph/) is removed by TestDir::drop.
}
