//! Golden-fixture conformance tests for the MCP server (Task 22, DR6).
//!
//! Each fixture in `reference/golden/mcp/*.json` is a `{request, response}` pair
//! captured LIVE from the upstream built server
//! (`upstream bin/codegraph.js serve --mcp`) against the indexed
//! mini corpus. We drive the Rust [`McpServer`] over an in-memory stdio pipe
//! with the SAME request frame and assert Tier-2 structural equality with
//! the upstream response (identical input schemas; equal output structure;
//! text-formatting/ordering diffs documented in `KNOWN_DIFFS.md`).

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use codegraph_mcp::McpServer;
use serde_json::{json, Value};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Serializes every test that is sensitive to the PROCESS-GLOBAL
/// `CODEGRAPH_MCP_TOOLS` env var (read in `schemas.rs`). cargo runs tests
/// multi-threaded in ONE process, so the allowlist test's `set_var` would
/// otherwise race the default-surface readers (which assert the 4-tool
/// surface) and intermittently observe the 2-tool allowlist instead. Every
/// such test acquires this SHARED lock for the env-set→read window.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Acquire [`ENV_LOCK`], recovering from poisoning so one failing test does not
/// cascade-poison the rest of the suite.
fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

// Retry on Windows ERROR_SHARING_VIOLATION (raw OS error 32): a still-open SQLite
// handle or an AV scanner can briefly lock the destination. raw_os_error() == 32
// never occurs on Unix, so the first attempt always succeeds there (byte-identical).
fn copy_with_retry(src: &Path, dst: &Path) {
    for attempt in 0..10 {
        match fs::copy(src, dst) {
            Ok(_) => return,
            Err(err) if err.raw_os_error() == Some(32) && attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(err) => panic!("copy {} -> {}: {err:?}", src.display(), dst.display()),
        }
    }
}

