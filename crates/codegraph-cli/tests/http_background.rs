//! HTTP MCP background mode: `serve --http --detach`, the addr-keyed registry,
//! multi-instance conflict detection, and the `codegraph http {list,status,stop}`
//! subcommand group. Exercises the real `codegraph` binary end-to-end with an
//! ISOLATED registry dir (`CODEGRAPH_HTTP_REGISTRY_DIR`) so it never touches a
//! developer's real state.
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
            "codegraph-httpbg-{label}-{}-{}",
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

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

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

/// Run a `codegraph` command with the isolated registry dir set.
fn run_cli(reg_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", reg_dir)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run codegraph")
}

/// End-to-end: `serve --http --detach` starts a background server that
/// registers itself and actually serves; `http list` shows it; a SECOND
/// `--detach` on the SAME addr is a CONFLICT (non-zero, lists the running
/// instance) and does NOT start a second; `http stop` stops it and `list` goes
/// empty.
#[test]
fn detach_registers_serves_conflicts_and_stops() {
    let home = TestDir::new("detach");
    let reg = home.path().join("registry");
    std::fs::create_dir_all(&reg).unwrap();
    let indexed = indexed_project(&home);
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    // (1) Detached start: prints "started", registers, and EXITS.
    let out = run_cli(
        &reg,
        &[
            "serve",
            "--http",
            "--http-addr",
            &addr,
            "--path",
            indexed.to_str().unwrap(),
            "--detach",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "detached start must exit 0: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("started HTTP MCP server") && stdout.contains(&addr),
        "detached start must print 'started ...': {stdout}"
    );

    // The detached child actually serves.
    assert!(
        wait_reachable(&addr),
        "detached server on {addr} must answer an initialize POST"
    );

    // `http list` shows it.
    let list = run_cli(&reg, &["http", "list"]);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains(&addr),
        "http list must show the running server {addr}: {list_out}"
    );

    // (2) Same-addr conflict: non-zero, lists the running instance, no 2nd server.
    let conflict = run_cli(
        &reg,
        &[
            "serve",
            "--http",
            "--http-addr",
            &addr,
            "--path",
            indexed.to_str().unwrap(),
            "--detach",
        ],
    );
    let conflict_err = String::from_utf8_lossy(&conflict.stderr);
    assert!(
        !conflict.status.success(),
        "same-addr start must exit non-zero"
    );
    assert!(
        conflict_err.contains("already running on") && conflict_err.contains(&addr),
        "conflict must name the addr already running: {conflict_err}"
    );
    assert!(
        conflict_err.contains("running:"),
        "conflict must LIST the running instance details: {conflict_err}"
    );

    // (3) Stop it; list goes empty for that addr.
    let stop = run_cli(&reg, &["http", "stop", &addr]);
    let stop_out = String::from_utf8_lossy(&stop.stdout);
    assert!(
        stop.status.success() && stop_out.contains("stopped HTTP MCP server"),
        "stop must succeed and confirm: {stop_out}"
    );

    // Give the process a moment to die, then confirm list no longer shows it.
    std::thread::sleep(Duration::from_millis(500));
    let list2 = run_cli(&reg, &["http", "list"]);
    let list2_out = String::from_utf8_lossy(&list2.stdout);
    assert!(
        !list2_out.contains(&addr),
        "after stop, http list must NOT show {addr}: {list2_out}"
    );
}

/// Two DIFFERENT addrs coexist (multi-instance allowed) and the second start's
/// note lists the first running server.
#[test]
fn different_addrs_coexist_and_note_others() {
    let home = TestDir::new("multi");
    let reg = home.path().join("registry");
    std::fs::create_dir_all(&reg).unwrap();
    let indexed = indexed_project(&home);
    let port_a = free_port();
    let port_b = free_port();
    let addr_a = format!("127.0.0.1:{port_a}");
    let addr_b = format!("127.0.0.1:{port_b}");

    let out_a = run_cli(
        &reg,
        &[
            "serve",
            "--http",
            "--http-addr",
            &addr_a,
            "--path",
            indexed.to_str().unwrap(),
            "--detach",
        ],
    );
    assert!(out_a.status.success(), "first detached start must succeed");
    assert!(wait_reachable(&addr_a), "first server must serve");

    // Second detached start on a DIFFERENT addr: succeeds AND notes the other.
    let out_b = run_cli(
        &reg,
        &[
            "serve",
            "--http",
            "--http-addr",
            &addr_b,
            "--path",
            indexed.to_str().unwrap(),
            "--detach",
        ],
    );
    let out_b_err = String::from_utf8_lossy(&out_b.stderr);
    assert!(
        out_b.status.success(),
        "second detached start on a different addr must succeed"
    );
    assert!(
        out_b_err.contains("other HTTP MCP server(s) running") && out_b_err.contains(&addr_a),
        "second start must note the other running server {addr_a}: {out_b_err}"
    );

    // list shows BOTH.
    let list = run_cli(&reg, &["http", "list"]);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains(&addr_a) && list_out.contains(&addr_b),
        "http list must show BOTH servers: {list_out}"
    );

    // Cleanup: stop both.
    let _ = run_cli(&reg, &["http", "stop", &addr_a]);
    let _ = run_cli(&reg, &["http", "stop", &addr_b]);
}

/// `http stop` on an addr with no running server prints the friendly not-found
/// line and exits 0 (idempotent, not an error).
#[test]
fn stop_unknown_addr_is_friendly() {
    let home = TestDir::new("stop-unknown");
    let reg = home.path().join("registry");
    std::fs::create_dir_all(&reg).unwrap();

    let out = run_cli(&reg, &["http", "stop", "127.0.0.1:59999"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stop unknown addr must exit 0");
    assert!(
        stdout.contains("No HTTP MCP server running on"),
        "stop unknown addr must print the not-found line: {stdout}"
    );
}

/// `--detach` without `--http` is a hard error naming both flags.
#[test]
fn detach_without_http_errors() {
    let home = TestDir::new("detach-nohttp");
    let reg = home.path().join("registry");
    std::fs::create_dir_all(&reg).unwrap();

    let out = run_cli(&reg, &["serve", "--detach"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "--detach without --http must fail");
    assert!(
        stderr.contains("--detach") && stderr.contains("--http"),
        "error must name --detach and --http: {stderr}"
    );
}

/// `http list` with an empty registry prints the "none" line.
#[test]
fn list_empty_registry_says_none() {
    let home = TestDir::new("list-empty");
    let reg = home.path().join("registry");
    std::fs::create_dir_all(&reg).unwrap();

    let out = run_cli(&reg, &["http", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.contains("No HTTP MCP servers running."),
        "empty registry must say none: {stdout}"
    );
}
