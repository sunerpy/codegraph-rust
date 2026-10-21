//! A cold-start stdio session keeps the shared daemon it spawned (upstream #2293).
//!
//! A cold `serve --mcp` answers its handshake direct and spawns the shared
//! daemon fire-and-forget. The session itself holds no writer and runs no
//! watcher, so the daemon is what keeps its index live. This test drives the
//! real cold path with a one-second daemon idle timeout and asserts that:
//!
//! - the daemon outlives three idle windows while the session is open;
//! - an edit made during the session reaches `codegraph_search`;
//! - a daemon killed mid-session is started again within the retry window, and
//!   the next edit is indexed by the new one;
//! - everything the session writes to stdout is JSON-RPC.
//!
//! The session is pinned with `--path` and never passes `projectPath`, so the
//! explicit-project broker (which retains its own daemon lease) stays out of
//! the picture.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The daemon's idle timeout in this test; the session must keep it alive for
/// several of these windows.
const IDLE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the session must keep the daemon alive without any tool call.
const IDLE_OBSERVATION: Duration = Duration::from_secs(3);
/// Upper bound for an edit to reach `codegraph_search`.
const EDIT_VISIBLE_WITHIN: Duration = Duration::from_secs(10);
/// Upper bound for a killed daemon to be replaced: the 200 ms retry delay set
/// below, the daemon's own startup, and slack for a loaded runner.
const RESTART_WITHIN: Duration = Duration::from_secs(10);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cold-live-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_live_source(project: &Path, symbol: &str) {
    fs::write(
        project.join("src/live.ts"),
        format!("export function {symbol}(): number {{ return 1; }}\n"),
    )
    .unwrap();
}

fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    write_live_source(&project, "initialColdSessionSymbol");
    let output = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .env_remove("CODEGRAPH_DIR")
        .output()
        .expect("run codegraph init");
    assert!(
        output.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    project
}

/// A cold `serve --mcp --path <project>` driven over line-delimited JSON-RPC.
struct ColdSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_log: PathBuf,
    next_id: i64,
}

impl ColdSession {
    fn start(project: &Path, scratch: &Path) -> Self {
        let stderr_log = scratch.join("serve.stderr.log");
        let mut child = Command::new(bin())
            .args(["serve", "--mcp", "--path"])
            .arg(project)
            .env_remove("CODEGRAPH_NO_DAEMON")
            .env_remove("CODEGRAPH_NO_WATCH")
            .env_remove("CODEGRAPH_DIR")
            .env("CODEGRAPH_MCP_REGISTRY_DIR", scratch.join("mcp-registry"))
            .env("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "1000")
            .env("CODEGRAPH_DAEMON_RETRY_MS", "200")
            .env("CODEGRAPH_DAEMON_RETRY_MAX_MS", "1000")
            .env("CODEGRAPH_WATCH_DEBOUNCE_MS", "100")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&stderr_log).expect("create serve stderr log"))
            .spawn()
            .expect("spawn cold serve --mcp");
        let stdin = child.stdin.take().expect("serve stdin");
        let stdout = BufReader::new(child.stdout.take().expect("serve stdout"));
        let mut session = Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr_log,
            next_id: 1,
        };
        let init_id = session.next_id();
        session.write(json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "cold-start-live-sync", "version": "0" }
            }
        }));
        let response = session.read_id(init_id);
        assert_eq!(
            response["result"]["serverInfo"]["name"], "codegraph",
            "{response}"
        );
        session.write(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        session
    }

    fn next_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn write(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().expect("serve stdin stays open");
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    }

    /// Read until the response carrying `want`. Every stdout line must be a
    /// JSON-RPC message: a log line or banner here breaks the MCP client.
    fn read_id(&mut self, want: i64) -> Value {
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read serve stdout");
            assert!(
                read > 0,
                "serve closed stdout before response {want}; stderr:\n{}",
                self.stderr()
            );
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(trimmed).unwrap_or_else(|error| {
                panic!("non-JSON line on serve stdout ({error}): {trimmed:?}")
            });
            assert_eq!(
                value["jsonrpc"], "2.0",
                "non-JSON-RPC stdout line: {trimmed}"
            );
            if value.get("id").and_then(Value::as_i64) == Some(want) {
                return value;
            }
        }
    }

    fn search(&mut self, query: &str) -> String {
        let id = self.next_id();
        self.write(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": "codegraph_search", "arguments": { "query": query } }
        }));
        let response = self.read_id(id);
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn wait_for_symbol(&mut self, symbol: &str, within: Duration, when: &str) {
        let deadline = Instant::now() + within;
        loop {
            let text = self.search(symbol);
            if text.contains(symbol) && text.contains("src/live.ts") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{when}: `{symbol}` was not searchable within {within:?}; last result:\n{text}\nstderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr_log).unwrap_or_default()
    }

    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("cold serve did not exit after stdin EOF");
    }
}

impl Drop for ColdSession {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn daemon_pid(project: &Path) -> Option<u32> {
    let path = codegraph_daemon::daemon_pid_path(project).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    codegraph_daemon::decode_lock_info(&raw)
        .map(|info| info.pid)
        .filter(|&pid| pid > 0 && codegraph_daemon::is_process_alive(pid))
}

/// Wait for a live daemon owner other than `not`.
fn wait_for_daemon(project: &Path, not: Option<u32>, within: Duration, when: &str) -> u32 {
    let deadline = Instant::now() + within;
    loop {
        if let Some(pid) = daemon_pid(project)
            && Some(pid) != not
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "{when}: no live daemon (other than {not:?}) within {within:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Fail as soon as `pid` stops being alive during `window`.
fn assert_alive_throughout(pid: u32, window: Duration, session: &ColdSession) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        assert!(
            codegraph_daemon::is_process_alive(pid),
            "the daemon {pid} exited while the cold session was still open \
             (idle timeout {IDLE_TIMEOUT:?}); stderr:\n{}",
            session.stderr()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
}

fn shutdown_daemon(project: &Path) {
    let paths = codegraph_core::IndexPaths::resolve(project, None).unwrap();
    match codegraph_daemon::request_daemon_shutdown(project, paths.project_identity()) {
        Ok(codegraph_daemon::ShutdownOutcome::NoDaemon)
        | Ok(codegraph_daemon::ShutdownOutcome::Drained { .. }) => {}
        other => panic!("failed to stop the daemon: {other:?}"),
    }
}

#[test]
fn cold_start_session_keeps_its_daemon_and_live_sync() {
    let dir = TestDir::new("session");
    let project = indexed_project(&dir);
    let mut session = ColdSession::start(&project, dir.path());

    let first = wait_for_daemon(&project, None, RESTART_WITHIN, "after the cold start");
    assert_alive_throughout(first, IDLE_OBSERVATION, &session);

    let edited = "coldSessionEditAfterIdleWindows";
    write_live_source(&project, edited);
    session.wait_for_symbol(edited, EDIT_VISIBLE_WITHIN, "edit with the first daemon");

    kill_pid(first);
    let second = wait_for_daemon(
        &project,
        Some(first),
        RESTART_WITHIN,
        "after the first daemon was killed",
    );
    let after_restart = "coldSessionEditAfterDaemonRestart";
    write_live_source(&project, after_restart);
    session.wait_for_symbol(
        after_restart,
        EDIT_VISIBLE_WITHIN,
        "edit with the restarted daemon",
    );
    assert_eq!(
        daemon_pid(&project),
        Some(second),
        "one daemon owns the project after the restart"
    );

    session.close();
    shutdown_daemon(&project);
}
