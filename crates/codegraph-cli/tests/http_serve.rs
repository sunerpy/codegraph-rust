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
    indexed_project_named(dir, "mini")
}

fn indexed_project_named(dir: &TestDir, name: &str) -> PathBuf {
    let project = dir.path().join(name);
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

/// (1b) `serve --http --http-addr localhost:PORT` starts and is reachable —
/// proving a hostname (not just an IP literal) is accepted by the address parse
/// and binds a loopback the client can reach.
#[test]
fn serve_http_localhost_hostname_starts_and_is_reachable() {
    let home = TestDir::new("localhost");
    let indexed = indexed_project(&home);
    let port = free_port();
    let bind = format!("localhost:{port}");
    let probe = format!("127.0.0.1:{port}");

    let mut child = Command::new(bin())
        .args([
            "serve",
            "--http",
            "--path",
            indexed.to_str().unwrap(),
            "--http-addr",
            &bind,
        ])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --http localhost");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reachable = false;
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    while Instant::now() < deadline {
        if let Ok(resp) = http_post_mcp(&probe, init)
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
        "serve --http --http-addr {bind} must resolve the hostname, bind a loopback, and answer an initialize POST"
    );
}

/// (2) PINNED mode: `serve --http --path <unindexed>` → a clean, non-zero hard
/// error (does not hang, exits promptly with an actionable message). This is the
/// require-index guarantee for the pinned path — it survives the global-mode
/// addition below (which only relaxes the *no-`--path*` case).
#[test]
fn serve_http_pinned_without_index_errors_cleanly() {
    let unindexed = TestDir::new("unindexed");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let output = Command::new(bin())
        .args([
            "serve",
            "--http",
            "--path",
            unindexed.path().to_str().unwrap(),
            "--http-addr",
            &addr,
        ])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run serve --http --path (no index)");

    assert!(
        !output.status.success(),
        "serve --http --path <unindexed> must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("index") || stderr.to_lowercase().contains("codegraph init"),
        "error must name the missing index / suggest `codegraph init`: {stderr}"
    );
    // Must NOT have self-indexed the pinned dir.
    assert!(
        !unindexed.path().join(".codegraph").exists(),
        "serve --http --path must NOT self-index the target"
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

fn spawn_global_server(addr: &str) -> std::process::Child {
    Command::new(bin())
        .args(["serve", "--http", "--http-addr", addr])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn global serve --http")
}

fn wait_reachable(addr: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    while Instant::now() < deadline {
        if let Ok(resp) = http_post_mcp(addr, init)
            && resp.contains("\"result\"")
            && resp.contains("2024-11-05")
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn call_search(addr: &str, query: &str, project_path: Option<&str>) -> String {
    let args = match project_path {
        Some(p) => format!(r#"{{"query":"{query}","projectPath":"{p}"}}"#),
        None => format!(r#"{{"query":"{query}"}}"#),
    };
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"codegraph_search","arguments":{args}}}}}"#
    );
    http_post_mcp(addr, &body).expect("tools/call codegraph_search")
}

/// (4) GLOBAL mode: `serve --http` with NO `--path` starts (does NOT error on a
/// missing index) and serves MANY projects from ONE server — a `codegraph_search`
/// carrying `projectPath=<projA>` returns projA's results, and a second call on
/// the SAME server with `projectPath=<projB>` (a distinct indexed fixture)
/// returns projB's results.
#[test]
fn serve_http_global_serves_multiple_projectpaths() {
    let home = TestDir::new("global");
    let proj_a = indexed_project_named(&home, "alpha");
    let proj_b = indexed_project_named(&home, "beta");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let mut child = spawn_global_server(&addr);
    let reachable = wait_reachable(&addr);

    let resp_a = call_search(&addr, "Greeter", Some(proj_a.to_str().unwrap()));
    let resp_b = call_search(&addr, "Counter", Some(proj_b.to_str().unwrap()));

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        reachable,
        "global serve --http (no --path) must start and answer initialize"
    );
    assert!(
        resp_a.contains("Greeter"),
        "global mode: projectPath={} must return that project's Greeter results: {resp_a}",
        proj_a.display()
    );
    assert!(
        resp_b.contains("Counter"),
        "global mode: a SECOND call with projectPath={} must return that project's Counter results from the SAME server: {resp_b}",
        proj_b.display()
    );
}

/// (5) GLOBAL mode, tool call WITHOUT `projectPath` → the actionable
/// "No indexed project resolved" error (a normal isError result, not a crash).
#[test]
fn serve_http_global_without_projectpath_returns_actionable_error() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let mut child = spawn_global_server(&addr);
    let reachable = wait_reachable(&addr);
    let resp = call_search(&addr, "Greeter", None);

    let _ = child.kill();
    let _ = child.wait();

    assert!(reachable, "global serve --http must start");
    assert!(
        resp.contains("No indexed project resolved"),
        "global mode without projectPath must return the actionable resolve error: {resp}"
    );
}

