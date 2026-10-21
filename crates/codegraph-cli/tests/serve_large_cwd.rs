//! A stdio server launched in a large unindexed workspace answers promptly.
//!
//! With no index at or above the launch directory and a `.git` there, `serve
//! --mcp` searches downward for indexed sub-projects. That search is bounded
//! by a counted entry budget and a deadline, and the server's own constructor
//! no longer runs it before `initialize`, so a directory like `/tmp` with tens
//! of thousands of entries cannot keep the handshake waiting.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const ANSWER_WITHIN: Duration = Duration::from_secs(2);

struct TestDir(PathBuf);

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `.git` plus 10,000 directories, two levels deep, none of them indexed.
fn large_workspace() -> TestDir {
    let root = std::env::temp_dir().join(format!(
        "codegraph-serve-large-cwd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join(".git")).unwrap();
    for outer in 0..100 {
        for inner in 0..100 {
            fs::create_dir_all(root.join(format!("pkg{outer:03}/mod{inner:03}"))).unwrap();
        }
    }
    TestDir(root)
}

fn read_id(reader: &mut impl BufRead, want: i64) -> Value {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read serve stdout");
        assert!(read > 0, "serve closed stdout before response {want}");
        let value: Value = serde_json::from_str(line.trim()).expect("JSON-RPC on stdout");
        if value.get("id").and_then(Value::as_i64) == Some(want) {
            return value;
        }
    }
}

fn answered_within(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    id: i64,
    request: &Value,
) -> (Value, Duration) {
    writeln!(stdin, "{request}").unwrap();
    stdin.flush().unwrap();
    let started = Instant::now();
    let response = read_id(stdout, id);
    (response, started.elapsed())
}

#[test]
fn initialize_and_tools_list_answer_promptly_in_a_large_unindexed_workspace() {
    let workspace = large_workspace();
    let registry = workspace.0.with_extension("registry");
    let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(["serve", "--mcp"])
        .current_dir(&workspace.0)
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry)
        .env_remove("CODEGRAPH_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let launched = Instant::now();

    let (init, init_latency) = answered_within(
        &mut stdin,
        &mut stdout,
        1,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "serve-large-cwd", "version": "0" }
            }
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "codegraph", "{init}");
    assert!(
        launched.elapsed() < ANSWER_WITHIN,
        "initialize took {:?} from launch (request to answer {init_latency:?})",
        launched.elapsed()
    );
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .unwrap();

    let (list, list_latency) = answered_within(
        &mut stdin,
        &mut stdout,
        2,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    assert!(list["result"]["tools"].is_array(), "{list}");
    assert!(
        list_latency < ANSWER_WITHIN,
        "tools/list, which runs the bounded downward scan, took {list_latency:?}"
    );

    drop(stdin);
    let _ = child.wait();
    let _ = fs::remove_dir_all(Path::new(&registry));
}