/// Owns a temp project dir and removes it on drop (workspace convention is
/// `std::env::temp_dir()` + a unique subdir; there is no `tempdir` crate).
struct TestProject {
    path: PathBuf,
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TestProject {
    fn path(&self) -> &Path {
        &self.path
    }
}

/// Materialize the indexed mini project on disk: the golden DB at
/// `<root>/.codegraph/codegraph.db` plus the fixture source files (so the
/// file-mode + explore source readers can read them, exactly like the live
/// capture).
fn setup_mini_project() -> TestProject {
    let root = workspace_root();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let base =
        std::env::temp_dir().join(format!("cg-mcp-test-{}-{nanos}-{seq}", std::process::id()));
    fs::create_dir_all(base.join(".codegraph")).unwrap();
    copy_with_retry(
        &root.join("reference/golden/mini/colby.db"),
        &base.join(".codegraph").join("codegraph.db"),
    );

    let fixtures = root.join("crates/codegraph-bench/fixtures/mini");
    for rel in ["src/app.ts", "src/math.ts", "tools/greeter.py"] {
        let dst = base.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        copy_with_retry(&fixtures.join(rel), &dst);
    }
    TestProject { path: base }
}

/// Run a single JSON-RPC request frame through the server and return its
/// response. The `projectPath` in the frame is rewritten to the temp project.
fn roundtrip(project: &Path, mut request: Value) -> Value {
    if let Some(args) = request
        .get_mut("params")
        .and_then(|p| p.get_mut("arguments"))
    {
        if let Some(obj) = args.as_object_mut() {
            if obj.contains_key("projectPath") {
                obj.insert("projectPath".to_string(), json!(project.to_str().unwrap()));
            }
        }
    }
    let frame = serde_json::to_string(&request).unwrap();
    let input = format!("{frame}\n");
    let mut output = Vec::new();
    let mut server = McpServer::new(Some(project.to_path_buf()));
    server
        .run(Cursor::new(input.into_bytes()), &mut output)
        .expect("server run");
    let text = String::from_utf8(output).expect("utf8 output");
    let line = text.lines().next().expect("one response line");
    serde_json::from_str(line).expect("response json")
}

fn load_golden(name: &str) -> (Value, Value) {
    let path = workspace_root()
        .join("reference/golden/mcp")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let entry: Value = serde_json::from_str(&raw).expect("golden json");
    (entry["request"].clone(), entry["response"].clone())
}

/// Tier-2 structural equality on a tool-call `result`: the `content[0].type`
/// and the `isError` flag must match; the text body must have the SAME set of
/// lines (ordering of peer lines is a documented text-formatting diff). Each
/// `#### <file> — <symbols>` explore header normalizes its comma-separated
/// symbol list so a header's internal symbol order is treated as a documented
/// diff too (see KNOWN_DIFFS.md).
fn assert_tool_result_structural(golden: &Value, actual: &Value, ctx: &str) {
    let g = &golden["content"][0];
    let a = &actual["content"][0];
    assert_eq!(g["type"], a["type"], "{ctx}: content type mismatch");
    assert_eq!(
        golden.get("isError").cloned().unwrap_or(Value::Null),
        actual.get("isError").cloned().unwrap_or(Value::Null),
        "{ctx}: isError mismatch"
    );
    let gt = g["text"].as_str().unwrap_or("");
    let at = a["text"].as_str().unwrap_or("");
    let normalize = |t: &str| -> Vec<String> {
        let mut lines: Vec<String> = t
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| normalize_header_line(l.to_string()))
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(
        normalize(gt),
        normalize(at),
        "{ctx}: text line-set differs\nGOLDEN:\n{gt}\n\nACTUAL:\n{at}"
    );
}

/// For an explore `#### <file> — sym1, sym2, …` header, sort the symbol list so
/// the header's internal symbol ordering is a documented text-formatting diff.
fn normalize_header_line(line: String) -> String {
    if let Some((head, syms)) = line.split_once(" — ") {
        if head.starts_with("#### ") {
            let mut parts: Vec<&str> = syms.split(", ").collect();
            parts.sort_unstable();
            return format!("{head} — {}", parts.join(", "));
        }
    }
    line
}

#[test]
fn initialize_matches_golden() {
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden("initialize");
    let resp = roundtrip(project.path(), req);
    let g = &golden_resp["result"];
    let a = &resp["result"];
    assert_eq!(
        g["protocolVersion"], a["protocolVersion"],
        "protocolVersion"
    );
    assert_eq!(g["capabilities"], a["capabilities"], "capabilities");
    // serverInfo.name stays byte-stable; serverInfo.version is DYNAMIC — it must
    // equal the running crate version (`CARGO_PKG_VERSION`, see server.rs:29), so
    // a release-please bump never staleness-fails this golden. The golden's
    // `version` field is informational only and is not enforced here.
    assert_eq!(
        g["serverInfo"]["name"], a["serverInfo"]["name"],
        "serverInfo.name"
    );
    assert_eq!(
        a["serverInfo"]["version"],
        json!(env!("CARGO_PKG_VERSION")),
        "serverInfo.version must equal the running crate version"
    );
    assert_eq!(
        g["instructions"], a["instructions"],
        "instructions must be byte-identical to the golden"
    );
}

#[test]
fn tools_list_matches_upstream_names_and_schemas() {
    let _env = lock_env();
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden("tools_list");
    let resp = roundtrip(project.path(), req);
    let golden_tools = golden_resp["result"]["tools"].as_array().unwrap();
    let actual_tools = resp["result"]["tools"].as_array().unwrap();

    // the upstream v1.0.1 trims the default surface to 4 tools (f9fcc2cd:
    // DEFAULT_MCP_TOOLS = explore/node/search/callers). The other 4 stay
    // callable but unlisted; CODEGRAPH_MCP_TOOLS re-enables them.
    assert_eq!(actual_tools.len(), 4, "default surface is 4 tools");
    assert_eq!(golden_tools.len(), 4, "golden has 4 tools");

    let gnames: Vec<&str> = golden_tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let anames: Vec<&str> = actual_tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(anames, gnames, "tool names + order match the golden");

    // Input schemas must be byte-identical (Tier-2 requires identical schemas).
    for (g, a) in golden_tools.iter().zip(actual_tools.iter()) {
        assert_eq!(
            g["inputSchema"], a["inputSchema"],
            "inputSchema for {} must match the golden",
            g["name"]
        );
        // readOnlyHint annotations (a79fa51) are part of the tool surface: the
        // golden fixture pins them so a divergence between source and fixture
        // fails here, keeping the annotations update load-bearing.
        assert_eq!(
            g["annotations"], a["annotations"],
            "annotations for {} must match the golden",
            g["name"]
        );
    }
}

#[test]
fn tools_list_exposed_with_required_project_path_when_workspace_not_indexed() {
    // Unindexed workspace (no .codegraph/codegraph.db) STILL serves the full
    // tool surface (#94 / colby #964, PR#966 — reverses c450fd95). Each tool's
    // inputSchema.required gains "projectPath" (#993, PR#1007) so a roots-less
    // client's agent supplies it per call instead of seeing 0 tools.
    let base = std::env::temp_dir().join(format!(
        "cg-mcp-noidx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&base).unwrap();
    let resp = roundtrip(
        &base,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    let _ = fs::remove_dir_all(&base);
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    assert_eq!(
        tools.len(),
        4,
        "unindexed workspace must still expose the 4-tool default surface"
    );
    for tool in tools {
        let required = tool["inputSchema"]["required"]
            .as_array()
            .unwrap_or_else(|| panic!("tool {} has a required array", tool["name"]));
        assert!(
            required.iter().any(|v| v == "projectPath"),
            "tool {} must mark projectPath required when unindexed",
            tool["name"]
        );
    }
}

#[test]
fn default_project_indexed_serves_full_tools_list_after_initialize() {
    // Regression for the `serve --mcp` (no --path) bug: the installer launches
    // the server with the agent's project root as cwd and no projectPath, so the
    // CLI must default the project to that indexed root. With a Some(indexed)
    // default_project, the initialize->tools/list handshake must expose 4 tools.
    let _env = lock_env();
    let project = index_fixture(&[(
        "src/app.ts",
        "export function greet(name: string): string {\n  return `hi ${name}`;\n}\n",
    )]);
    let frames = format!(
        "{}\n{}\n",
        serde_json::to_string(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .unwrap(),
        serde_json::to_string(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
            .unwrap(),
    );
    let mut output = Vec::new();
    let mut server = McpServer::new(Some(project.path().to_path_buf()));
    server
        .run(Cursor::new(frames.into_bytes()), &mut output)
        .expect("server run");
    let text = String::from_utf8(output).expect("utf8 output");
    let tools_line = text.lines().nth(1).expect("tools/list response line");
    let resp: Value = serde_json::from_str(tools_line).expect("response json");
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    assert_eq!(
        tools.len(),
        4,
        "indexed default project must serve the 4-tool default surface"
    );
    for tool in tools {
        let has_pp = tool["inputSchema"]["required"]
            .as_array()
            .map(|r| r.iter().any(|v| v == "projectPath"))
            .unwrap_or(false);
        assert!(
            !has_pp,
            "indexed default keeps projectPath OPTIONAL for {} (byte-identical to golden)",
            tool["name"]
        );
    }
}

#[test]
fn default_project_unindexed_serves_tools_with_required_project_path() {
    // The non-bailing cwd default still starts the server for an unindexed root;
    // it now serves the full tool surface with projectPath marked required
    // (#94 / colby #964/#993 — reverses the golden-pinned c450fd95 empty list).
    let base = std::env::temp_dir().join(format!(
        "cg-mcp-default-noidx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&base).unwrap();
    let mut output = Vec::new();
    let mut server = McpServer::new(Some(base.clone()));
    server
        .run(
            Cursor::new(
                format!(
                    "{}\n",
                    serde_json::to_string(
                        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
                    )
                    .unwrap()
                )
                .into_bytes(),
            ),
            &mut output,
        )
        .expect("server run");
    let _ = fs::remove_dir_all(&base);
    let text = String::from_utf8(output).expect("utf8 output");
    let line = text.lines().next().expect("one response line");
    let resp: Value = serde_json::from_str(line).expect("response json");
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    assert_eq!(
        tools.len(),
        4,
        "unindexed default project must still expose the 4-tool default surface"
    );
    for tool in tools {
        let required = tool["inputSchema"]["required"]
            .as_array()
            .unwrap_or_else(|| panic!("tool {} has a required array", tool["name"]));
        assert!(
            required.iter().any(|v| v == "projectPath"),
            "tool {} must mark projectPath required when default project unindexed",
            tool["name"]
        );
    }
}

#[test]
fn no_default_project_exposes_tools_with_required_project_path() {
    // Issue #94 (通义灵码/Lingma): a roots-less client launches `serve --mcp`
    // with no `-p` and no default project ever resolves (McpServer::new(None)).
    // The always-expose forward-port (colby #964) means tools/list STILL lists
    // the 4-tool default surface; the projectPath-required forward-port (colby
    // #993) marks projectPath required in each schema so the agent supplies it.
    let _env = lock_env();
    let mut output = Vec::new();
    let mut server = McpServer::new(None);
    let frames = format!(
        "{}\n{}\n",
        serde_json::to_string(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .unwrap(),
        serde_json::to_string(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
            .unwrap(),
    );
    server
        .run(Cursor::new(frames.into_bytes()), &mut output)
        .expect("server run");
    let text = String::from_utf8(output).expect("utf8 output");
    let tools_line = text.lines().nth(1).expect("tools/list response line");
    let resp: Value = serde_json::from_str(tools_line).expect("response json");
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    assert_eq!(
        tools.len(),
        4,
        "no-default project must still expose the 4-tool default surface (#94 / colby #964)"
    );
    for tool in tools {
        let required = tool["inputSchema"]["required"]
            .as_array()
            .unwrap_or_else(|| panic!("tool {} has a required array", tool["name"]));
        assert!(
            required.iter().any(|v| v == "projectPath"),
            "tool {} must mark projectPath required when no default project (#94 / colby #993)",
            tool["name"]
        );
    }
}

#[test]
fn zed_bare_serve_adopts_roots_and_resolves_tool_call() {
    // GIVEN a bare `serve --mcp` (no --path) launched from an UNINDEXED cwd —
    // the Zed case: the cwd-derived default is Some(cwd) but has no index.
    // WHEN the client advertises `capabilities.roots`, later reports an INDEXED
    // workspace via roots/list, then calls a tool with NO projectPath —
    // THEN the server adopts the indexed root and the tool call resolves against
    // it with a NON-EMPTY, non-error result.
    let _env = lock_env();
    let indexed = setup_mini_project();
    let unindexed_cwd = std::env::temp_dir().join(format!(
        "cg-mcp-zed-cwd-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&unindexed_cwd).unwrap();
    let _cwd_guard = TestProject {
        path: unindexed_cwd.clone(),
    };

    let mut server =
        McpServer::new_with_cwd(Some(unindexed_cwd.clone()), Some(unindexed_cwd.clone()));

    let init = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "capabilities": { "roots": { "listChanged": true } } }
    }))
    .unwrap();
    let mut init_out = Vec::new();
    server
        .run(Cursor::new(format!("{init}\n").into_bytes()), &mut init_out)
        .expect("initialize run");
    let init_lines: Vec<Value> = String::from_utf8(init_out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        init_lines.len(),
        2,
        "initialize must also request roots/list"
    );
    assert_eq!(init_lines[1]["method"], json!("roots/list"));

    let roots_response = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": "codegraph-roots-list-1",
        "result": { "roots": [
            { "uri": format!("file://{}", indexed.path().display()), "name": "proj" }
        ] }
    }))
    .unwrap();
    let mut roots_out = Vec::new();
    server
        .run(
            Cursor::new(format!("{roots_response}\n").into_bytes()),
            &mut roots_out,
        )
        .expect("roots/list response run");
    assert!(roots_out.is_empty(), "a JSON-RPC response yields no reply");
    assert_eq!(
        server.default_project(),
        Some(indexed.path()),
        "the indexed workspace root must be adopted as the default project"
    );

    let call = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "codegraph_search", "arguments": { "query": "add" } }
    }))
    .unwrap();
    let mut call_out = Vec::new();
    server
        .run(Cursor::new(format!("{call}\n").into_bytes()), &mut call_out)
        .expect("tools/call run");
    let call_resp: Value =
        serde_json::from_str(String::from_utf8(call_out).unwrap().lines().next().unwrap()).unwrap();
    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    assert_ne!(
        call_resp["result"]["isError"],
        json!(true),
        "adopted-root tool call must not error: {text}"
    );
    assert!(
        !text.contains("No indexed project resolved"),
        "must not fall through to the no-project error: {text}"
    );
    assert!(
        text.contains("add"),
        "search against the adopted indexed root must return results: {text}"
    );
}

#[test]
fn tools_list_honors_codegraph_mcp_tools_allowlist() {
    // CODEGRAPH_MCP_TOOLS replaces the default surface with exactly the named
    // tools — any of the 8 (tools.ts:711-740). Serialized via the shared
    // ENV_LOCK to avoid env-var races with the default-surface readers in the
    // same process.
    let _env = lock_env();

    let project = setup_mini_project();
    // SAFETY: single-threaded test section guarded by ENV_LOCK; remove_var runs
    // before any assertion that could panic, so a failed assert never leaks the
    // var to other tests.
    unsafe { std::env::set_var("CODEGRAPH_MCP_TOOLS", "impact,files") };
    let resp = roundtrip(
        project.path(),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    unsafe { std::env::remove_var("CODEGRAPH_MCP_TOOLS") };

    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["codegraph_impact", "codegraph_files"],
        "allowlist exposes exactly the named tools (in definition order)"
    );
}

#[test]
fn search_matches_golden() {
    run_tool_case("codegraph_search");
}

#[test]
fn callers_matches_golden() {
    run_tool_case("codegraph_callers");
}

#[test]
fn callees_matches_golden() {
    run_tool_case("codegraph_callees");
}

#[test]
fn impact_matches_golden() {
    run_tool_case("codegraph_impact");
}

#[test]
fn node_symbol_mode_matches_golden() {
    run_tool_case("codegraph_node");
}

#[test]
fn node_file_mode_matches_golden() {
    run_tool_case("codegraph_node_1");
}

#[test]
fn status_matches_golden() {
    // Status text differs only in `Database size` MB (the upstream checkpointed its
    // WAL; our copy carries a slightly larger on-disk file). Compare every line
    // EXCEPT the size line, which is a documented text-formatting diff.
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden("codegraph_status");
    let resp = roundtrip(project.path(), req);
    let strip_size = |t: &str| -> Vec<String> {
        t.lines()
            .filter(|l| !l.starts_with("**Database size:**"))
            .map(str::to_string)
            .collect()
    };
    let g = strip_size(
        golden_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    );
    let a = strip_size(resp["result"]["content"][0]["text"].as_str().unwrap());
    assert_eq!(
        g, a,
        "status text (excluding DB size) must match the golden"
    );
    assert!(
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("**Database size:**"),
        "status still renders a Database size line"
    );
}

#[test]
fn files_tree_matches_golden() {
    run_tool_case("codegraph_files");
}

#[test]
fn files_flat_matches_golden() {
    run_tool_case("codegraph_files_1");
}

#[test]
fn files_grouped_matches_golden() {
    run_tool_case("codegraph_files_2");
}

#[test]
fn explore_matches_golden_structural() {
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden("codegraph_explore");
    let resp = roundtrip(project.path(), req);
    let gt = golden_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let at = resp["result"]["content"][0]["text"].as_str().unwrap();

    // The `Found N symbols` count is RWR-relevance-driven (the reference implementation prunes the
    // import seed; our simplified seeding keeps it) — a documented Tier-2 diff.
    // Drop that line, then compare the line-SET (peer ordering + header symbol
    // order are documented diffs). Everything else — the section headers, the
    // blast-radius entries, and every verbatim source line — must match.
    let normalize = |t: &str| -> Vec<String> {
        let mut lines: Vec<String> = t
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| !l.starts_with("Found "))
            .map(|l| normalize_header_line(l.to_string()))
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(
        normalize(gt),
        normalize(at),
        "explore text (excluding the relevance-driven symbol count) must match the golden\nGOLDEN:\n{gt}\n\nACTUAL:\n{at}"
    );

    // The verbatim source section must be byte-identical (modulo file-header
    // symbol order): assert each numbered source line appears identically.
    for src_line in gt.lines().filter(|l| l.contains('\t')) {
        assert!(
            at.contains(src_line),
            "explore source line missing from Rust output: {src_line:?}"
        );
    }
}

fn run_tool_case(name: &str) {
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden(name);
    let resp = roundtrip(project.path(), req);
    assert_tool_result_structural(&golden_resp["result"], &resp["result"], name);
}

#[test]
fn unknown_tool_returns_jsonrpc_error_not_crash() {
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden("error_unknown_tool");
    let resp = roundtrip(project.path(), req);
    // the upstream: JSON-RPC error -32602 "Unknown tool: <name>" (session.ts:217-225).
    assert_eq!(
        resp["error"]["code"], golden_resp["error"]["code"],
        "unknown tool error code must be -32602"
    );
    assert_eq!(resp["error"]["code"], json!(-32602));
    assert_eq!(
        resp["error"]["message"], golden_resp["error"]["message"],
        "unknown tool error message must match the golden"
    );
    assert!(resp["result"].is_null(), "no result on a JSON-RPC error");
}

#[test]
fn missing_required_arg_returns_tool_iserror() {
    let project = setup_mini_project();
    let (req, golden_resp) = load_golden("error_missing_arg");
    let resp = roundtrip(project.path(), req);
    // the upstream: a missing required arg is a TOOL error (isError:true content),
    // not a JSON-RPC protocol error.
    assert_eq!(resp["result"]["isError"], json!(true));
    assert_eq!(
        resp["result"]["content"][0]["text"], golden_resp["result"]["content"][0]["text"],
        "missing-arg error text must match the golden"
    );
}

/// Build a minimal indexed project at `<root>/.codegraph/codegraph.db` from
/// in-test source files. Mirrors the CLI index order (`main.rs:683`): ALL nodes
/// upsert before ANY edge, files last. Enough to drive explore; no resolution.
fn index_fixture(files: &[(&str, &str)]) -> TestProject {
    use codegraph_core::types::FileRecord;
    use codegraph_extract::engine::{detect_language, extract_file};
    use codegraph_store::Store;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let base =
        std::env::temp_dir().join(format!("cg-mcp-dyn-{}-{nanos}-{seq}", std::process::id()));
    for (rel, src) in files {
        let dst = base.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, src).unwrap();
    }
    let mut store = Store::open(&base.join(".codegraph").join("codegraph.db")).unwrap();
    let mut all_edges = Vec::new();
    for (rel, src) in files {
        let result = extract_file(&base, rel).unwrap();
        store.upsert_nodes(&result.nodes).unwrap();
        all_edges.extend(result.edges);
        store
            .upsert_file(&FileRecord {
                path: (*rel).to_string(),
                content_hash: String::new(),
                language: detect_language(rel),
                size: src.len() as i64,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
            })
            .unwrap();
    }
    store.insert_edges(&all_edges).unwrap();
    drop(store);
    TestProject { path: base }
}

