//! Batch M acceptance item 17 (frozen plan line 779):
//! `failed_engine_open_is_not_cached_and_next_request_recovers`.
//!
//! ONE long-lived SHIPPED MCP process (`codegraph serve --mcp --path <project>`)
//! receives a request whose engine/`Store` open FAILS for a real v2
//! state/artifact reason. The process must stay healthy, must retain NOTHING
//! from that failed open — no cached error, no partial engine, no `Store`, no
//! SQLite handle, no lease, no stale project result — and the NEXT request in the
//! SAME process, after the namespace is repaired without a restart, must succeed
//! and serve ONLY the repaired graph.
//!
//! # The staged failure is a real v2 inconsistency, not a stub
//!
//! The served project is really indexed by the shipped `codegraph init`, then
//! BOTH fixed state slots are removed and nothing else. The namespace therefore
//! classifies `Missing` while its main database (and its permanent lock) are
//! still present, which the read gate refuses with
//! `state is missing but a database artifact already exists at <db>`
//! (`reject_missing_database_artifacts`). Nothing here weakens or repairs a gate
//! to produce the failure, and nothing teaches a production read to accept an
//! invalid state.
//!
//! Crucially the failure happens AFTER project resolution: the current-namespace
//! DB file still exists, so `roots::probe_root` classifies the project `Indexed`
//! and `resolve_project_arg` resolves it. The test asserts request 1 carries the
//! ENGINE-OPEN diagnostic (`Failed to open project at …`), never the
//! resolution-miss diagnostic, so a resolution failure can never be mistaken for
//! the engine-open failure this item is about.
//!
//! # Determinism: protocol frames and fail-closed gates, never a sleep
//!
//! 1. **READY** — the parent blocks until the child frames the JSON-RPC response
//!    for request 1. rmcp writes that frame only after the request's OWNED result
//!    was fully materialized, so the frame proves request 1 finished end-to-end.
//! 2. **NO RETAINED LEASE** — the parent then acquires the namespace's EXCLUSIVE
//!    lease. A shared reader lease retained by the FAILED open makes this
//!    acquisition fail, so the exclusive lease is the fail-closed observation
//!    that the failed open let go of everything.
//! 3. **NO RETAINED SQLITE HANDLE** — still holding that exclusive lease, the
//!    parent renames the separately built repaired database over the live main
//!    database file. On native Windows that rename fails with a sharing violation
//!    while any process holds an open handle on the destination, so it is the
//!    Windows handle proof for the failed-open path.
//! 4. **REPAIR COMPLETION** — the repair is finished by the protocol-aware
//!    `Missing -> Building -> Current` fixture finalizer, and the parent asserts
//!    the namespace classifies `Current` (plus lock/state-slot/sidecar
//!    invariants) BEFORE sending request 2. Completion is observed from published
//!    state, never from elapsed time.
//! 5. **CONTINUE** — request 2's frame is written only after that. The child
//!    cannot have started it earlier because it had not been written.
//!
//! Channel/lease deadlines (`WAIT`) are DEADLOCK GUARDS only. No assertion in
//! this file is satisfied by waiting, by mtime, or by process exit.
//!
//! # Startup catch-up can neither repair the staged state nor fake the recovery
//!
//! `serve --mcp --path` runs a one-shot startup catch-up sync on a detached
//! thread and `--no-watch` does not disable it, so it must be excluded as an
//! explanation for BOTH halves of the acceptance:
//!
//! - It cannot repair the staged `Missing`-with-database namespace:
//!   [`catch_up_sync_refuses_missing_state_with_database_without_mutating_bytes`]
//!   drives the SAME `codegraph_watch::sync_project_once` entry point the
//!   catch-up thread calls against an identically staged namespace and proves it
//!   fails with the same state/artifact diagnostic while leaving the database
//!   bytes byte-identical and the state slots absent. That is a gate, not a race.
//! - It cannot invent the repaired rows: the two distinguishing sources live
//!   under a directory the served project's own root `.gitignore` excludes, which
//!   the test PROVES with the shipped scanner. No sync can create those rows
//!   (`scan_project` never yields the paths) and none can delete them (the cold
//!   removal pass only considers a tracked path that is ALSO absent from disk,
//!   and both files stay present). The only scannable file is byte-identical in
//!   both graphs, so a catch-up that somehow ran after the repair is a proven
//!   no-op rather than a source of the assertions.
//!
//! # A legacy namespace is not a recovery source
//!
//! The project also carries a legacy `.codegraph/codegraph.db` holding a TRAP
//! symbol that exists in no other graph. Request 2 must not surface it, so a
//! silent legacy fallback cannot masquerade as recovery.
//!
//! # Scope
//!
//! Query-side acceptance over the shipped binary plus one in-process test of the
//! same engine-open seam. M15 already made every MCP engine request-scoped, so
//! this item is the behavioral proof of that ownership; no production byte
//! changes here.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_extract::ExtractOptions;
use codegraph_store::{ExtractionStatus, IndexLease, Store};
use serde_json::{Value, json};

