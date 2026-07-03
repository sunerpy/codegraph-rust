//! Phase D — the CLI direct stdio serve path (`serve --mcp`) routes through the
//! rmcp `CodeGraphHandler` when built `--features rmcp` and opted in with
//! `CODEGRAPH_DAEMON_RMCP=1`.
//!
//! Drives the real `codegraph` binary end-to-end from an INDEXED cwd with the
//! daemon disabled (`CODEGRAPH_NO_DAEMON=1`, forcing `serve_direct`), sends an
//! `initialize` + a `tools/call codegraph_search` over stdio, and asserts a
//! non-empty, non-error tool result — proving the rmcp direct path serves the
//! same tools as the hand-rolled path. Then closes stdin and confirms the
//! process exits (stdin EOF → rmcp serve ends → block_on returns → exit).
//!
//! Requires the `rmcp` feature; the whole file is gated so the default
//! `cargo test -p codegraph-rs` skips it.
#![cfg(all(unix, feature = "rmcp"))]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-rmcp-serve-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let status = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run codegraph init");
    assert!(status.success(), "init failed for {}", project.display());
    project
}

fn read_json_line_with_id(
    reader: &mut impl BufRead,
    want_id: i64,
    deadline: Instant,
) -> Option<Value> {
    loop {
        if Instant::now() > deadline {
            return None;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(trimmed)
                    && value.get("id").and_then(Value::as_i64) == Some(want_id)
                {
                    return Some(value);
                }
            }
            Err(_) => return None,
        }
    }
}

#[test]
fn serve_mcp_direct_routes_through_rmcp_handler() {
    // GIVEN an indexed project served directly (daemon disabled) with the rmcp
    // path opted in.
    let home = TestDir::new("indexed");
    let indexed = indexed_project(&home);

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", indexed.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_DAEMON_RMCP", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp (rmcp)");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(20);

    // WHEN initialize + a tools/call are sent over stdio.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-serve-test", "version": "0" }
        }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();

    let init_resp = read_json_line_with_id(&mut stdout, 1, deadline).expect("initialize result");
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"],
        json!("codegraph"),
        "the rmcp handler must identify as codegraph"
    );

    // The MCP spec requires the `initialized` notification after initialize.
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    writeln!(stdin, "{initialized}").unwrap();
    stdin.flush().unwrap();

    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "codegraph_search", "arguments": { "query": "add" } }
    });
    writeln!(stdin, "{call}").unwrap();
    stdin.flush().unwrap();

    let call_resp = read_json_line_with_id(&mut stdout, 2, deadline).expect("tools/call response");

    // THEN the tool call resolves against the pinned indexed project — non-empty
    // and not an error.
    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert_ne!(
        call_resp["result"]["isError"],
        json!(true),
        "rmcp direct-serve tool call must not error: {text}"
    );
    assert!(
        text.contains("add"),
        "rmcp direct-serve search must return results for the pinned index: {text}"
    );

    // Close stdin → EOF → rmcp serve ends → process exits.
    drop(stdin);
    let exited = wait_with_timeout(&mut child, Duration::from_secs(10));
    assert!(
        exited,
        "serve --mcp (rmcp) must exit after stdin EOF (rmcp serve loop must end on stream close)"
    );
}

/// Poll for process exit within `timeout`, returning whether it exited (killing
/// it on timeout so the test process never leaks a child).
fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}