/// Spawn `serve --http --path <indexed>` with `CODEGRAPH_DEBUG` set to `debug`
/// and stderr redirected to `err_path` (a file, so the still-running child does
/// not block a piped read). Returns the child handle.
fn spawn_pinned_server_stderr_to(
    indexed: &Path,
    addr: &str,
    debug: Option<&str>,
    err_path: &Path,
) -> std::process::Child {
    let err_file = std::fs::File::create(err_path).expect("create stderr capture file");
    let mut cmd = Command::new(bin());
    cmd.args([
        "serve",
        "--http",
        "--path",
        indexed.to_str().unwrap(),
        "--http-addr",
        addr,
    ])
    .env("CODEGRAPH_NO_DAEMON", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::from(err_file));
    match debug {
        Some(v) => {
            cmd.env("CODEGRAPH_DEBUG", v);
        }
        None => {
            cmd.env_remove("CODEGRAPH_DEBUG");
        }
    }
    cmd.spawn().expect("spawn serve --http (stderr capture)")
}

fn poll_stderr_for(err_path: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::read_to_string(err_path)
            .map(|s| s.contains(needle))
            .unwrap_or(false)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// (6) DEBUG ON: with `CODEGRAPH_DEBUG=1` (which bumps the base log level to
/// debug when RUST_LOG is unset), each POST to `/mcp` logs a per-request
/// `http request` event (method=POST path=/mcp ...) on STDERR, and a
/// `tools/call` logs a handler line naming the tool + resolved projectPath.
/// STDOUT stays pure protocol (the JSON-RPC responses come back over the
/// socket, not stdout — asserted by (1)).
#[test]
fn serve_http_debug_on_logs_per_request_lines_to_stderr() {
    let home = TestDir::new("dbg-on");
    let indexed = indexed_project(&home);
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let err_path = home.path().join("dbg-on.err");

    let mut child = spawn_pinned_server_stderr_to(&indexed, &addr, Some("1"), &err_path);
    let reachable = wait_reachable(&addr);
    // Drive a tools/call so the handler debug line also fires.
    let _ = call_search(&addr, "McpServer", None);
    // Poll the redirected stderr for BOTH the middleware event and the handler
    // tool line BEFORE killing — a hard kill can truncate buffered stderr, and
    // re-reading the file after the kill can race with that truncation, so we
    // capture the settled contents from the poll itself.
    let saw_request = poll_stderr_for(&err_path, "http request", Duration::from_secs(5));
    let saw_tool = poll_stderr_for(&err_path, "tool=codegraph_search", Duration::from_secs(5));
    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();

    let _ = child.kill();
    let _ = child.wait();

    assert!(reachable, "serve --http (debug on) must start");
    assert!(
        saw_request && stderr.contains("/mcp"),
        "debug-on stderr must contain the per-request middleware event: {stderr}"
    );
    assert!(
        saw_tool,
        "debug-on stderr must contain the handler tool line naming the tool: {stderr}"
    );
}

/// (7) DEBUG OFF: with `CODEGRAPH_DEBUG` unset (and no RUST_LOG), the base level
/// stays at the config default (info), so NO per-request debug events appear on
/// STDERR. Guards the "off by default, zero new output" contract.
#[test]
fn serve_http_debug_off_emits_no_per_request_lines() {
    let home = TestDir::new("dbg-off");
    let indexed = indexed_project(&home);
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let err_path = home.path().join("dbg-off.err");

    let mut child = spawn_pinned_server_stderr_to(&indexed, &addr, None, &err_path);
    let reachable = wait_reachable(&addr);
    let _ = call_search(&addr, "McpServer", None);
    std::thread::sleep(Duration::from_millis(300));

    let _ = child.kill();
    let _ = child.wait();

    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();

    assert!(reachable, "serve --http (debug off) must start");
    assert!(
        !stderr.contains("http request"),
        "debug-off stderr must NOT contain any per-request middleware event: {stderr}"
    );
    assert!(
        !stderr.contains("tool=codegraph_search"),
        "debug-off stderr must NOT contain any handler tool line: {stderr}"
    );
}