/// Finite deadlock guard for every blocking wait. Never ordering evidence.
const WAIT: Duration = Duration::from_secs(60);

/// The scannable file every project here shares, byte-identical, so the startup
/// catch-up sync is a proven no-op instead of a degenerate empty scan.
const NEUTRAL_FILE: &str = "src/mkqneutral.ts";
/// The gitignored directory holding the supplied-graph-only sources. Nothing
/// under it is ever scanned in the SERVED project.
const SUPPLIED_DIR: &str = "supplied";
/// The symbol that exists ONLY in the pre-repair (original) graph.
const ORIGINAL_SYMBOL: &str = "zrqmfoss";
/// The repo-relative file that exists ONLY in the pre-repair graph.
const ORIGINAL_FILE: &str = "supplied/zrqmfoss.ts";
/// The symbol that exists ONLY in the repaired graph.
const REPAIRED_SYMBOL: &str = "vtwkhelm";
/// The repo-relative file that exists ONLY in the repaired graph.
const REPAIRED_FILE: &str = "supplied/vtwkhelm.ts";
/// The symbol that exists ONLY in the LEGACY `.codegraph` database.
const LEGACY_TRAP_SYMBOL: &str = "jbxdlurn";
/// The repo-relative file that exists ONLY in the legacy database.
const LEGACY_TRAP_FILE: &str = "supplied/jbxdlurn.ts";

/// The exact fail-closed diagnostic the v2 read gate produces for a `Missing`
/// state whose database artifact still exists.
const MISSING_WITH_DB: &str = "state is missing but a database artifact already exists";

/// The compatibility close seam this recovery flow must never touch, spelled in
/// two halves so this file's own bytes are not a match for it.
const CLOSE_SEAM: &str = concat!("close_cached", "_handles");

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

/// A temp directory removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-m17-{label}-{}-{}",
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
    source_for("mkqneutral")
}

/// A command with the ambient index-location override cleared, the daemon opted
/// out (ONE direct long-lived process, never a proxy) and the live watcher off.
fn base_command() -> Command {
    let mut command = Command::new(bin());
    command
        .env_remove("CODEGRAPH_DIR")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1");
    command
}

/// Run the SHIPPED `codegraph init` over `project`, producing a real v2
/// namespace (permanent lock + published `Current` state slots + stamped,
/// checkpointed database). Never a hand-authored fixture.
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

fn index_paths(project: &Path) -> IndexPaths {
    IndexPaths::resolve(project, None).expect("resolve v2 IndexPaths")
}

/// Build a project containing the neutral source plus `supplied_rel`, index it
/// with the shipped `init`, and return its current database path. The staging
/// project carries NO `.gitignore`, so its graph really does contain
/// `supplied_rel` and cannot contain any other supplied source.
fn build_graph_database(
    staging: &TestDir,
    label: &str,
    supplied_rel: &str,
    symbol: &str,
) -> PathBuf {
    let project = staging.path().join(label);
    fs::create_dir_all(&project).expect("create staging project");
    write_source(&project, NEUTRAL_FILE, &neutral_source());
    write_source(&project, supplied_rel, &source_for(symbol));
    shipped_init(&project);
    let db = index_paths(&project).current_db();
    assert!(
        db.is_file(),
        "staging build must produce a database at {}",
        db.display()
    );
    db
}

