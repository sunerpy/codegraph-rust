//! REGRESSION — stale index (file shrank since indexing) → `codegraph_explore`.
//!
//! This locks the CLASS of bug behind the historical `engine.rs:848` slice
//! panic: `range start index N out of range for slice of length M`. The trigger
//! is a STALE index — a symbol's stored `end_line` is deeper than the file's
//! CURRENT line count, because the file shrank on disk AFTER it was indexed but
//! BEFORE any re-sync. `render_explore_file` then clustered a range whose start
//! sat past EOF and sliced out of bounds.
//!
//! Why the pre-existing suite never caught it: no test ever created the stale
//! state (index built large → file truncated → served without a resync). This
//! integration test reproduces it end-to-end through the REAL `serve --mcp`
//! process:
//!
//!   1. Write a LONG source file (>`WHOLE_FILE_MAX_LINES` = 220 lines) with a
//!      symbol near the very end, then `codegraph init` — the DB records that
//!      symbol's `end_line` deep in the file (~896).
//!   2. SHRINK the file on disk to ~300 lines: still >220 so `explore` takes the
//!      CLUSTERING path (not whole-file), but now the stored `end_line` (896)
//!      exceeds the current line count (300) — the exact stale state.
//!   3. Serve with `--no-watch` so the file watcher does NOT auto-resync and
//!      erase the stale state, then `codegraph_explore` for the shrunk file's
//!      symbols (a 2-symbol query mirroring the user's "McpServer
//!      GraphTraverser" report that clustered ranges across files).
//!   4. ASSERT the response is a normal exploration result (`isError` NOT true,
//!      NO "tool handler panicked"), and that a FOLLOW-UP `codegraph_search`
//!      still succeeds — proving the worker/runtime survived the request.
//!
//! RED→GREEN: against the pre-Fix-C engine this explore returns
//! `{"isError":true,"...tool handler panicked"}` (the spawn_blocking unwind
//! catch converts the slice panic into an isError result — the process limps on
//! but the explore request is POISONED). With Fix C's clamp in the tree it
//! returns real clustered source and `isError:false`. See
//! `.omo/evidence/test-explore-stale-index.txt` for the captured proof.
//!
//! De-flaking: `--no-watch` makes the stale state deterministic (nothing
//! resyncs), so no polling/sleep is needed — the two `tools/call`s are issued
//! synchronously. Responses may arrive OUT OF ORDER (async handler), so reads
//! match on the JSON-RPC `id`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-stale-{label}-{}-{}",
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

/// A live `serve --mcp` child driven over stdin/stdout with line-delimited
/// JSON-RPC. Killed + reaped on drop so no orphan serve process leaks. The
/// `BufReader` is created ONCE and held for the process lifetime — re-creating
/// it per read would discard buffered bytes and silently drop responses.
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
    /// stdout closes (a closed pipe returns `None` — signals a crashed server).
    fn read_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        match self.reader.read_line(&mut buf) {
            Ok(0) => None,
            Ok(_) => Some(buf),
            Err(_) => None,
        }
    }

    /// Read response lines until one carries `"id":<id>` (responses can arrive
    /// out of order because the handler runs on a blocking pool). Bounded by a
    /// generous deadline so a crashed/hung server fails the test instead of
    /// blocking forever. `None` means the pipe closed (server died) before the
    /// awaited id arrived.
    fn read_response_for(&mut self, id: u64) -> Option<String> {
        let needle = format!("\"id\":{id}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match self.read_line() {
                Some(line) if line.contains(&needle) => return Some(line),
                // A different id (e.g. an earlier/later response): keep reading.
                Some(_) => continue,
                None => return None,
            }
        }
        None
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The shrink-target file, generated at two sizes so the test controls the
/// stale state exactly. Long form: a symbol near line ~893 → indexed
/// `end_line` ~896, comfortably past `WHOLE_FILE_MAX_LINES` so `explore`
/// clusters. Short form: 300 lines — still clustering (>220), but now every
/// stored line number for the tail symbol is past EOF.
const TARGET_REL: &str = "src/big.rs";
const LONG_FILLER: usize = 890;
const SHRUNK_LINES: usize = 300;

