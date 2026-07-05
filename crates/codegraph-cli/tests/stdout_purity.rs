//! stdio MCP stdout-purity: `serve --mcp` owns stdout for the JSON-RPC stream,
//! so tracing logs MUST go to stderr, never stdout. A single log byte on stdout
//! corrupts the protocol.
//!
//! Drives the real `codegraph` binary through an `initialize` + `tools/call`
//! round-trip over stdio with `CODEGRAPH_DEBUG=1` (which bumps the log level to
//! debug, so the migrated `tracing::debug!` traces fire). Asserts every
//! non-empty stdout line parses as JSON-RPC, and that the debug traces landed on
//! stderr with an RFC3339 local timestamp instead.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
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
            "codegraph-stdout-purity-{label}-{}-{}",
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
    stdout_lines: &mut Vec<String>,
) -> Option<Value> {
    loop {
        if Instant::now() > deadline {
            return None;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                stdout_lines.push(trimmed.clone());
                if let Ok(value) = serde_json::from_str::<Value>(&trimmed)
                    && value.get("id").and_then(Value::as_i64) == Some(want_id)
                {
                    return Some(value);
                }
            }
            Err(_) => return None,
        }
    }
}

/// True when `line` starts with an RFC3339 timestamp once ANSI color escapes
/// (the fmt layer emits `ESC[2m…` before the timestamp) are stripped.
fn has_rfc3339_prefix(line: &str) -> bool {
    let stripped = strip_ansi(line);
    let bytes = stripped.as_bytes();
    if bytes.len() < 19 {
        return false;
    }
    let digit = |i: usize| bytes[i].is_ascii_digit();
    digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && bytes[4] == b'-'
        && digit(5)
        && digit(6)
        && bytes[7] == b'-'
        && digit(8)
        && digit(9)
        && bytes[10] == b'T'
        && digit(11)
        && digit(12)
        && bytes[13] == b':'
        && digit(14)
        && digit(15)
        && bytes[16] == b':'
        && digit(17)
        && digit(18)
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for tail in chars.by_ref() {
                if tail.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn stdio_serve_stdout_is_pure_jsonrpc_and_logs_go_to_stderr() {
    // GIVEN an indexed project served directly (daemon disabled) with
    // CODEGRAPH_DEBUG=1 so the migrated tracing::debug! traces fire.
    let home = TestDir::new("indexed");
    let indexed = indexed_project(&home);

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", indexed.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_DEBUG", "1")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve --mcp");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let mut stderr_handle = child.stderr.take().expect("child stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_handle.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stdout_lines: Vec<String> = Vec::new();

    // WHEN initialize + initialized + a tools/call are sent over stdio.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "stdout-purity-test", "version": "0" }
        }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    let init_resp = read_json_line_with_id(&mut stdout, 1, deadline, &mut stdout_lines)
        .expect("initialize result on stdout");
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"],
        json!("codegraph"),
        "initialize must return the codegraph serverInfo"
    );

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
    let call_resp = read_json_line_with_id(&mut stdout, 2, deadline, &mut stdout_lines)
        .expect("tools/call response on stdout");
    assert_ne!(
        call_resp["result"]["isError"],
        json!(true),
        "tools/call must not error"
    );

    // Close stdin → EOF → serve loop ends → process exits; then drain stderr.
    drop(stdin);
    let _ = child.wait();
    let stderr = stderr_reader.join().expect("join stderr reader");

    // THEN every non-empty stdout line is valid JSON-RPC — no log bytes leaked.
    for line in &stdout_lines {
        let parsed: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC: {line:?} ({e})"));
        assert_eq!(
            parsed["jsonrpc"],
            json!("2.0"),
            "stdout line missing jsonrpc marker: {line:?}"
        );
    }
    assert!(
        !stdout_lines.is_empty(),
        "expected at least the initialize + tools/call responses on stdout"
    );

    // AND the migrated tracing debug traces landed on stderr (not stdout), with
    // an RFC3339 timestamp prefix — the timestamps the user's bare lines lacked.
    assert!(
        stderr.contains("logger initialized") || stderr.contains("serve startup"),
        "expected tracing events on stderr, got: {stderr:?}"
    );
    let has_timestamped_line = stderr.lines().any(has_rfc3339_prefix);
    assert!(
        has_timestamped_line,
        "expected at least one RFC3339-timestamped log line on stderr, got: {stderr:?}"
    );
}