/// Materialize a really indexed served project, freeze the supplied sources out
/// of every future scan, plant the legacy trap graph, and then break the v2
/// namespace into the REAL `Missing`-state-with-existing-database inconsistency
/// by removing ONLY the two fixed state slots.
fn stage_missing_state_with_database(project: &Path, staging: &TestDir, label: &str) -> IndexPaths {
    fs::create_dir_all(project).expect("create served project");
    write_source(project, NEUTRAL_FILE, &neutral_source());
    write_source(project, ORIGINAL_FILE, &source_for(ORIGINAL_SYMBOL));
    shipped_init(project);

    // Both distinguishing sources exist on disk from here on, so no sync can
    // classify either as removed.
    write_source(project, REPAIRED_FILE, &source_for(REPAIRED_SYMBOL));
    write_source(project, LEGACY_TRAP_FILE, &source_for(LEGACY_TRAP_SYMBOL));
    freeze_supplied_sources_out_of_scans(project);
    plant_legacy_trap_graph(project, staging, label);

    let paths = index_paths(project);
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Current,
        "the shipped init must leave a readable Current namespace before it is broken"
    );

    for slot in paths.state_slots() {
        if slot.exists() {
            fs::remove_file(&slot).expect("remove one fixed state slot");
        }
    }

    // The namespace is now genuinely inconsistent: no state, but the database
    // and the permanent lock are still there.
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Missing,
        "removing both fixed state slots must classify the namespace Missing"
    );
    assert!(
        paths.current_db().is_file(),
        "the staged failure requires the main database to still exist at {}",
        paths.current_db().display()
    );
    assert!(
        paths.permanent_lock().is_file(),
        "the staged failure keeps the permanent lock at {}",
        paths.permanent_lock().display()
    );
    assert!(
        !paths.tombstone().exists(),
        "the staged failure is a missing-state inconsistency, not an uninit tombstone"
    );

    // The read gate must reject it for exactly the staged reason, checked before
    // any server is involved so the acceptance cannot mistake another failure
    // for this one.
    let rejection = Store::open_for_read(&paths, Instant::now() + WAIT, || false)
        .expect_err("a Missing state with an existing database must fail closed")
        .to_string();
    assert!(
        rejection.contains(MISSING_WITH_DB),
        "the staged condition must be the missing-state-with-database refusal: {rejection}"
    );

    paths
}

/// Freeze the supplied sources out of every future scan of `project`, then PROVE
/// it with the shipped scanner — the same `scan_project` a sync feeds from.
fn freeze_supplied_sources_out_of_scans(project: &Path) {
    fs::write(project.join(".gitignore"), format!("{SUPPLIED_DIR}/\n"))
        .expect("write root .gitignore");

    let scanned = codegraph_extract::engine::scan_project(project, &ExtractOptions::default())
        .expect("scan the served project with the shipped scanner");
    assert!(
        scanned.contains(&NEUTRAL_FILE.to_string()),
        "the neutral source must stay scannable so startup catch-up is a real no-op: {scanned:?}"
    );
    for rel in [ORIGINAL_FILE, REPAIRED_FILE, LEGACY_TRAP_FILE] {
        assert!(
            !scanned.contains(&rel.to_string()),
            "no sync may be able to index {rel}, or the recovery request could pass by reindexing \
             instead of by reading the repaired database: {scanned:?}"
        );
        assert!(
            project.join(rel).is_file(),
            "{rel} must remain on disk so no sync can classify it as removed"
        );
    }
}

/// Plant a LEGACY `.codegraph/codegraph.db` holding a graph that exists nowhere
/// else, so a silent legacy fallback cannot masquerade as recovery.
fn plant_legacy_trap_graph(project: &Path, staging: &TestDir, label: &str) {
    let trap_db = build_graph_database(
        staging,
        &format!("{label}-legacy-trap"),
        LEGACY_TRAP_FILE,
        LEGACY_TRAP_SYMBOL,
    );
    let legacy_root = project.join(".codegraph");
    fs::create_dir_all(&legacy_root).expect("create the legacy root");
    fs::copy(&trap_db, legacy_root.join("codegraph.db")).expect("plant the legacy trap database");
}

/// Repair the staged namespace WITHOUT restarting the server.
///
/// The EXCLUSIVE lease is acquired first: a shared reader lease retained by the
/// FAILED open makes this fail, which is the fail-closed proof that the failed
/// open released everything. `fs::rename` under that lease then installs the
/// repaired database — on native Windows it fails outright if any process still
/// holds an open handle on the destination. The protocol-aware fixture finalizer
/// republishes `Missing -> Building -> Current` with the exact extraction stamp,
/// so no production read gate is weakened to make the recovery work.
fn repair_without_restart(paths: &IndexPaths, repaired_db: &Path) {
    let lease = IndexLease::acquire_exclusive_existing(paths, Instant::now() + WAIT, || false)
        .expect(
            "the failed engine open must have retained no shared reader lease, so the exclusive \
             lease is available for the repair",
        );

    let target = paths.current_db();
    fs::rename(repaired_db, &target).unwrap_or_else(|error| {
        panic!(
            "installing the repaired database at {} must succeed while the long-lived MCP process \
             is running (a retained SQLite handle from the failed open makes this fail on native \
             Windows): {error}",
            target.display()
        )
    });
    drop(lease);

    codegraph_store::test_support::finalize_current_test_fixture(paths)
        .expect("republish Missing -> Building -> Current over the repaired database");

    // Repair COMPLETION is observed from published state and artifact shape, not
    // from elapsed time.
    assert_eq!(
        Store::extraction_status(paths),
        ExtractionStatus::Current,
        "the repair must publish a readable Current namespace"
    );
    let [slot_zero, slot_one] = paths.state_slots();
    assert!(
        slot_zero.is_file() || slot_one.is_file(),
        "the repair must leave a published state slot behind"
    );
    assert!(
        paths.permanent_lock().is_file(),
        "the repair must preserve the permanent lock at {}",
        paths.permanent_lock().display()
    );
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = target.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        assert!(
            !sidecar.exists(),
            "the repaired namespace must stay sidecar-free: {} exists",
            sidecar.display()
        );
    }
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
                "clientInfo": { "name": "m17-acceptance", "version": "0" }
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

