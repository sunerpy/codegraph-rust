//! `codegraph mcp list` — the READ side of the PID-keyed stdio MCP registry.
//!
//! Drives the real `codegraph` binary against an ISOLATED registry dir
//! (`CODEGRAPH_MCP_REGISTRY_DIR`) so it never touches a developer's real state.
//! Four cases cover the command itself: two live `serve --mcp` processes are
//! BOTH listed; an empty registry prints a friendly empty state and exits 0; an
//! UNREADABLE registry prints a visibly different diagnostic and still exits 0;
//! and `--json` emits a stable, parseable shape.
//!
//! A fifth case covers the fourth stdio exit of `cmd_serve`: the too-broad-root
//! ("home guard") early return also serves tools off any existing index in the
//! foreground, so it must register itself too — otherwise `mcp list` silently
//! misses exactly the Kiro-style `CWD=$HOME` launches this command exists to
//! surface.
//!
//! By decision A of the rev55 plan there is no `mcp stop`: entries are
//! PID-keyed, and a stale entry whose PID was reused would let us terminate an
//! innocent process. `list` therefore only PRINTS platform-appropriate stop
//! guidance, which the first case asserts.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
            "codegraph-mcplist-{label}-{}-{}",
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

/// Copy the mini fixture to `<dir>/<name>` and index it. Each live server in the
/// multi-instance case gets its OWN project so their background catch-up threads
/// never contend on one database.
fn indexed_project(dir: &TestDir, name: &str) -> PathBuf {
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

/// Run a `codegraph` command with the isolated MCP registry dir set.
fn run_cli(registry_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .env("CODEGRAPH_MCP_REGISTRY_DIR", registry_dir)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run codegraph")
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

/// Poll for process exit within `timeout`, returning whether it exited (killing
/// it on timeout so the test process never leaks a child).
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
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

/// A spawned `serve --mcp` whose `initialize` round-trip has already completed —
/// the barrier proving startup (and therefore registration) has landed. No
/// sleeps, no polling on the filesystem.
struct LiveServer {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
}

impl LiveServer {
    fn spawn(project: &Path, registry_dir: &Path) -> Self {
        let child = Command::new(bin())
            .args(["serve", "--mcp", "--path", project.to_str().unwrap()])
            .env("CODEGRAPH_NO_DAEMON", "1")
            .env("CODEGRAPH_NO_WATCH", "1")
            .env("CODEGRAPH_MCP_REGISTRY_DIR", registry_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve --mcp");
        Self::handshake(child)
    }

    fn handshake(mut child: Child) -> Self {
        let mut stdin = child.stdin.take().expect("child stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "mcp-list-test", "version": "0" }
            }
        });
        writeln!(stdin, "{init}").unwrap();
        stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let response =
            read_json_line_with_id(&mut stdout, 1, deadline).expect("initialize must be answered");
        assert_eq!(response["result"]["serverInfo"]["name"], json!("codegraph"));
        Self {
            child,
            stdin: Some(stdin),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Close stdin (EOF → serve ends → process exits) and confirm it exited.
    fn shutdown(mut self) -> bool {
        self.stdin.take();
        wait_with_timeout(&mut self.child, Duration::from_secs(10))
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// PIDs read out of the text table's FIRST column (exact tokens, so a pid never
/// matches by accident inside a longer number).
fn table_pids(stdout: &str) -> Vec<u32> {
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|token| token.parse::<u32>().ok())
        .collect()
}

fn seed_entry(registry_dir: &Path, pid: u32, project: &str) {
    std::fs::create_dir_all(registry_dir).unwrap();
    let entry = json!({
        "pid": pid,
        "project": project,
        "transport": "stdio",
        "startedAt": 1_700_000_000_123u64,
        "version": "1.2.3-test",
    });
    std::fs::write(
        registry_dir.join(format!("{pid}.json")),
        format!("{entry}\n"),
    )
    .unwrap();
}

/// (a) TWO live `serve --mcp` processes are BOTH listed, with the platform's
/// stop command printed as guidance (we never stop them ourselves).
#[test]
fn list_shows_every_live_stdio_server() {
    let home = TestDir::new("two-live");
    let registry_dir = home.path().join("mcp-registry");
    let project_a = indexed_project(&home, "mini-a");
    let project_b = indexed_project(&home, "mini-b");

    let server_a = LiveServer::spawn(&project_a, &registry_dir);
    let server_b = LiveServer::spawn(&project_b, &registry_dir);
    let (pid_a, pid_b) = (server_a.pid(), server_b.pid());

    let out = run_cli(&registry_dir, &["mcp", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "mcp list must exit 0: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pids = table_pids(&stdout);
    assert!(
        pids.contains(&pid_a) && pids.contains(&pid_b),
        "mcp list must show BOTH live servers ({pid_a}, {pid_b}); saw {pids:?}: {stdout}"
    );
    assert!(
        stdout.contains("mini-a") && stdout.contains("mini-b"),
        "each row must name the project it serves: {stdout}"
    );
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "each row must carry the binary version: {stdout}"
    );
    // Decision A: guidance only, never a terminate path.
    assert!(
        stdout.contains("kill"),
        "list must print the platform's stop command as GUIDANCE: {stdout}"
    );

    assert!(server_a.shutdown(), "server a must exit on stdin EOF");
    assert!(server_b.shutdown(), "server b must exit on stdin EOF");
}

/// (b) An EMPTY (but readable) registry is the normal pre-first-serve state: a
/// friendly empty line, exit 0, and none of the outage wording.
#[test]
fn list_empty_registry_is_friendly_and_exits_zero() {
    let home = TestDir::new("empty");
    let registry_dir = home.path().join("mcp-registry");
    std::fs::create_dir_all(&registry_dir).unwrap();

    let out = run_cli(&registry_dir, &["mcp", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "mcp list on an empty registry must exit 0: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("No stdio MCP servers"),
        "an empty registry must print a friendly empty state: {stdout:?}"
    );
    assert!(
        !stdout.contains("registry unavailable"),
        "an EMPTY registry must not be reported as an outage: {stdout}"
    );
}

/// (c) An UNREADABLE registry (a file where the directory should be) is a
/// distinguishable diagnostic — and still exits 0, because failing hard while
/// the user is already debugging would be hostile.
#[test]
fn list_unavailable_registry_is_distinguishable_and_exits_zero() {
    let home = TestDir::new("unavailable");
    let registry_dir = home.path().join("mcp-registry");
    std::fs::write(&registry_dir, b"not a directory").unwrap();

    let out = run_cli(&registry_dir, &["mcp", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "mcp list is a diagnostic: an unreadable registry must still exit 0: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("registry unavailable") && stdout.contains(registry_dir.to_str().unwrap()),
        "an unreadable registry must be reported with its path: {stdout:?}"
    );
    assert!(
        !stdout.contains("No stdio MCP servers"),
        "an outage must NOT read like an empty registry: {stdout}"
    );
}

/// (d) `--json` is a stable, parseable shape: always a `servers` array of the
/// registry's own camelCase fields, plus a `registryUnavailable` object ONLY
/// when the registry could not be read.
#[test]
fn list_json_shape_is_stable_and_parseable() {
    let home = TestDir::new("json");
    let registry_dir = home.path().join("mcp-registry");
    // The test process's own pid is guaranteed alive, so the seeded entry
    // survives the read-time prune.
    let pid = std::process::id();
    seed_entry(&registry_dir, pid, "/work/project");

    let out = run_cli(&registry_dir, &["mcp", "list", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "mcp list --json must exit 0: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value =
        serde_json::from_str(&stdout).expect("mcp list --json emits valid JSON on stdout");
    let servers = value["servers"]
        .as_array()
        .unwrap_or_else(|| panic!("`servers` must always be an array: {stdout}"));
    assert_eq!(servers.len(), 1, "one live entry was seeded: {stdout}");
    assert_eq!(servers[0]["pid"], json!(pid), "{stdout}");
    assert_eq!(servers[0]["project"], json!("/work/project"), "{stdout}");
    assert_eq!(servers[0]["transport"], json!("stdio"), "{stdout}");
    assert_eq!(
        servers[0]["startedAt"],
        json!(1_700_000_000_123u64),
        "startedAt stays camelCase epoch millis: {stdout}"
    );
    assert_eq!(servers[0]["version"], json!("1.2.3-test"), "{stdout}");
    assert!(
        value.get("registryUnavailable").is_none(),
        "a readable registry must not carry the outage key: {stdout}"
    );

    // The SAME command on an unreadable registry keeps `servers` an array and
    // adds the outage key, so a consumer never has to guess the shape.
    let broken = TestDir::new("json-broken");
    let broken_dir = broken.path().join("mcp-registry");
    std::fs::write(&broken_dir, b"not a directory").unwrap();
    let out = run_cli(&broken_dir, &["mcp", "list", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "unreadable registry still exits 0");
    let value: Value =
        serde_json::from_str(&stdout).expect("the outage branch is JSON too: {stdout}");
    assert_eq!(
        value["servers"],
        json!([]),
        "`servers` stays an array in the outage branch: {stdout}"
    );
    assert_eq!(
        value["registryUnavailable"]["path"],
        json!(broken_dir.to_str().unwrap()),
        "{stdout}"
    );
    assert!(
        value["registryUnavailable"]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "the outage must carry a non-empty error: {stdout}"
    );
}

/// (e) The FOURTH stdio exit — the too-broad-root ("home guard") early return —
/// is a foreground process serving tools off any existing index, so it must
/// register itself just like `Direct` does. Without this, `mcp list` misses the
/// Kiro-style `CWD=$HOME` launches it exists to surface.
#[test]
fn home_guard_serve_registers_itself() {
    let home = TestDir::new("home-guard");
    let registry_dir = home.path().join("mcp-registry");
    // `too_broad_root_reason` fires on an EXACT $HOME match, so point HOME at
    // the temp dir and serve that same dir.
    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", home.path().to_str().unwrap()])
        .env("HOME", home.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve --mcp at $HOME");

    let pid = child.id();
    let entry_path = registry_dir.join(format!("{pid}.json"));
    let mut stderr = child.stderr.take().expect("child stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "mcp-home-guard-test", "version": "0" }
        }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let response = read_json_line_with_id(&mut stdout, 1, deadline)
        .expect("the home-guard path must still answer initialize");
    assert_eq!(response["result"]["serverInfo"]["name"], json!("codegraph"));

    // THEN it registered itself, exactly like the Direct arm does.
    let raw = std::fs::read_to_string(&entry_path).unwrap_or_else(|error| {
        panic!(
            "the too-broad-root serve path must register {}: {error}",
            entry_path.display()
        )
    });
    let entry: Value = serde_json::from_str(&raw).expect("registry entry is valid JSON");
    assert_eq!(entry["pid"], json!(pid), "{raw}");
    assert_eq!(entry["transport"], json!("stdio"), "{raw}");

    drop(stdin);
    assert!(
        wait_with_timeout(&mut child, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
    assert!(
        !entry_path.exists(),
        "the entry must be removed on graceful shutdown: {}",
        entry_path.display()
    );

    // Proof the HOME-guard branch (not the Direct arm) served this session.
    let logs = stderr_reader.join().expect("stderr reader thread");
    assert!(
        logs.contains("home directory"),
        "this session must have taken the too-broad-root branch: {logs}"
    );
}
