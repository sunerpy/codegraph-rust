//! Regression test for the Zed bare-serve roots-adoption race.
//!
//! A bare `codegraph serve --mcp` (NO `--path`) launched from an UNINDEXED cwd
//! must NOT self-index that cwd (the old `spawn_catch_up` -> `Store::open` race
//! created `.codegraph/` there, defeating roots adoption). It must instead
//! request `roots/list`, adopt the INDEXED workspace root the client reports,
//! and resolve a `tools/call` (with no `projectPath`) against it — NON-EMPTY.
//!
//! Asserts, end-to-end against the real binary:
//!   (a) the server proactively requests `roots/list` after initialize,
//!   (b) `codegraph_search` (no projectPath) returns a NON-EMPTY, non-error
//!       result resolved against the adopted root,
//!   (c) the unindexed cwd still has NO `.codegraph/` dir afterward.

#![cfg(unix)]

use std::fs;
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
            "codegraph-zed-bare-{label}-{}-{}",
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

fn read_one_json_line(reader: &mut impl BufRead, deadline: Instant) -> Option<Value> {
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
                return serde_json::from_str(trimmed).ok();
            }
            Err(_) => return None,
        }
    }
}

#[test]
fn zed_bare_serve_adopts_roots_and_does_not_self_index_cwd() {
    // GIVEN a bare `serve --mcp` (no --path) from an UNINDEXED cwd, plus a
    // separate INDEXED project the client will report via roots/list.
    let unindexed = TestDir::new("unindexed-cwd");
    let indexed_home = TestDir::new("indexed-home");
    let indexed = indexed_project(&indexed_home);

    // Force foreground direct mode so we exercise serve_direct (the fixed path)
    // rather than spawning a daemon; disable the live watcher for hermeticity.
    let mut child = Command::new(bin())
        .args(["serve", "--mcp"])
        .current_dir(unindexed.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(20);

    // WHEN the client advertises capabilities.roots on initialize.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "capabilities": { "roots": { "listChanged": true } } }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();

    // THEN two frames come back: the initialize result and a roots/list request.
    let first = read_one_json_line(&mut stdout, deadline).expect("initialize result");
    assert_eq!(
        first["id"],
        json!(1),
        "first frame is the initialize result"
    );
    let roots_req = read_one_json_line(&mut stdout, deadline).expect("roots/list request");
    assert_eq!(
        roots_req["method"],
        json!("roots/list"),
        "server must proactively request roots/list for an unindexed cwd default"
    );

    // The client replies with the INDEXED workspace root.
    let roots_reply = json!({
        "jsonrpc": "2.0",
        "id": roots_req["id"],
        "result": { "roots": [
            { "uri": format!("file://{}", indexed.display()), "name": "proj" }
        ] }
    });
    writeln!(stdin, "{roots_reply}").unwrap();
    stdin.flush().unwrap();

    // codegraph_search WITHOUT projectPath must resolve against the adopted root.
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "codegraph_search", "arguments": { "query": "add" } }
    });
    writeln!(stdin, "{call}").unwrap();
    stdin.flush().unwrap();

    let call_resp = loop {
        let frame = read_one_json_line(&mut stdout, deadline).expect("tools/call response");
        if frame["id"] == json!(2) {
            break frame;
        }
    };

    // Close stdin so the server loop sees EOF and exits.
    drop(stdin);
    let _ = child.wait();

    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert_ne!(
        call_resp["result"]["isError"],
        json!(true),
        "adopted-root tool call must not error: {text}"
    );
    assert!(
        !text.contains("No indexed project resolved"),
        "must not fall through to the no-project error: {text}"
    );
    assert!(
        text.contains("add"),
        "search against the adopted indexed root must return results: {text}"
    );

    // The core regression guard: the unindexed cwd was NEVER self-indexed.
    assert!(
        !unindexed.path().join(".codegraph").exists(),
        "bare serve from an unindexed cwd must NOT create .codegraph/ in it"
    );
}