/// The rendered text of a SUCCESSFUL tool result, rejecting a protocol error or
/// an `isError` result so a failed request can never be mistaken for a graph
/// observation.
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

/// The rendered text of an `isError` tool result. A JSON-RPC protocol error or a
/// SUCCESS result is rejected: the failed open must surface as a tool error the
/// live session survives, not as a transport failure and not as a pass.
fn tool_error_text(response: &Value, context: &str) -> String {
    assert!(
        response.get("error").is_none(),
        "{context} must fail as a tool error, not a JSON-RPC error: {response}"
    );
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "{context} must return an isError result: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn failed_engine_open_is_not_cached_and_next_request_recovers() {
    // GIVEN a really indexed v2 project broken into the REAL
    // `Missing`-state-with-existing-database inconsistency, its distinguishing
    // sources frozen out of every scan, a legacy trap graph planted beside it,
    // and a separately built repaired database.
    let home = TestDir::new("served");
    let project = home.path().join("project");
    let staging = TestDir::new("staging");
    let paths = stage_missing_state_with_database(&project, &staging, "served");
    let repaired_db = build_graph_database(&staging, "repaired", REPAIRED_FILE, REPAIRED_SYMBOL);

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

    // WHEN request 1 resolves the project but FAILS opening the engine/Store on
    // the staged v2 inconsistency (its framed response IS the READY barrier) ...
    server.send(&search_frame(2, &project, REPAIRED_SYMBOL));
    let failure = tool_error_text(&server.await_response(2), "request 1");
    assert!(
        failure.contains("Failed to open project at"),
        "request 1 must fail in the ENGINE OPEN, after project resolution: {failure}"
    );
    assert!(
        failure.contains(MISSING_WITH_DB),
        "request 1 must fail for the staged v2 state/artifact reason: {failure}"
    );
    assert!(
        !failure.contains("No indexed project"),
        "request 1 must not be a project-resolution miss: {failure}"
    );

    // ... the namespace is repaired IN PLACE, with no restart: the exclusive
    // lease proves no reader lease survived the failed open, the rename proves no
    // SQLite handle did, and the fixture finalizer republishes a real Current
    // state.
    repair_without_restart(&paths, &repaired_db);

    // THEN the very next request in the SAME process succeeds and serves ONLY
    // the repaired graph, so the failed open was cached neither as an error nor
    // as a stale project result.
    server.send(&search_frame(3, &project, REPAIRED_SYMBOL));
    let recovered = tool_text(&server.await_response(3), "request 2");
    assert!(
        recovered.contains("## Search Results ("),
        "request 2 must render real search results, not an echo: {recovered}"
    );
    assert!(
        recovered.contains(REPAIRED_FILE),
        "request 2 must serve the repaired graph: {recovered}"
    );
    assert!(
        !recovered.contains(ORIGINAL_FILE),
        "request 2 must not leak the pre-repair graph: {recovered}"
    );
    assert!(
        !recovered.contains(LEGACY_TRAP_FILE),
        "request 2 must not read the legacy namespace: {recovered}"
    );

    // The pre-repair symbol is GONE from the same long-lived session, observed
    // through a lookup that resolves nothing rather than a response that merely
    // omits it.
    server.send(&search_frame(4, &project, ORIGINAL_SYMBOL));
    let original = tool_text(&server.await_response(4), "request 3");
    assert!(
        original.contains("No results found"),
        "the pre-repair symbol must be absent from the repaired graph: {original}"
    );

    // And the legacy trap graph is unreachable through the recovered session.
    server.send(&search_frame(5, &project, LEGACY_TRAP_SYMBOL));
    let trap = tool_text(&server.await_response(5), "request 4");
    assert!(
        trap.contains("No results found"),
        "the legacy trap symbol must never be reachable: {trap}"
    );

    server.shutdown();
}