fn write_long_target(project: &Path) {
    let mut body = String::new();
    body.push_str("pub struct McpServer {\n    pub name: String,\n}\n\n");
    for i in 1..=LONG_FILLER {
        // Filler keeps the file long so explore takes the clustering path.
        body.push_str(&format!("// filler line {i} keeps the file long\n"));
    }
    // Symbol near the very END — its indexed end_line will be deep in the file.
    body.push_str("pub fn graph_traverser_entry() -> usize {\n");
    body.push_str("    let server = McpServer { name: String::from(\"cg\") };\n");
    body.push_str("    server.name.len()\n");
    body.push_str("}\n");
    fs::write(project.join(TARGET_REL), body).unwrap();
}

fn write_shrunk_target(project: &Path) {
    // Fewer lines than the indexed tail symbol's end_line, yet still >220 so
    // explore CLUSTERS (the panic path) rather than returning the file whole.
    let mut body = String::new();
    body.push_str("pub struct McpServer {\n    pub name: String,\n}\n");
    // Lines 4..=SHRUNK_LINES are filler; total == SHRUNK_LINES.
    for i in 4..=SHRUNK_LINES {
        body.push_str(&format!("// filler line {i}\n"));
    }
    fs::write(project.join(TARGET_REL), body).unwrap();
    // Sanity: the shrunk file must stay in the clustering band and be far
    // shorter than the indexed tail symbol's end_line.
    let actual = fs::read_to_string(project.join(TARGET_REL))
        .unwrap()
        .lines()
        .count();
    assert!(
        (221..LONG_FILLER).contains(&actual),
        "shrunk file must be >220 (clusters) and < original ({LONG_FILLER}); got {actual}"
    );
}

/// Extract the `isError` boolean from a JSON-RPC result line (absent ⇒ false).
fn is_error(line: &str) -> bool {
    line.contains("\"isError\":true")
}

#[test]
fn explore_survives_stale_index_after_file_shrinks() {
    let dir = TestDir::new("shrink");
    let project = dir.path().join("proj");
    fs::create_dir_all(project.join("src")).unwrap();

    // 1. Write the LONG file and index it — records the tail symbol's deep
    //    end_line (~896) into the DB.
    write_long_target(&project);
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    // 2. SHRINK the file below the indexed end_line, staying in the clustering
    //    band. This is the STALE state that historically panicked.
    write_shrunk_target(&project);

    // 3. Serve DIRECT + --no-watch so nothing resyncs and erases the stale
    //    state. `no_watch=true` is CRITICAL — with the watcher on, the shrink
    //    would trigger a re-index and the bug window would close.
    let mut serve = ServeProcess::spawn(&project, true);

    serve.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e-test","version":"0"}}}"#);
    let init = serve
        .read_response_for(1)
        .expect("initialize response (server alive)");
    assert!(
        init.contains("serverInfo") || init.contains("protocolVersion") || init.contains("result"),
        "unexpected initialize response: {init}"
    );

    // 4. Explore the shrunk file's symbols — 2-symbol query mirroring the user's
    //    report that clustered ranges across the stale span.
    serve.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codegraph_explore","arguments":{"query":"McpServer graph_traverser_entry"}}}"#);
    let explore = serve
        .read_response_for(2)
        .expect("explore must return a response — a closed pipe here means the server CRASHED");

    // 5a. The pre-fix panic surfaced as `{"isError":true,"...tool handler
    //     panicked"}`. Assert BOTH the sentinel text is absent AND isError is
    //     false — this is exactly the behavior that failed before Fix C.
    assert!(
        !explore.contains("tool handler panicked"),
        "explore over a stale index must NOT panic in the handler; got: {explore}"
    );
    assert!(
        !is_error(&explore),
        "explore over a stale index must return a normal (non-error) result; got: {explore}"
    );
    assert!(
        explore.contains("\"result\""),
        "explore must return a JSON-RPC result; got: {explore}"
    );

    // 5b. A FOLLOW-UP request must still succeed — proving the worker thread /
    //     runtime survived the stale-index request (before Fix C the panic
    //     poisoned that request; this guards the whole request path stays live).
    serve.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"codegraph_search","arguments":{"query":"McpServer"}}}"#);
    let search = serve.read_response_for(3).expect(
        "follow-up search must return — server must still be ALIVE after the stale explore",
    );
    assert!(
        !is_error(&search) && search.contains("\"result\""),
        "follow-up search must succeed, proving the runtime survived; got: {search}"
    );
    assert!(
        search.contains("McpServer"),
        "follow-up search should still find the indexed symbol; got: {search}"
    );
    // serve killed + temp dir removed on drop.
}