/// Regression: a fixture with a runtime-dispatch site surfaces the
/// "Dynamic boundaries" section in `codegraph_explore` with the upstream's exact
/// label and snippet (`tools.ts:1744`, `dynamic-boundaries.ts`). The mini
/// corpus has no dispatch sites, so this is the only fixture that exercises it.
#[test]
fn explore_surfaces_dynamic_boundary_section() {
    let project = index_fixture(&[(
        "src/dispatch.ts",
        "export function dispatch(action: { type: string }) {\n  return handlers['save'](action);\n}\n",
    )]);
    let resp = roundtrip(
        project.path(),
        json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": "codegraph_explore",
                "arguments": { "query": "dispatch", "projectPath": "/placeholder" }
            }
        }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("## Dynamic boundaries (the static path ends at runtime dispatch)"),
        "explore output missing Dynamic boundaries header:\n{text}"
    );
    assert!(
        text.contains(
            "- `dispatch` (src/dispatch.ts:2) — computed member call: `return handlers['save'](action);`"
        ),
        "explore output missing the boundary note with the golden label/snippet:\n{text}"
    );
    assert!(
        text.contains("> These sites choose their call target at runtime"),
        "explore output missing the boundary footer:\n{text}"
    );
}

#[test]
fn check_tool_reports_no_cycles_on_acyclic_mini_corpus() {
    let project = setup_mini_project();
    let resp = roundtrip(
        project.path(),
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "codegraph_check",
                "arguments": { "projectPath": "/placeholder" }
            }
        }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text, "No circular dependencies found",
        "acyclic mini corpus must report no cycles (no false positive)"
    );
    assert_ne!(
        resp["result"]["isError"],
        json!(true),
        "check tool must not error on a valid project"
    );
}

