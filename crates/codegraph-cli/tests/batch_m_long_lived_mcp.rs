//! Batch M acceptance item 16 (frozen plan lines 775-778):
//! `long_lived_v2_mcp_releases_handles_per_request`.
//!
//! ONE long-lived SHIPPED MCP process (`codegraph serve --mcp --path <project>`)
//! must complete request 1, release EVERY SQLite handle and index lease it used,
//! permit an atomic replacement of the v2 main database file WITHOUT any
//! participation from the compatibility close seam, and then serve ONLY the
//! replacement graph on the next request.
//!
//! # Determinism: a child-process READY/CONTINUE barrier, never a sleep
//!
//! The barrier is the MCP wire protocol itself, so nothing here infers ordering
//! from elapsed time:
//!
//! 1. **READY** — the parent blocks until the child frames the JSON-RPC response
//!    for request 1 on stdout. rmcp writes that frame only after the request's
//!    OWNED, fully materialized result was produced, so the arrival of the frame
//!    is proof that request 1 completed end-to-end (not merely that its SQL
//!    finished).
//! 2. **HANDLES RELEASED** — the parent then acquires the namespace's EXCLUSIVE
//!    index lease. A retained reader lease (or any request-scoped `Store` kept
//!    alive between requests) makes this acquisition fail, so the exclusive
//!    lease is the fail-closed observation that the child let go.
//! 3. **REPLACEMENT** — still holding the exclusive lease, the parent renames a
//!    separately built database over the live v2 main database file. On native
//!    Windows this rename (`MoveFileEx` + `REPLACE_EXISTING`) FAILS with a
//!    sharing violation while any process holds an open handle on the
//!    destination, because SQLite opens without `FILE_SHARE_DELETE`. That is the
//!    Windows-specific handle proof this item exists for.
//! 4. **CONTINUE** — the parent releases the lease and writes the next request
//!    frame. That frame is the continue signal; the child cannot have started it
//!    earlier because it had not been written.
//!
//! Channel timeouts (`WAIT`) and the lease deadline are DEADLOCK GUARDS only. No
//! assertion in this file is satisfied by waiting.
//!
//! # The replacement graph cannot come from a reindex
//!
//! `serve --mcp --path` runs a one-shot startup catch-up sync on a detached
//! thread (`spawn_catch_up`), and `--no-watch` does not disable it. If the served
//! project's SOURCES could produce the replacement graph, that background sync
//! could satisfy the post-replacement assertions without the replaced bytes ever
//! being read — and could equally delete the replacement rows again, making the
//! test flaky in both directions.
//!
//! So the two distinguishing source files live under a directory the served
//! project's own root `.gitignore` excludes:
//!
//! - No sync can ever CREATE those rows: `scan_project` never yields the paths.
//!   The test asserts this with the shipped scanner before the acceptance runs.
//! - No sync can ever DELETE those rows: the cold sync's removal pass only
//!   considers a tracked path that is absent from disk, both files stay present,
//!   and the same ignore policy filters them out of `should_handle_file` anyway.
//!
//! What remains scannable is one neutral file that is byte-identical in both
//! graphs, so the startup catch-up is a proven no-op instead of a race. The ONLY
//! way request 2 can observe the replacement file is by reading the database
//! bytes the parent supplied.
//!
//! # Scope
//!
//! Query-side acceptance over the shipped binary. Nothing here is a production
//! change and nothing weakens a state-slot, sidecar, stamp, or lease gate: both
//! databases are produced by the real `codegraph init`, so the target namespace
//! keeps its own valid permanent lock, its own `Current` state slots, and an
//! exact extraction stamp in the replaced bytes.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_extract::ExtractOptions;
use codegraph_store::IndexLease;
use serde_json::{Value, json};

/// Finite deadlock guard for every blocking wait. Never ordering evidence.
const WAIT: Duration = Duration::from_secs(60);

/// The scannable file both graphs share, byte-identical. Its only job is to keep
/// the startup catch-up sync a proven no-op instead of a degenerate empty scan.
const NEUTRAL_FILE: &str = "src/pwqneutral.ts";
/// The gitignored directory holding the two supplied-graph-only sources. Nothing
/// under it is ever scanned in the SERVED project.
const SUPPLIED_DIR: &str = "supplied";
/// The symbol that exists ONLY in the original graph.
const ORIGINAL_SYMBOL: &str = "qxwoolrig";
/// The repo-relative file that exists ONLY in the original graph.
const ORIGINAL_FILE: &str = "supplied/mdzzplant.ts";
/// The symbol that exists ONLY in the replacement graph.
const REPLACEMENT_SYMBOL: &str = "hbtvancur";
/// The repo-relative file that exists ONLY in the replacement graph.
const REPLACEMENT_FILE: &str = "supplied/kfrnbadge.ts";

