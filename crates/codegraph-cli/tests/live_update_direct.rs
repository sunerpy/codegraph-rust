//! T3 — DIRECT-mode live auto-sync E2E.
//!
//! Drives `codegraph serve --mcp` (forced DIRECT via `CODEGRAPH_NO_DAEMON=1`)
//! over its stdin/stdout JSON-RPC pipes, writes a NEW source file with a
//! uniquely-named symbol, and asserts the in-process watcher re-indexes it so a
//! subsequent `codegraph_search` finds the new symbol. The `--no-watch` variant
//! asserts the symbol is NOT found (auto-sync disabled).
//!
//! De-flaking: the watcher debounce is pinned to 100ms via
//! `CODEGRAPH_WATCH_DEBOUNCE_MS=100`, and instead of a single fixed sleep the
//! search is POLLED (re-issued) up to ~6s with a fresh JSON-RPC id each round,
//! so a slow CI host just takes a couple extra poll rounds rather than failing.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

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

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-live-{label}-{}-{}",
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

fn cli(args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// A live `serve --mcp` child whose stdin/stdout we drive with line-delimited
/// JSON-RPC. Killed + reaped on drop so no orphan serve process leaks.
///
/// The `BufReader` over stdout is created ONCE and held for the process
/// lifetime — re-creating it per read would discard any buffered bytes that
/// arrived after the line we consumed, which silently drops responses.
struct ServeProcess {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl ServeProcess {
    fn spawn(project: &Path, no_watch: bool) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_codegraph"));
        cmd.arg("serve")
            .arg("--mcp")
            .arg("--path")
            .arg(project)
            // Force DIRECT mode (no detached daemon, no proxy).
            .env("CODEGRAPH_NO_DAEMON", "1")
            // Pin a short debounce so the watcher reacts quickly + deterministically.
            .env("CODEGRAPH_WATCH_DEBOUNCE_MS", "100")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if no_watch {
            cmd.arg("--no-watch");
        }
        let mut child = cmd.spawn().expect("spawn serve --mcp");
        let stdin = child.stdin.take().expect("serve stdin");
        let stdout = child.stdout.take().expect("serve stdout");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
        }
    }

    fn send(&mut self, line: &str) {
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.write_all(b"\n").expect("write newline");
        self.stdin.flush().expect("flush request");
    }

    /// Read one JSON-RPC response line. Blocks until a line is available or
    /// stdout closes (the caller's poll budget bounds total wall time).
    fn read_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        match self.reader.read_line(&mut buf) {
            Ok(0) => None,
            Ok(_) => Some(buf),
            Err(_) => None,
        }
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Issue a `codegraph_search` for `symbol` and return whether it was FOUND.
///
/// The not-found response echoes the query (`No results found for "<symbol>"`),
/// so a naive substring check on the symbol is always true; we instead key on
/// the explicit not-found sentinel to decide presence.
fn search_finds(serve: &mut ServeProcess, id: u64, symbol: &str) -> bool {
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"codegraph_search","arguments":{{"query":"{symbol}"}}}}}}"#
    );
    serve.send(&req);
    match serve.read_line() {
        Some(line) => line.contains(symbol) && !line.contains("No results found"),
        None => false,
    }
}

#[test]
fn direct_mode_auto_syncs_new_file() {
    let dir = TestDir::new("autosync");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    let mut serve = ServeProcess::spawn(&project, false);

    // Handshake.
    serve.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e-test","version":"0"}}}"#);
    let init = serve.read_line().expect("initialize response");
    assert!(
        init.contains("serverInfo") || init.contains("protocolVersion") || init.contains("result"),
        "unexpected initialize response: {init}"
    );

    // Sanity: the unique symbol does NOT exist yet.
    assert!(
        !search_finds(&mut serve, 2, "zzz_live_marker"),
        "symbol must not exist before the file is written"
    );

    // Write a NEW source file with a uniquely-named symbol.
    fs::write(
        project.join("src/live_marker.rs"),
        "pub fn zzz_live_marker() {}\n",
    )
    .unwrap();

    // Poll the search up to ~6s (debounce 100ms + reindex + settle), fresh id
    // each round, instead of a single fixed sleep — de-flakes on slow CI.
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut id = 3u64;
    let mut found = false;
    // Initial wait covers debounce + first reindex pass.
    std::thread::sleep(Duration::from_millis(1100));
    while Instant::now() < deadline {
        if search_finds(&mut serve, id, "zzz_live_marker") {
            found = true;
            break;
        }
        id += 1;
        std::thread::sleep(Duration::from_millis(250));
    }

    assert!(
        found,
        "direct-mode watcher must auto-sync the new file so zzz_live_marker becomes searchable"
    );
    // serve killed + temp dir removed on drop.
}

#[test]
fn no_watch_disables_auto_sync() {
    let dir = TestDir::new("nowatch");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    let mut serve = ServeProcess::spawn(&project, true);

    serve.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e-test","version":"0"}}}"#);
    let _ = serve.read_line().expect("initialize response");

    assert!(
        !search_finds(&mut serve, 2, "zzz_nowatch_marker"),
        "symbol must not exist before the file is written"
    );

    fs::write(
        project.join("src/nowatch_marker.rs"),
        "pub fn zzz_nowatch_marker() {}\n",
    )
    .unwrap();

    // Give the (disabled) watcher the SAME generous window the happy test uses;
    // with --no-watch nothing should pick the file up.
    std::thread::sleep(Duration::from_millis(1100));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut id = 3u64;
    let mut found = false;
    while Instant::now() < deadline {
        if search_finds(&mut serve, id, "zzz_nowatch_marker") {
            found = true;
            break;
        }
        id += 1;
        std::thread::sleep(Duration::from_millis(250));
    }

    assert!(
        !found,
        "--no-watch must disable auto-sync: the new symbol must NOT be indexed"
    );
}
