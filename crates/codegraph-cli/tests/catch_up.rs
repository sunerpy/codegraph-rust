//! T5 — catch-up sync on serve open, non-blocking first call (#905).
//!
//! Models the offline-edit scenario: a project is indexed, the server is DOWN,
//! a file is edited while it is down, THEN `codegraph serve --mcp` starts. The
//! live file watcher only sees events that happen AFTER it starts, so it can
//! NEVER absorb an edit made while the server was off — only a background
//! catch-up `sync_project_once` on serve open does. This test therefore proves
//! the catch-up path exists and is non-blocking:
//!
//!   (a) the FIRST `tools/call` returns within a SMALL bound (< 2s), proving the
//!       handshake / first call did NOT block on a full reconcile (#905);
//!   (b) polling `codegraph_search` for the offline-only symbol up to ~6s finds
//!       it, proving the background catch-up absorbed the offline edit.
//!
//! Forced DIRECT mode via `CODEGRAPH_NO_DAEMON=1`; watcher debounce pinned so a
//! watcher event (if any) cannot be confused with catch-up. The edit is appended
//! to an already-tracked file, so the content-hash gate in `sync_project_once`
//! reindexes it deterministically.

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
            "codegraph-cli-catchup-{label}-{}-{}",
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

/// A live `serve --mcp` child driven over line-delimited JSON-RPC. Killed +
/// reaped on drop so no orphan serve process leaks. The `BufReader` is created
/// ONCE and held for the process lifetime (re-creating it per read would drop
/// buffered bytes).
struct ServeProcess {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl ServeProcess {
    fn spawn(project: &Path) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_codegraph"));
        cmd.arg("serve")
            .arg("--mcp")
            .arg("--path")
            .arg(project)
            // Force DIRECT mode (no detached daemon, no proxy).
            .env("CODEGRAPH_NO_DAEMON", "1")
            // Pin a short debounce so a watcher event would be fast IF it fired;
            // the offline edit predates the watcher, so only catch-up absorbs it.
            .env("CODEGRAPH_WATCH_DEBOUNCE_MS", "100")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
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
/// The not-found response echoes the query, so a naive substring check is always
/// true; key on the explicit not-found sentinel to decide presence.
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
fn catch_up_absorbs_offline_edit() {
    let dir = TestDir::new("offline");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    // Index the project (init runs the first index) while the server is DOWN.
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    // OFFLINE edit: append a uniquely-named symbol to an already-tracked file
    // while NO server is running. The live watcher will never see this event;
    // only background catch-up on serve open can absorb it.
    let marker_file = project.join("src/math.ts");
    let existing = fs::read_to_string(&marker_file).expect("read tracked fixture file");
    fs::write(
        &marker_file,
        format!("{existing}\nexport function zzz_offline_marker() {{}}\n"),
    )
    .unwrap();

    // NOW start the server.
    let mut serve = ServeProcess::spawn(&project);

    // Handshake.
    serve.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    let init = serve.read_line().expect("initialize response");
    assert!(
        init.contains("serverInfo") || init.contains("protocolVersion") || init.contains("result"),
        "unexpected initialize response: {init}"
    );

    // (a) The FIRST tools/call must return promptly — proving catch-up did NOT
    // block the request path (#905).
    let first_call_started = Instant::now();
    let _ = search_finds(&mut serve, 2, "zzz_offline_marker");
    let first_call_latency = first_call_started.elapsed();
    assert!(
        first_call_latency < Duration::from_secs(2),
        "first tools/call must not block on catch-up reconcile (#905); took {first_call_latency:?}"
    );

    // (b) Within a short window, the offline edit becomes searchable via the
    // background catch-up sync.
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut id = 3u64;
    let mut found = false;
    while Instant::now() < deadline {
        if search_finds(&mut serve, id, "zzz_offline_marker") {
            found = true;
            break;
        }
        id += 1;
        std::thread::sleep(Duration::from_millis(200));
    }

    assert!(
        found,
        "background catch-up must absorb the offline edit so zzz_offline_marker becomes searchable"
    );
    // serve killed + temp dir removed on drop.
}