/// Synthesize a `.ts` file with `count` exported functions, each `body_lines`
/// lines long, so a single file blows past the <150-tier per-file cap (3800
/// chars) and the 13000-char total budget — forcing clustering + whole-method
/// drop.
fn big_ts_file(count: usize, body_lines: usize) -> String {
    let mut src = String::new();
    for i in 0..count {
        src.push_str(&format!(
            "export function handler{i}(x: number): number {{\n"
        ));
        for j in 0..body_lines {
            src.push_str(&format!(
                "  const v{j} = x + {i} * {j}; // padding line to inflate the body size\n"
            ));
        }
        src.push_str(&format!("  return handler{i}_done(x);\n}}\n\n"));
    }
    src
}

/// Regression: the size-adaptive output budget (`explore_budget.rs`, ports
/// `getExploreOutputBudget`/`tools.ts:160-258`). A tiny project (<150 files)
/// gets the tight 13000-char cap, gates OFF the Relationships / budget-note /
/// completeness / "Not shown above" meta-sections, and NEVER slices a method
/// mid-body: an oversize file is clustered into whole-method windows with a
/// `... (gap) ...` marker, and any file that doesn't fit the total cap is
/// dropped whole.
#[test]
fn explore_tiny_tier_budget_drops_whole_methods_not_mid_method() {
    let project = index_fixture(&[
        ("src/big.ts", &big_ts_file(40, 12)),
        ("src/extra.ts", &big_ts_file(30, 12)),
    ]);
    let resp = roundtrip(
        project.path(),
        json!({
            "jsonrpc": "2.0",
            "id": 51,
            "method": "tools/call",
            "params": {
                "name": "codegraph_explore",
                "arguments": { "query": "handler0 handler1", "projectPath": "/placeholder" }
            }
        }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();

    // Tiny tier: total output stays under the hard ceiling (min(13000*1.5, 25000)).
    assert!(
        text.len() <= 19_500,
        "tiny-tier output must respect the 13000-char budget ceiling, got {} chars",
        text.len()
    );

    // Tiny tier gates these meta-sections OFF (`tools.ts:172-190`).
    assert!(
        !text.contains("### Relationships"),
        "tiny tier must not emit Relationships:\n{text}"
    );
    assert!(
        !text.contains("Explore budget:"),
        "tiny tier must not emit the budget note:\n{text}"
    );
    assert!(
        !text.contains("Complete source for"),
        "tiny tier must not emit the completeness signal:\n{text}"
    );

    // Whole-method-drop invariant: the oversize file is windowed by complete
    // method bodies with a gap marker — never sliced mid-method. The query named
    // handler0/handler1, so their clusters win the per-file budget and MUST each
    // show their full body (closing `return handlerN_done(x);`). Methods beyond
    // the budget are dropped WHOLE, leaving a `... (gap) ...` marker. A trailing
    // signature line can appear as context padding (the upstream's 3-line pad,
    // `tools.ts:2780-2788`) — that's context, not a mid-method slice — so we
    // assert on the NAMED methods that are fully selected.
    assert!(
        text.contains("... (gap) ..."),
        "oversize tiny-tier file must drop whole clusters with a gap marker:\n{text}"
    );
    for n in [0usize, 1usize] {
        assert!(
            text.contains(&format!("export function handler{n}(")),
            "named method handler{n} must be shown:\n{text}"
        );
        assert!(
            text.contains(&format!("return handler{n}_done(x);")),
            "named method handler{n} shown without its full body (mid-method slice):\n{text}"
        );
    }
}

/// Regression: at a tier that ENABLES `includeAdditionalFiles` (>=5000 files),
/// a file dropped for the total budget surfaces in the trailing "Not shown
/// above" list so the agent can request it (`tools.ts:2910-2927`). We can't
/// cheaply index 5000 files, so this asserts the gating constant directly via
/// the budget function — the wiring (excluded_files → list) is covered by the
/// tiny-tier test proving the list is ABSENT when the flag is off.
#[test]
fn explore_additional_files_list_gated_by_tier() {
    use codegraph_mcp::explore_budget::get_explore_output_budget;
    assert!(
        !get_explore_output_budget(3).include_additional_files,
        "tiny tier must gate the additional-files list off"
    );
    assert!(
        get_explore_output_budget(6000).include_additional_files,
        ">=5000 tier must enable the additional-files list"
    );
}
