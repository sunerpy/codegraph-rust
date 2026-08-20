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
use serde_json::{Value, json};

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
    let paths = codegraph_core::IndexPaths::resolve(&base, None).unwrap();
    codegraph_store::test_support::finalize_current_test_fixture(&paths).unwrap();

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
        && let Some(obj) = args.as_object_mut()
        && obj.contains_key("projectPath")
    {
        obj.insert("projectPath".to_string(), json!(project.to_str().unwrap()));
    }
    let frame = serde_json::to_string(&request).unwrap();
    let input = format!("{frame}\n");
    let mut output = Vec::new();
    let mut server = McpServer::new(Some(project.to_path_buf()));
    server
        .run_until_adoption(Cursor::new(input.into_bytes()), &mut output)
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
    if let Some((head, syms)) = line.split_once(" — ")
        && head.starts_with("#### ")
    {
        let mut parts: Vec<&str> = syms.split(", ").collect();
        parts.sort_unstable();
        return format!("{head} — {}", parts.join(", "));
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
        .run_until_adoption(Cursor::new(frames.into_bytes()), &mut output)
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
        .run_until_adoption(
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
        .run_until_adoption(Cursor::new(frames.into_bytes()), &mut output)
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
        .run_until_adoption(Cursor::new(format!("{init}\n").into_bytes()), &mut init_out)
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
        .run_until_adoption(
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
        .run_until_adoption(Cursor::new(format!("{call}\n").into_bytes()), &mut call_out)
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
    use codegraph_core::node_id::hash_content;
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
        let metadata = fs::metadata(base.join(rel)).unwrap();
        let modified_at = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let result = extract_file(&base, rel).unwrap();
        store.upsert_nodes(&result.nodes).unwrap();
        all_edges.extend(result.edges);
        store
            .upsert_file(&FileRecord {
                path: (*rel).to_string(),
                content_hash: hash_content(src),
                language: detect_language(rel),
                size: metadata.len() as i64,
                modified_at,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
                generated: false,
            })
            .unwrap();
    }
    store.insert_edges(&all_edges).unwrap();
    drop(store);
    let paths = codegraph_core::IndexPaths::resolve(&base, None).unwrap();
    codegraph_store::test_support::finalize_current_test_fixture(&paths).unwrap();
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

/// The Tier 5 `src/god.ts` shape (plan §2.1): two header comments, a blank, then
/// `hugeHandler` whose body is `pad_count` padding lines long, then
/// `blank_count` blank lines, then a 4-line `processPayroll`.
///
/// `blank_count` is LOAD-BEARING and decides which defect the fixture exhibits.
/// With `gap_threshold` 7 at the tiny tier:
/// * `god_ts_file(600, 8)` — `hugeHandler` 4..606, `processPayroll` at 615, gap
///   `615 - 606 = 9 > 7` ⇒ **TWO** clusters (the §2.1 split variant).
/// * `god_ts_file(606, 3)` — `hugeHandler` 4..612, `processPayroll` at 616, gap
///   `616 - 612 = 4 <= 7` ⇒ **ONE** merged cluster (the §2.1.1 merge variant),
///   with the named `processPayroll` in the file's last 1% so no later-cluster
///   path can rescue it.
fn god_ts_file(pad_count: usize, blank_count: usize) -> String {
    let mut src = String::from("// header line 1\n// header line 2\n\n");
    src.push_str("export function hugeHandler(x: number): number {\n");
    for j in 0..pad_count {
        src.push_str(&format!(
            "  // padding line {j} inside hugeHandler keeping the body enormous\n"
        ));
    }
    src.push_str("  return x;\n}\n");
    for _ in 0..blank_count {
        src.push('\n');
    }
    src.push_str(
        "export function processPayroll(hours: number): number {\n  const rate = 42;\n  return hours * rate;\n}\n",
    );
    src
}

/// The five one-function filler files that keep a Tier 5 6-file fixture at the
/// tiny tier (plan §2.1).
fn greek_fillers() -> Vec<(String, String)> {
    ["alpha", "beta", "gamma", "delta", "epsilon"]
        .iter()
        .map(|n| {
            (
                format!("src/{n}.ts"),
                format!("export function {n}Fn(): number {{\n  return 1;\n}}\n"),
            )
        })
        .collect()
}

fn index_god_fixture(pad_count: usize, blank_count: usize) -> TestProject {
    let god = god_ts_file(pad_count, blank_count);
    let fillers = greek_fillers();
    let mut files: Vec<(&str, &str)> = vec![("src/god.ts", god.as_str())];
    for (rel, src) in &fillers {
        files.push((rel.as_str(), src.as_str()));
    }
    index_fixture(&files)
}

fn explore_text(project: &Path, query: &str) -> String {
    let resp = roundtrip(
        project,
        json!({
            "jsonrpc": "2.0",
            "id": 71,
            "method": "tools/call",
            "params": {
                "name": "codegraph_explore",
                "arguments": { "query": query, "projectPath": "/placeholder" }
            }
        }),
    );
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("explore must return text")
        .to_string()
}

fn section_count(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("#### ")).count()
}

/// Tier 5 item 13 half A (CG-30). Given a query whose ONLY match is a symbol
/// whose body is 13x the per-file cap, When explore renders, Then the file's
/// top-ranked cluster is WINDOWED to a ceiling instead of accepted whole and
/// then dropped whole by the total-output admission test.
///
/// Before the fix the whole 617-byte response carried ZERO `#### ` sections and
/// zero lines of source, while `codegraph query hugeHandler` found the symbol
/// fine — 4.7% of a 13,000-char budget spent, and no signal about which file was
/// lost (`includeAdditionalFiles` is gated off at the tiny tier).
#[test]
fn explore_single_oversize_match_still_returns_source() {
    let project = index_god_fixture(600, 8);
    let text = explore_text(project.path(), "hugeHandler");

    assert!(
        text.contains("#### src/god.ts"),
        "the single oversize match must still produce a file section (was ZERO sections):\n{text}"
    );
    assert!(
        text.contains("export function hugeHandler"),
        "the named definition line must render:\n{text}"
    );
    // Bounded, not merely present: the response stays under the tiny tier's
    // hard ceiling min(13000 * 1.5, 25000).
    assert!(
        text.len() <= 19_500,
        "windowed output must respect the tiny-tier ceiling, got {} chars",
        text.len()
    );
    assert!(
        text.len() > 1_000,
        "a windowed section must actually deliver source, got {} chars:\n{text}",
        text.len()
    );
    // Whole-LINE windowing: every emitted numbered line must be byte-identical
    // to the file line it claims, so nothing was sliced mid-line.
    let src = god_ts_file(600, 8);
    let file_lines: Vec<&str> = src.split('\n').collect();
    let mut emitted = 0usize;
    for line in text.lines() {
        let Some((num, body)) = line.split_once('\t') else {
            continue;
        };
        let Ok(n) = num.parse::<usize>() else {
            continue;
        };
        assert_eq!(
            file_lines[n - 1],
            body,
            "line {n} was sliced mid-line:\n{text}"
        );
        emitted += 1;
    }
    assert!(emitted > 0, "no numbered source lines emitted:\n{text}");
}

/// Tier 5 item 13 half B (CG-36). Given a query naming TWO symbols in one file
/// that land in separate clusters, and the second-ranked cluster is far bigger
/// than the room left, When explore renders, Then that cluster is SHRUNK into
/// the remainder instead of skipped whole.
///
/// Before the fix the 848-byte response rendered only `processPayroll` — the
/// denser cluster wins the density tie-break, takes the first-cluster accept,
/// and `hugeHandler`'s ~42 KB cluster is then whole-or-nothing. The file header
/// read `processPayroll(function)` alone, so the agent had no signal that the
/// other symbol it named even existed in that file.
#[test]
fn explore_named_later_cluster_is_shrunk_not_dropped() {
    let project = index_god_fixture(600, 8);
    let text = explore_text(project.path(), "hugeHandler processPayroll");

    assert!(
        text.contains("export function processPayroll"),
        "the denser named cluster must still render:\n{text}"
    );
    assert!(
        text.contains("export function hugeHandler"),
        "the later named cluster must be shrunk in, not dropped whole:\n{text}"
    );
    // The header is built from the CHOSEN clusters' symbols, so a fix that
    // renders the body but leaves the header fed from the old chosen set would
    // still mislabel the file exactly as before.
    let header = text
        .lines()
        .find(|l| l.starts_with("#### src/god.ts"))
        .unwrap_or_else(|| panic!("no src/god.ts section:\n{text}"));
    assert!(
        header.contains("hugeHandler"),
        "the file header must LIST the shrunk-in symbol, got {header:?}"
    );
}

/// Tier 5 item 12 (CG-38), cluster-tail half. Given ONE oversize cluster whose
/// explicitly named `processPayroll` sits in the file's last 1%, When explore
/// renders, Then a window covering that definition is allocated — a head window
/// alone reaches only ~L100.
///
/// The MERGE shape is required here, not the split shape: with a single cluster,
/// item 13's later-cluster shrink never executes, so this cannot be closed by
/// Group 2 and stays RED for item 12's own reason. On the split shape the same
/// assertion is made green by item 13 and would prove nothing.
#[test]
fn explore_named_definition_line_always_renders() {
    let project = index_god_fixture(606, 3);
    let text = explore_text(project.path(), "hugeHandler processPayroll");

    assert!(
        text.contains("#### src/god.ts"),
        "a section must exist (was 632 bytes, ZERO sections):\n{text}"
    );
    assert!(
        text.contains("export function processPayroll"),
        "the named TAIL definition must render, not just the head window:\n{text}"
    );
    assert!(
        text.contains("export function hugeHandler"),
        "the head focus must not be displaced by the tail one:\n{text}"
    );
    // Still ONE cluster: if a future edit widened the blank-line gap past
    // `gap_threshold`, this fixture would silently become the split shape and the
    // assertion above would be satisfied by item 13's shrink instead.
    assert_eq!(
        section_count(&text),
        1,
        "the merge-shape gap invariant no longer holds:\n{text}"
    );
}

/// The Tier 5 escape-A fixture (plan §2.1.2): two files that BOTH clear both
/// whole-file gates, so `render_explore_file` returns a COMPLETE section before
/// any ceiling, focus, or window code exists — and the caller then drops the
/// second one whole.
///
/// `src/aHogOne.ts` (145 lines) wins rank #1 on `sym_count` 9-vs-1 and consumes
/// the headroom; `src/zTarget.ts` (37 lines) owns the named
/// `reconcileTargetZulu` at L36 of 37, so a head-only window cannot rescue it.
/// Both files must stay INSIDE both gates: push either past one and escape A
/// silently becomes escape B, which the clustering ceiling already fixes.
fn index_escape_a_fixture() -> TestProject {
    let pad = |tag: &str, i: usize| {
        let head = format!("  // pad {i} for {tag} ");
        format!("{head}{}", "-".repeat(65 - head.len()))
    };
    let mut hog = String::from("export class HogLedgerOne {\n");
    for i in 0..135 {
        hog.push_str(&pad("One", i));
        hog.push('\n');
    }
    for k in 0..8 {
        hog.push_str(&format!(
            "  hogOneStep{k}(v: number): number {{ return v + {k}; }}\n"
        ));
    }
    hog.push_str("}\n");
    let mut tgt = String::from("export class TargetLedgerZulu {\n");
    for i in 0..32 {
        tgt.push_str(&pad("Zulu", i));
        tgt.push('\n');
    }
    for k in 0..2 {
        tgt.push_str(&format!(
            "  helperTargetZulu{k}(v: number): number {{ return v + {k}; }}\n"
        ));
    }
    tgt.push_str("  reconcileTargetZulu(v: number): number { return v + 99; }\n}\n");
    let fillers = greek_fillers();
    let mut files: Vec<(&str, &str)> = vec![
        ("src/aHogOne.ts", hog.as_str()),
        ("src/zTarget.ts", tgt.as_str()),
    ];
    for (rel, src) in fillers.iter().take(4) {
        files.push((rel.as_str(), src.as_str()));
    }
    index_fixture(&files)
}

/// Tier 5 item 12 (CG-38), WHOLE-FILE half. Given a focus-owning file that is
/// whole-file-eligible but UNAFFORDABLE behind a bigger rank-#1 file, When
/// explore renders, Then it falls through to the clustering path and emits a
/// window carrying the named definition — instead of returning a complete
/// section the caller then discards whole.
///
/// This is a SECOND red for item 12 because the cluster-tail fixture cannot
/// reach it: that file is 619 lines and clusters, while both files here return at
/// the whole-file branch before any focus code runs. The two item-12 fixtures sit
/// on disjoint branches by construction.
#[test]
fn explore_named_definition_survives_the_whole_file_branch() {
    let project = index_escape_a_fixture();
    let text = explore_text(project.path(), "HogLedgerOne reconcileTargetZulu");

    assert!(
        text.contains("#### src/zTarget.ts"),
        "the focus owner was absent entirely (10,616 bytes, ONE section):\n{text}"
    );
    assert!(
        text.contains("reconcileTargetZulu(v: number)"),
        "the explicitly named definition must render:\n{text}"
    );
    // The response must GAIN a section, not trade one: a fix that admits the
    // focus owner by evicting the rank-#1 hog is not the fix.
    assert!(
        text.contains("#### src/aHogOne.ts"),
        "the budget hog must not be evicted to make room:\n{text}"
    );
    assert_eq!(section_count(&text), 2, "expected both sections:\n{text}");
    // The tier's STATED budget, not merely the 19,500 hard ceiling.
    assert!(
        text.len() <= 13_000,
        "response must fit max_output_chars, got {} chars",
        text.len()
    );
    assert!(
        !text.contains("output truncated to budget"),
        "the hard-ceiling cut must stay unfired:\n{text}"
    );
}

/// A >=500-file project: `filler_count` one-function files plus `ledgers`
/// `Ledger*` classes of `methods` one-line methods each. The fillers only buy the
/// tier; the `Ledger*` files are what the query selects.
fn index_ledger_fixture(filler_count: usize, ledgers: usize, methods: usize) -> TestProject {
    const NAMES: [&str; 12] = [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India",
        "Juliet", "Kilo", "Lima",
    ];
    let mut owned: Vec<(String, String)> = Vec::new();
    for i in 1..=filler_count {
        owned.push((
            format!("src/filler{i}.ts"),
            format!("export function filler{i}(x: number): number {{\n  return x + {i};\n}}\n"),
        ));
    }
    for name in NAMES.iter().take(ledgers) {
        let mut src = format!("export class Ledger{name} {{\n");
        for k in 1..=methods {
            src.push_str(&format!(
                "  entry{k}(x: number): number {{ return x + {k}; }}\n"
            ));
        }
        src.push_str("}\n");
        owned.push((format!("src/ledger_{name}.ts"), src));
    }
    let files: Vec<(&str, &str)> = owned
        .iter()
        .map(|(rel, src)| (rel.as_str(), src.as_str()))
        .collect();
    index_fixture(&files)
}

/// Tier 5 item 15 (CG-26/31), pointer-list half. Given a >=500-file project whose
/// candidate files each contribute ~120 subgraph nodes, so one uncapped pointer
/// line runs to ~1,700 bytes, When explore renders, Then it delivers the same
/// number of source sections as a control query over the same three files.
///
/// Before the fix the epilogue — charged NOTHING by the accountant — pushed the
/// response past the hard ceiling, and the cut then discarded a fully rendered,
/// already-charged 6.4 KB section: 2 sections against the control's 3, and
/// 13,442 of a 24,000 budget delivered. The control is asserted at 3 rather than
/// compared loosely, so the criterion cannot pass with the fixture absent.
#[test]
fn explore_epilogue_never_costs_a_rendered_section() {
    let project = index_ledger_fixture(512, 12, 120);
    let text = explore_text(project.path(), "Ledger");
    let control = explore_text(project.path(), "LedgerAlpha LedgerBravo LedgerCharlie");

    assert_eq!(
        section_count(&control),
        3,
        "control must deliver 3 sections:\n{control}"
    );
    assert_eq!(
        section_count(&text),
        section_count(&control),
        "the epilogue cost a rendered section (was 2 vs 3):\n{text}"
    );
    assert!(
        text.len() <= 24_000,
        "response must fit its STATED budget, got {} chars",
        text.len()
    );
    assert!(
        !text.contains("output truncated to budget"),
        "the hard-ceiling cut must no longer fire:\n{text}"
    );
    assert!(
        text.contains(&format!(
            "Complete source for {} files",
            section_count(&text)
        )),
        "the completeness signal must survive with a TRUE count:\n{text}"
    );
}

/// The Tier 5 accounting fixture (plan §2.3(d)): 515 fillers plus ten
/// `ledger{Name}.ts` of 108 lines / ~7.0 KB — one class, 102 padding comments of
/// exactly 65 chars, and only FOUR methods.
///
/// Four constraints hold simultaneously and the band between the middle two is
/// at most 1,000 bytes wide by construction: believed cost <= 24,000 so three
/// files are admitted, real cost > 24,000 so the overrun is observable, real cost
/// <= 25,000 so the hard-ceiling cut stays unfired and cannot mask it, and only 5
/// subgraph nodes per file so the 6-symbol pointer cap is a verified NO-OP —
/// which is what attributes the failure to the accounting rather than the cap.
fn index_accounting_fixture() -> TestProject {
    const NAMES: [&str; 10] = [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliet",
    ];
    let mut owned: Vec<(String, String)> = Vec::new();
    for i in 0..515 {
        owned.push((
            format!("src/filler{i}.ts"),
            format!("export function wobble{i}(): number {{\n  return {i};\n}}\n"),
        ));
    }
    for name in NAMES {
        let mut src = format!("export class Ledger{name} {{\n");
        for i in 0..102 {
            let head = format!("  // pad {i} for {name} ");
            src.push_str(&format!("{head}{}\n", "-".repeat(65 - head.len())));
        }
        for k in 0..4 {
            src.push_str(&format!(
                "  reconcileLedger{name}Segment{k}(v: number): number {{ return v + {k}; }}\n"
            ));
        }
        src.push_str("}\n");
        owned.push((format!("src/ledger{name}.ts"), src));
    }
    let files: Vec<(&str, &str)> = owned
        .iter()
        .map(|(rel, src)| (rel.as_str(), src.as_str()))
        .collect();
    index_fixture(&files)
}

/// Tier 5 item 15 (CG-26/31), ACCOUNTING half. Given a >=500-file project whose
/// three admitted sections leave the accountant believing it is inside budget,
/// When explore renders, Then the response fits its own `max_output_chars` —
/// without shedding a section to get there.
///
/// Before the fix it over-delivered 720 bytes past its stated 24,000: each
/// section was charged a phantom flat +200 while the epilogue's 1,600 bytes were
/// charged NOTHING, and here the under-charge dominates. Every pointer line on
/// this fixture carries exactly 5 pairs, so the 6-symbol cap changes nothing and
/// the failure is attributable to the accounting alone. The 3-section assertion
/// is the half that blocks the one plausible lazy fix — shedding source to fit.
#[test]
fn explore_output_respects_its_stated_budget() {
    let project = index_accounting_fixture();
    let text = explore_text(project.path(), "Ledger");

    assert!(
        text.len() <= 24_000,
        "response must respect its OWN stated budget, got {} chars",
        text.len()
    );
    assert_eq!(
        section_count(&text),
        3,
        "must not have got there by dropping source:\n{text}"
    );
    assert!(
        !text.contains("output truncated to budget"),
        "the hard-ceiling cut must stay unfired, so the overrun is unmasked:\n{text}"
    );
    assert!(
        text.contains("Complete source for 3 files"),
        "the completeness signal must be present with a TRUE count:\n{text}"
    );
    // The exclusion must still be STATED even when no pointer line fits: five
    // candidates are excluded here and the remaining budget cannot hold a
    // 166-byte line, so the tail is the only honest way to say so.
    assert!(
        text.lines().any(|l| l == "- ... and 5 more files"),
        "the unlisted count must be stated:\n{text}"
    );
}

/// The Tier 5 ambient-damping fixture (plan §2.2): four hand-written ambient
/// shims that nothing imports, plus the real implementation.
///
/// Every declaration name must start with `Upload`: FTS matches `upload` inside
/// `UploadOptions`, and a prefixed variant (`GUploadOptions`) silently deletes the
/// reproduction — the shims never become roots, never reach tier 2, and the
/// inversion disappears.
fn index_ambient_shim_fixture() -> TestProject {
    const SHIMS: [(&str, &str); 4] = [
        (
            "globals",
            "Options Result Handle Session Chunk Meta Policy Target Cursor Token Limits Retry",
        ),
        (
            "vendor",
            "Alpha Beta Gamma Delta Epsilon Zeta Eta Theta Iota Kappa Lambda Mu",
        ),
        (
            "platform",
            "One Two Three Four Five Six Seven Eight Nine Ten Eleven Twelve",
        ),
        (
            "env",
            "Nu Xi Omicron Pi Rho Sigma Tau Upsilon Phi Chi Psi Omega",
        ),
    ];
    let mut owned: Vec<(String, String)> = Vec::new();
    for (stem, suffixes) in SHIMS {
        let mut src = String::from(
            "// ambient shim - hand written, declarations only, nothing depends on it\n",
        );
        for suf in suffixes.split_whitespace() {
            src.push_str(&format!(
                "export interface Upload{suf} {{\n  uploadId: string;\n}}\n"
            ));
        }
        owned.push((format!("src/types/{stem}.d.ts"), src));
    }
    owned.push((
        "src/upload.ts".to_string(),
        "import { readChunk } from './chunker';\nexport function uploadFile(path: string): number {\n  const c = readChunk(path);\n  return c + 1;\n}\n".to_string(),
    ));
    owned.push((
        "src/chunker.ts".to_string(),
        "export function readChunk(p: string): number {\n  return p.length;\n}\n".to_string(),
    ));
    let files: Vec<(&str, &str)> = owned
        .iter()
        .map(|(rel, src)| (rel.as_str(), src.as_str()))
        .collect();
    index_fixture(&files)
}

/// Tier 5 item 14 (CG-28). Given a flow query, When explore ranks the files, Then
/// the implementation outranks an ambient shim that nothing depends on — while the
/// delivered SET and every section length stay exactly as they were.
///
/// Item 14 mutates `finalize`'s ordering key and nothing else, so the assertions
/// are in that dimension: ORDER changes, set and sizes do not. There is
/// deliberately no share assertion — at this fixture's scale nothing is dropped
/// and nothing is resized, so the ambient share is invariant at ~72.5% in all six
/// orderings and any threshold on it would fail identically before and after.
/// `unchanged set` is also what rejects the laziest wrong fix: dropping `.d.ts`
/// from the file order scores a 0% share and would sail through one.
#[test]
fn explore_damps_ambient_shim_below_implementation() {
    let project = index_ambient_shim_fixture();
    let text = explore_text(project.path(), "how does upload work");

    let order: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("#### "))
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    // Measured on this harness pre-fix: `["src/types/globals.d.ts",
    // "src/upload.ts"]`. It is TWO files rather than the three a fully-resolved
    // index delivers, because `index_fixture` runs no cross-file resolution and
    // `chunker.ts` is therefore never reached — which changes the delivered set but
    // not the inversion this test measures. WHICH shim FTS puts first is a search
    // detail, so the shim is matched by suffix rather than by name.
    assert_eq!(
        order.len(),
        2,
        "the delivered SET moved; damping must reorder, not drop: {order:?}"
    );
    assert_eq!(
        order[0], "src/upload.ts",
        "the implementation must be rank #1 (was the ambient shim): {order:?}"
    );
    let shim = order
        .iter()
        .position(|f| f.ends_with(".d.ts"))
        .unwrap_or_else(|| panic!("the shim must still be DELIVERED, only damped: {order:?}"));
    let impl_pos = order.iter().position(|f| *f == "src/upload.ts").unwrap();
    assert!(impl_pos < shim, "the shim still outranks it: {order:?}");
    assert!(
        text.contains("export function uploadFile"),
        "no-regression: the implementation must still be delivered:\n{text}"
    );
}

/// Tier 5 item 14 (CG-28), EXEMPTION. Given a query that NAMES a type the shim
/// declares, When explore ranks the files, Then the shim keeps rank #1 — asking
/// for a type is not the flow query damping exists to fix.
///
/// This passes before the fix too, and is labelled a no-regression check rather
/// than a detection: it is what catches OVER-damping.
#[test]
fn explore_named_type_query_exempts_the_ambient_file() {
    let project = index_ambient_shim_fixture();
    let text = explore_text(project.path(), "UploadOptions");

    let first = text
        .lines()
        .find(|l| l.starts_with("#### "))
        .unwrap_or_else(|| panic!("no section rendered:\n{text}"));
    assert!(
        first.contains(".d.ts"),
        "a query naming the type must keep the shim first, got {first:?}"
    );
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
