//! Phase C — CLI arg wiring for `codegraph serve --http`.
//!
//! Exercises the real `codegraph` binary (built with `--features rmcp`, gated
//! below) end-to-end:
//!   (1) `serve --http --path <indexed> --http-addr 127.0.0.1:PORT` starts and
//!       is reachable — an `initialize` POST returns a 200 JSON result;
//!   (2) `serve --http` with NO indexed project → a clean, non-zero hard error
//!       naming the missing index (does NOT hang, does NOT self-index);
//!   (3) `serve --mcp --http` together → a clean, non-zero error.
//!
//! Requires the `rmcp` feature (the shipped default build is rmcp-free); the
//! whole file is gated so the default `cargo test -p codegraph-rs` skips it.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
            "codegraph-http-{label}-{}-{}",
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

/// Pick a currently-free localhost port by binding an ephemeral socket and
/// immediately dropping it (there is a small TOCTOU window, acceptable in test).
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// A raw HTTP/1.1 POST of a JSON body to `/mcp` on `addr`, returning the raw
/// response bytes as a string (headers + body) — a curl-equivalent probe with
/// no reqwest dependency in this crate.
fn http_post_mcp(addr: &str, body: &str) -> std::io::Result<String> {
    let sockaddr = addr.to_socket_addrs()?.next().expect("resolve addr");
    let mut stream = TcpStream::connect_timeout(&sockaddr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    Ok(buf)
}

/// (1) `serve --http --path <indexed> --http-addr 127.0.0.1:PORT` starts and an
/// initialize POST returns a JSON result over HTTP.
#[test]
fn serve_http_indexed_starts_and_is_reachable() {
    let home = TestDir::new("indexed");
    let indexed = indexed_project(&home);
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let mut child = Command::new(bin())
        .args([
            "serve",
            "--http",
            "--path",
            indexed.to_str().unwrap(),
            "--http-addr",
            &addr,
        ])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --http");

    // Poll until the listener accepts a connection (server bound).
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reachable = false;
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    while Instant::now() < deadline {
        if let Ok(resp) = http_post_mcp(&addr, init)
            && resp.contains("\"result\"")
            && resp.contains("2024-11-05")
        {
            reachable = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        reachable,
        "serve --http must bind {addr} and answer an initialize POST with a JSON result"
    );
}

/// (2) `serve --http` with NO indexed project → a clean, non-zero hard error
/// (does not hang, exits promptly with an actionable message).
#[test]
fn serve_http_without_index_errors_cleanly() {
    let unindexed = TestDir::new("unindexed");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let output = Command::new(bin())
        .args(["serve", "--http", "--http-addr", &addr])
        .current_dir(unindexed.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run serve --http (no index)");

    assert!(
        !output.status.success(),
        "serve --http without an index must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("index") || stderr.to_lowercase().contains("codegraph init"),
        "error must name the missing index / suggest `codegraph init`: {stderr}"
    );
    // Must NOT have self-indexed the cwd.
    assert!(
        !unindexed.path().join(".codegraph").exists(),
        "serve --http must NOT self-index the cwd"
    );
}

/// (3) `serve --mcp --http` together → a clean, non-zero error (mutually
/// exclusive transports).
#[test]
fn serve_mcp_and_http_together_errors() {
    let home = TestDir::new("both");
    let indexed = indexed_project(&home);
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let output = Command::new(bin())
        .args([
            "serve",
            "--mcp",
            "--http",
            "--path",
            indexed.to_str().unwrap(),
            "--http-addr",
            &addr,
        ])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run serve --mcp --http");

    assert!(
        !output.status.success(),
        "serve --mcp --http together must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("--mcp") && stderr.to_lowercase().contains("--http"),
        "error must name the conflicting --mcp / --http flags: {stderr}"
    );
}