/// The in-process half of the same seam: `CodeGraphEngine::open` is the exact
/// call `execute_owned` makes per request, so a failed open must leave NOTHING
/// behind in THIS process either — no memoized error, no lease, no handle — and a
/// later open in the same process must observe the repaired graph.
#[test]
fn in_process_engine_open_failure_leaves_no_cached_state_and_reopens_repaired() {
    let home = TestDir::new("inproc");
    let project = home.path().join("project");
    let staging = TestDir::new("inproc-staging");
    let paths = stage_missing_state_with_database(&project, &staging, "inproc");
    let repaired_db =
        build_graph_database(&staging, "inproc-repaired", REPAIRED_FILE, REPAIRED_SYMBOL);

    // `CodeGraphEngine` is deliberately not `Debug` (it owns a SQLite handle), so
    // the failure is taken by matching instead of `expect_err`.
    let first = match codegraph_mcp::CodeGraphEngine::open(&project) {
        Ok(_) => panic!("the staged namespace must fail the engine open"),
        Err(error) => error.to_string(),
    };
    assert!(
        first.contains(MISSING_WITH_DB),
        "the in-process failure must be the staged state/artifact refusal: {first}"
    );

    // Same fail-closed release proof, then the same legitimate repair.
    repair_without_restart(&paths, &repaired_db);

    let engine = codegraph_mcp::CodeGraphEngine::open(&project)
        .expect("the SAME process must reopen the repaired namespace");
    let result = engine.execute("codegraph_search", &json!({ "query": REPAIRED_SYMBOL }));
    assert_ne!(
        result.is_error,
        Some(true),
        "the reopened engine must serve the repaired graph"
    );
    let text = result
        .content
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(REPAIRED_FILE),
        "the reopened engine must observe the repaired file: {text}"
    );
    assert!(
        !text.contains(ORIGINAL_FILE) && !text.contains(LEGACY_TRAP_FILE),
        "the reopened engine must not surface the pre-repair or legacy graph: {text}"
    );
}

/// The startup catch-up sync cannot be the thing that repairs the staged
/// namespace, and this is a GATE rather than a race: the same
/// `sync_project_once` entry point the catch-up thread calls refuses the staged
/// `Missing`-with-database namespace with the same diagnostic and leaves the
/// database bytes and the (absent) state slots exactly as they were.
#[test]
fn catch_up_sync_refuses_missing_state_with_database_without_mutating_bytes() {
    let home = TestDir::new("catchup");
    let project = home.path().join("project");
    let staging = TestDir::new("catchup-staging");
    let paths = stage_missing_state_with_database(&project, &staging, "catchup");

    let before = fs::read(paths.current_db()).expect("read the staged database bytes");

    let error = codegraph_watch::sync_project_once(&project)
        .expect_err("a catch-up sync must refuse the staged Missing-with-database namespace")
        .to_string();
    assert!(
        error.contains(MISSING_WITH_DB),
        "the catch-up refusal must be the staged state/artifact gate: {error}"
    );

    let after = fs::read(paths.current_db()).expect("re-read the staged database bytes");
    assert_eq!(
        before, after,
        "the refused catch-up must leave the database bytes byte-identical"
    );
    for slot in paths.state_slots() {
        assert!(
            !slot.exists(),
            "the refused catch-up must not publish a state slot at {}",
            slot.display()
        );
    }
    assert!(
        !paths.tombstone().exists(),
        "the refused catch-up must not create a tombstone"
    );
    assert!(
        paths.permanent_lock().is_file(),
        "the refused catch-up must preserve the permanent lock"
    );
}

/// The recovery flow must be independent of the compatibility close seam, so no
/// CODE in this file may name it — not as a call, an import, an alias, or a
/// wrapper. Checked structurally against this file's own bytes: comment lines
/// (which document the seam by name) are excluded, and the needle itself is
/// assembled from two halves so neither this oracle nor its failure message is a
/// match for what it forbids.
#[test]
fn recovery_flow_never_calls_the_close_helper() {
    let offenders = code_lines_naming(include_str!("batch_m_failed_open_recovery.rs"), CLOSE_SEAM);
    assert!(
        offenders.is_empty(),
        "the M17 recovery flow must not depend on the {CLOSE_SEAM} compatibility seam, but these \
         code lines name it: {offenders:?}"
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
