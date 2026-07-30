//! Upstream `ce983a0` (#1314) — `codegraph node <symbol> -f <file>` must return
//! the symbol's SOURCE BODY, not just its location.
//!
//! `codegraph node setState` prints every overload's body. `-f <file>` is the
//! tool's own suggestion for picking ONE of them, so it must print that one's
//! body — the whole point of the disambiguation. These tests drive the REAL
//! binary against a temp project with two same-named definitions and assert on
//! the process's stdout.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-node-file-pin-{label}-{}-{}",
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

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn run_in(cwd: &Path, args: &[&str]) -> Run {
    let output = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

/// Two `setState` definitions, each carrying a marker only its own body holds.
fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("proj");
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("alpha.ts"),
        "export function setState(next: number): number {\n  \
         const ALPHA_MARKER = next + 1;\n  return ALPHA_MARKER;\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("beta.ts"),
        "export function setState(next: string): string {\n  \
         const BETA_MARKER = next + \"!\";\n  return BETA_MARKER;\n}\n",
    )
    .unwrap();
    let run = run_in(dir.path(), &["init", project.to_str().unwrap()]);
    assert!(run.ok, "init failed: {} {}", run.stdout, run.stderr);
    project
}

#[test]
fn node_symbol_pinned_to_a_file_prints_that_definitions_body_only() {
    let dir = TestDir::new("pin");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(
        project.as_path(),
        &["node", "setState", "-f", "src/beta.ts", "-p", p],
    );
    assert!(
        run.ok,
        "node <symbol> -f <file> must succeed: {} {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.contains("BETA_MARKER"),
        "the pinned definition's SOURCE BODY must be printed: {}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("ALPHA_MARKER"),
        "the other overload's body must NOT be printed: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("src/beta.ts"),
        "the pinned location must be named: {}",
        run.stdout
    );
}

/// A basename pins as well as a repo-relative path.
#[test]
fn node_symbol_pinned_by_basename_prints_that_definitions_body() {
    let dir = TestDir::new("basename");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(
        project.as_path(),
        &["node", "setState", "-f", "alpha.ts", "-p", p],
    );
    assert!(run.ok, "basename pin must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("ALPHA_MARKER"),
        "the basename-pinned body must be printed: {}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("BETA_MARKER"),
        "the other overload's body must NOT be printed: {}",
        run.stdout
    );
}

/// A `--file` that matches no definition of the symbol must say so rather than
/// silently falling back to an arbitrary overload.
#[test]
fn node_symbol_pinned_to_a_non_matching_file_reports_not_found() {
    let dir = TestDir::new("nomatch");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(
        project.as_path(),
        &["node", "setState", "-f", "src/nowhere.ts", "-p", p],
    );
    assert!(
        !run.stdout.contains("ALPHA_MARKER") && !run.stdout.contains("BETA_MARKER"),
        "a non-matching pin must not fall back to an arbitrary overload: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("not found") || run.stdout.contains("No "),
        "a non-matching pin must report not-found: {}",
        run.stdout
    );
}

/// The bare (unpinned) form is unchanged: every overload's body, as before.
#[test]
fn bare_node_symbol_still_returns_every_overload_body() {
    let dir = TestDir::new("bare");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(project.as_path(), &["node", "setState", "-p", p]);
    assert!(run.ok, "bare node <symbol> must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("ALPHA_MARKER") && run.stdout.contains("BETA_MARKER"),
        "the unpinned form must still return every overload in full: {}",
        run.stdout
    );
}

/// The MCP `codegraph_node` contract: `symbol` + `file` must pin to that file's
/// definition and return its body, over real stdio.
#[cfg(unix)]
#[test]
fn mcp_node_symbol_plus_file_pins_and_returns_the_body() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = TestDir::new("mcp");
    let project = indexed_project(&dir);

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", project.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);

    let send = |stdin: &mut std::process::ChildStdin, v: serde_json::Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = |want_id: i64| -> serde_json::Value {
        loop {
            assert!(Instant::now() < deadline, "timed out awaiting id {want_id}");
            let mut line = String::new();
            match stdout.read_line(&mut line) {
                Ok(0) => panic!("serve --mcp closed stdout before id {want_id}"),
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim())
                        && v.get("id").and_then(serde_json::Value::as_i64) == Some(want_id)
                    {
                        return v;
                    }
                }
                Err(e) => panic!("reading serve --mcp stdout: {e}"),
            }
        }
    };

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "node-file-pin-test", "version": "0" }
            }
        }),
    );
    let _ = recv(1);
    send(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {
                "name": "codegraph_node",
                "arguments": { "symbol": "setState", "file": "src/beta.ts", "includeCode": true }
            }
        }),
    );
    let resp = recv(2);
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    assert_ne!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "MCP node with symbol+file must not error: {text}"
    );
    assert!(
        text.contains("BETA_MARKER"),
        "MCP node symbol+file must return the pinned body: {text}"
    );
    assert!(
        !text.contains("ALPHA_MARKER"),
        "MCP node symbol+file must not return the other overload: {text}"
    );
}