/// The compatibility close seam this acceptance flow must never touch, spelled
/// in two halves so this file's own bytes are not a match for it.
const CLOSE_SEAM: &str = concat!("close_cached", "_handles");

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

/// A temp directory removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-m16-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Write one source file, creating parents.
fn write_source(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("source parent")).expect("create source dir");
    fs::write(&path, contents).expect("write source file");
}

fn source_for(symbol: &str) -> String {
    format!("export function {symbol}(): number {{\n  return 1;\n}}\n")
}

/// The neutral scannable source, byte-identical in every project here.
fn neutral_source() -> String {
    source_for("pwqneutral")
}

/// Run the SHIPPED `codegraph init` over `project`, producing a real v2
/// namespace (permanent lock + `Current` state slots + stamped, checkpointed
/// database). Never a hand-authored fixture.
fn shipped_init(project: &Path) {
    let output = base_command()
        .args(["init"])
        .arg(project)
        .output()
        .expect("run codegraph init");
    assert!(
        output.status.success(),
        "codegraph init failed for {}: stdout={} stderr={}",
        project.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A command with the ambient index-location override cleared, so both the
/// staging build and the served project resolve their own default v2 namespace.
/// The daemon opt-out keeps ONE direct long-lived process (never a proxy to a
/// shared daemon), and the watch opt-out keeps the live watcher off.
fn base_command() -> Command {
    let mut command = Command::new(bin());
    command
        .env_remove("CODEGRAPH_DIR")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1");
    command
}

fn index_paths(project: &Path) -> IndexPaths {
    IndexPaths::resolve(project, None).expect("resolve v2 IndexPaths")
}

/// One long-lived shipped MCP stdio server plus the reader thread that turns its
/// stdout into framed JSON-RPC responses.
struct LongLivedMcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl LongLivedMcp {
    /// Spawn `serve --mcp --path <project> --no-watch` with the daemon disabled,
    /// so ONE direct, long-lived process owns the whole session.
    fn spawn(project: &Path) -> Self {
        let mut child = base_command()
            .args(["serve", "--mcp", "--no-watch", "--path"])
            .arg(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn long-lived serve --mcp");
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take().expect("child stdin");
        Self {
            child,
            stdin: Some(stdin),
            lines,
        }
    }

    fn send(&mut self, frame: &Value) {
        let stdin = self.stdin.as_mut().expect("child stdin still owned");
        writeln!(stdin, "{frame}").expect("write MCP frame");
        stdin.flush().expect("flush MCP frame");
    }

    /// Block until the child frames the response for `id`. The ARRIVAL of this
    /// frame is the child's READY acknowledgement: rmcp writes it only after the
    /// request's owned result was fully produced.
    fn await_response(&self, id: i64) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default();
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("no response for id {id} before deadline: {error}"));
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(trimmed)
                && value.get("id").and_then(Value::as_i64) == Some(id)
            {
                return value;
            }
        }
    }

    /// Close stdin (EOF) and wait for the process to exit within the guard.
    fn shutdown(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + WAIT;
        loop {
            match self.child.try_wait().expect("poll child status") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "the long-lived MCP process must exit cleanly on stdin EOF: {status:?}"
                    );
                    return;
                }
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "the long-lived MCP process did not exit before its finite deadline"
                    );
                    std::thread::park_timeout(Duration::from_millis(5));
                }
            }
        }
    }
}

impl Drop for LongLivedMcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize_frames() -> [Value; 2] {
    [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "m16-acceptance", "version": "0" }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ]
}

fn search_frame(id: i64, project: &Path, query: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "codegraph_search",
            "arguments": {
                "query": query,
                "projectPath": project.to_str().expect("utf8 project path")
            }
        }
    })
}

/// The rendered tool text, rejecting a protocol error or an `isError` result so a
/// failed request can never be mistaken for a graph observation.
fn tool_text(response: &Value, context: &str) -> String {
    assert!(
        response.get("error").is_none(),
        "{context} returned a JSON-RPC error: {response}"
    );
    assert_ne!(
        response["result"]["isError"],
        json!(true),
        "{context} returned an error result: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Freeze the supplied-graph sources out of every future scan of the SERVED
/// project, then PROVE it with the shipped scanner.
///
/// This is what makes the acceptance deterministic against the startup catch-up
/// sync: the scanner is the same `scan_project` a sync feeds from, so an empty
/// result for both supplied paths means no sync in this project can invent the
/// replacement rows (or the original ones) from disk.
fn freeze_supplied_sources_out_of_scans(project: &Path) {
    fs::write(project.join(".gitignore"), format!("{SUPPLIED_DIR}/\n"))
        .expect("write root .gitignore");

    let scanned = codegraph_extract::engine::scan_project(project, &ExtractOptions::default())
        .expect("scan the served project with the shipped scanner");
    assert!(
        scanned.contains(&NEUTRAL_FILE.to_string()),
        "the neutral source must stay scannable so startup catch-up is a real no-op: {scanned:?}"
    );
    assert!(
        !scanned.contains(&REPLACEMENT_FILE.to_string()),
        "no sync may be able to index {REPLACEMENT_FILE}, or request 2 could pass by reindexing \
         instead of by reading the replaced database: {scanned:?}"
    );
    assert!(
        !scanned.contains(&ORIGINAL_FILE.to_string()),
        "no sync may be able to re-index {ORIGINAL_FILE} back into the replacement graph: \
         {scanned:?}"
    );
    // Both supplied sources stay PRESENT on disk, so the cold sync's removal
    // pass (tracked-but-absent) can never delete their rows either.
    for rel in [ORIGINAL_FILE, REPLACEMENT_FILE] {
        assert!(
            project.join(rel).is_file(),
            "{rel} must remain on disk so no sync can classify it as removed"
        );
    }
}

/// Build the replacement database in a staging project and return its path. The
/// staging project holds the neutral file plus ONLY the replacement supplied
/// source and carries NO `.gitignore`, so its graph contains the replacement
/// file and cannot contain the original one.
fn build_replacement_database(staging: &TestDir) -> PathBuf {
    let project = staging.path().join("replacement-build");
    fs::create_dir_all(&project).expect("create staging project");
    write_source(&project, NEUTRAL_FILE, &neutral_source());
    write_source(&project, REPLACEMENT_FILE, &source_for(REPLACEMENT_SYMBOL));
    shipped_init(&project);
    let db = index_paths(&project).current_db();
    assert!(
        db.is_file(),
        "staging build must produce a database at {}",
        db.display()
    );
    db
}

/// Replace the live v2 main database file while the long-lived MCP process is
/// still running. The project's SOURCES are deliberately left untouched.
///
/// The EXCLUSIVE index lease is acquired first: a retained reader lease from
/// request 1 makes this fail, which is the fail-closed proof that the long-lived
/// process released its lease when the request ended. `fs::rename` is then the
/// atomic replacement — on native Windows it fails outright if any process still
/// holds an open handle on the destination.
fn replace_database(project: &Path, replacement_db: &Path) {
    let paths = index_paths(project);
    let lease = IndexLease::acquire_exclusive_existing(&paths, Instant::now() + WAIT, || false)
        .expect(
            "the long-lived MCP process must have released every reader lease once request 1 \
             completed, so the exclusive lease is available for the replacement",
        );

    let target = paths.current_db();
    fs::rename(replacement_db, &target).unwrap_or_else(|error| {
        panic!(
            "atomically replacing {} must succeed while the long-lived MCP process is running \
             (a retained SQLite handle makes this fail on native Windows): {error}",
            target.display()
        )
    });

    // The read path refuses a `Current` namespace whose SQLite sidecars
    // reappeared without a state change. Assert that fail-closed contract holds
    // for the replaced artifact instead of repairing it.
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = target.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        assert!(
            !sidecar.exists(),
            "the replaced namespace must stay sidecar-free: {} exists",
            sidecar.display()
        );
    }
    assert!(
        paths.permanent_lock().is_file(),
        "the replacement must preserve the permanent lock at {}",
        paths.permanent_lock().display()
    );
    let [slot_zero, slot_one] = paths.state_slots();
    assert!(
        slot_zero.is_file() || slot_one.is_file(),
        "the replacement must preserve the published Current state slots"
    );

    drop(lease);
}

#[test]
fn long_lived_v2_mcp_releases_handles_per_request() {
    // GIVEN a really indexed v2 project whose graph contains the original
    // supplied symbol, with both supplied sources frozen out of every future
    // scan, and a separately built replacement database that contains ONLY the
    // replacement supplied symbol.
    let home = TestDir::new("served");
    let project = home.path().join("project");
    fs::create_dir_all(&project).expect("create served project");
    write_source(&project, NEUTRAL_FILE, &neutral_source());
    write_source(&project, ORIGINAL_FILE, &source_for(ORIGINAL_SYMBOL));
    shipped_init(&project);

    write_source(&project, REPLACEMENT_FILE, &source_for(REPLACEMENT_SYMBOL));
    freeze_supplied_sources_out_of_scans(&project);

    let staging = TestDir::new("staging");
    let replacement_db = build_replacement_database(&staging);

    // ONE long-lived shipped MCP process serves the whole session.
    let mut server = LongLivedMcp::spawn(&project);
    let [initialize, initialized] = initialize_frames();
    server.send(&initialize);
    let init_response = server.await_response(1);
    assert_eq!(
        init_response["result"]["serverInfo"]["name"],
        json!("codegraph"),
        "the long-lived process must be the shipped codegraph MCP server: {init_response}"
    );
    server.send(&initialized);

    // WHEN request 1 completes (its framed response IS the READY barrier) ...
    server.send(&search_frame(2, &project, ORIGINAL_SYMBOL));
    let first = tool_text(&server.await_response(2), "request 1");
    assert!(
        first.contains(ORIGINAL_FILE),
        "request 1 must observe the original graph: {first}"
    );
    assert!(
        !first.contains(REPLACEMENT_FILE),
        "request 1 must not observe the replacement graph: {first}"
    );

    // ... the parent takes the exclusive lease (handles released) and atomically
    // replaces the v2 database. No compatibility seam participates.
    replace_database(&project, &replacement_db);

    // THEN the CONTINUE frame (request 2) serves ONLY the replacement graph.
    server.send(&search_frame(3, &project, REPLACEMENT_SYMBOL));
    let second = tool_text(&server.await_response(3), "request 2");
    assert!(
        second.contains("## Search Results ("),
        "request 2 must render real search results, not an echo: {second}"
    );
    assert!(
        second.contains(REPLACEMENT_FILE),
        "request 2 must serve the replacement graph: {second}"
    );
    assert!(
        !second.contains(ORIGINAL_FILE),
        "request 2 must not leak the original graph: {second}"
    );

    // And the original symbol is GONE from the same long-lived session: the
    // absence is observed through a lookup that resolves nothing, not through a
    // response that merely omits it.
    server.send(&search_frame(4, &project, ORIGINAL_SYMBOL));
    let third = tool_text(&server.await_response(4), "request 3");
    assert!(
        third.contains("No results found"),
        "the original symbol must be absent from the replacement graph: {third}"
    );
    assert!(
        !third.contains(ORIGINAL_FILE),
        "the original file must be absent from the replacement graph: {third}"
    );

    server.shutdown();
}

/// The acceptance flow must be independent of the compatibility close seam, so
/// no CODE in this file may name it — not as a call, an import, an alias, or a
/// wrapper. Checked structurally against this file's own bytes: comment lines
/// (which document the seam by name) are excluded, and the needle itself is
/// assembled from two halves so neither this oracle nor its failure message is a
/// match for what it forbids.
#[test]
fn acceptance_flow_never_calls_the_close_helper() {
    let offenders = code_lines_naming(include_str!("batch_m_long_lived_mcp.rs"), CLOSE_SEAM);
    assert!(
        offenders.is_empty(),
        "the M16 acceptance flow must not depend on the {CLOSE_SEAM} compatibility seam, but \
         these code lines name it: {offenders:?}"
    );
}

/// Non-comment lines of `source` that contain `needle`, as `"<line>: <text>"`.
/// A line whose first non-whitespace characters are `//` is documentation (this
/// file's own module docs and comments discuss the seam by name), so only real
/// code is inspected.
fn code_lines_naming(source: &str, needle: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains(needle))
        .map(|(index, line)| format!("{}: {}", index + 1, line.trim()))
        .collect()
}

#[cfg(test)]
mod oracle_tests {
    use super::code_lines_naming;

    /// The forbidden-name oracle must SEE a real invocation and IGNORE a comment
    /// that merely mentions the name, so it can neither pass vacuously nor fail
    /// on this file's own documentation.
    #[test]
    fn code_lines_naming_sees_code_and_ignores_comments() {
        let needle = concat!("forbidden", "_seam");
        let source = concat!(
            "//! docs mention forbidden_seam by name\n",
            "    // so does an indented comment: forbidden_seam\n",
            "let clean = other_call();\n",
            "    store.forbidden_seam();\n"
        );
        let found = code_lines_naming(source, needle);
        assert_eq!(
            found,
            vec!["4: store.forbidden_seam();".to_string()],
            "only the real invocation line is an offender"
        );
    }
}
