//! L1 behavioral-parity harness.
//!
//! Runs one MCP request through the rmcp transport and returns the parsed
//! response `Value`, then structurally compares it against the GOLDEN response.
//! This is how "rmcp reproduces the golden" is PROVEN across the 15 fixtures
//! (L2) rather than asserted:
//! - [`run_rmcp_stdio`] drives the `CodeGraphHandler` over a `tokio::io::duplex`
//!   pair with a real rmcp client — exercising rmcp's REAL framing, NOT direct
//!   method calls.
//! - [`assert_parity`] applies the SAME structural comparison the golden suite
//!   uses (type/isError/sorted-text-lines for tool results; names+order+schema
//!   for tools/list; capabilities/serverInfo.name/instructions + negotiated
//!   protocolVersion for initialize). The BASELINE is the golden JSON itself
//!   (Phase E deleted the hand-rolled `run_old` server).
//!
//! Included via `#[path]` by the parity tests; not a standalone test binary.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_mcp::rmcp_handler::CodeGraphHandler;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, PaginatedRequestParams,
    ProtocolVersion,
};
use serde_json::{Value, json};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

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

/// Owns a temp project dir and removes it on drop.
pub struct TestProject {
    path: PathBuf,
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TestProject {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }
}

/// Materialize the indexed mini project on disk (golden DB + fixture sources),
/// identical to `golden_mcp.rs::setup_mini_project`.
pub fn setup_mini_project() -> TestProject {
    let root = workspace_root();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "cg-mcp-parity-{}-{nanos}-{seq}",
        std::process::id()
    ));
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

/// Rewrite a fixture request's `projectPath` argument to the temp project.
pub fn rewrite_project(project: &Path, request: &mut Value) {
    if let Some(args) = request
        .get_mut("params")
        .and_then(|p| p.get_mut("arguments"))
        && let Some(obj) = args.as_object_mut()
        && obj.contains_key("projectPath")
    {
        obj.insert("projectPath".to_string(), json!(project.to_str().unwrap()));
    }
}

/// An rmcp client that requests protocolVersion 2024-11-05 (the golden's
/// initialize request), so negotiation lands on 2024-11-05 for parity.
pub fn golden_client() -> ClientInfo {
    ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("golden", "0"),
    )
    .with_protocol_version(ProtocolVersion::V_2024_11_05)
}

/// Run one JSON-RPC request frame through the NEW rmcp `CodeGraphHandler` over a
/// real `tokio::io::duplex` transport driven by an rmcp client — exercising
/// rmcp's REAL wire framing, NOT direct method calls.
///
/// Returns a response `Value` shaped like the old server's `{result: ...}` /
/// `{error: ...}` envelope so [`assert_parity`] can compare the two.
pub fn run_rmcp_stdio(project: &Path, mut request: Value) -> Value {
    rewrite_project(project, &mut request);
    let project = project.to_path_buf();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move { run_rmcp_stdio_async(&project, request).await })
}

async fn run_rmcp_stdio_async(project: &Path, request: Value) -> Value {
    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);

    let handler = CodeGraphHandler::new(Some(project.to_path_buf()));
    let server_task = tokio::spawn(async move {
        let running = handler.serve(server_io).await?;
        running.waiting().await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    });

    // rmcp NEGOTIATES protocolVersion against the client's request; a default
    // `()` client requests LATEST and would negotiate to LATEST. `golden_client`
    // requests 2024-11-05 (what the golden initialize sends), so it negotiates to
    // 2024-11-05 — parity with the hand-rolled server.
    let client = golden_client()
        .serve(client_io)
        .await
        .expect("rmcp client handshake");

    let method = request["method"].as_str().unwrap_or("");
    let id = request.get("id").cloned().unwrap_or(Value::Null);

    let response = match method {
        "initialize" => {
            let info = client.peer_info().expect("server info negotiated");
            let result = serde_json::to_value(&*info).expect("serialize server info");
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        }
        "tools/list" => {
            let listed = client
                .list_tools(Some(PaginatedRequestParams::default()))
                .await
                .expect("list_tools");
            let result = serde_json::to_value(&listed).expect("serialize tools list");
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        }
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or("").to_string();
            let arguments = params.get("arguments").and_then(Value::as_object).cloned();
            let mut call = CallToolRequestParams::new(name);
            if let Some(map) = arguments {
                call = call.with_arguments(map);
            }
            match client.call_tool(call).await {
                Ok(result) => {
                    let result = serde_json::to_value(&result).expect("serialize tool result");
                    json!({ "jsonrpc": "2.0", "id": id, "result": result })
                }
                Err(err) => {
                    // rmcp surfaces a McpError; re-shape into the JSON-RPC error
                    // envelope the golden fixtures use for unknown-tool.
                    let (code, message) = error_code_message(&err);
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": code, "message": message },
                    })
                }
            }
        }
        other => panic!("parity harness does not drive method {other:?}"),
    };

    let _ = client.cancel().await;
    let _ = server_task.await;
    response
}

/// Extract the JSON-RPC error code + message from an rmcp service error, so an
/// unknown-tool `-32602` maps back to the golden error envelope.
fn error_code_message(err: &rmcp::ServiceError) -> (i64, String) {
    match err {
        rmcp::ServiceError::McpError(data) => (i64::from(data.code.0), data.message.to_string()),
        other => (-32603, other.to_string()),
    }
}

// === Structural parity assertions (reused from golden_mcp.rs) ================

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

fn normalized_lines(t: &str) -> Vec<String> {
    let mut lines: Vec<String> = t
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| normalize_header_line(l.to_string()))
        .collect();
    lines.sort();
    lines
}

/// Assert two response `Value`s are structurally equal for the given fixture,
/// using the SAME comparison `golden_mcp.rs` applies:
/// - initialize: negotiated protocolVersion + capabilities + serverInfo.name +
///   instructions match.
/// - tools/list: tool names + order + inputSchema value-equality.
/// - tool results: content[0].type + isError + sorted non-empty text line-set.
/// - errors: error.code + error.message.
pub fn assert_parity(fixture: &str, old: &Value, new: &Value, ctx: &str) {
    // Error envelope (unknown tool).
    if !old["error"].is_null() || !new["error"].is_null() {
        assert_eq!(
            old["error"]["code"], new["error"]["code"],
            "{ctx}: error.code mismatch"
        );
        assert_eq!(
            old["error"]["message"], new["error"]["message"],
            "{ctx}: error.message mismatch"
        );
        return;
    }

    let old_result = &old["result"];
    let new_result = &new["result"];

    match fixture {
        "initialize" => {
            assert_eq!(
                old_result["protocolVersion"], new_result["protocolVersion"],
                "{ctx}: protocolVersion (negotiated) mismatch"
            );
            assert_eq!(
                old_result["capabilities"], new_result["capabilities"],
                "{ctx}: capabilities mismatch"
            );
            assert_eq!(
                old_result["serverInfo"]["name"], new_result["serverInfo"]["name"],
                "{ctx}: serverInfo.name mismatch"
            );
            assert_eq!(
                old_result["instructions"], new_result["instructions"],
                "{ctx}: instructions must be byte-identical"
            );
        }
        "tools_list" => {
            let old_tools = old_result["tools"].as_array().expect("old tools array");
            let new_tools = new_result["tools"].as_array().expect("new tools array");
            let onames: Vec<&str> = old_tools
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect();
            let nnames: Vec<&str> = new_tools
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect();
            assert_eq!(onames, nnames, "{ctx}: tool names + order mismatch");
            for (o, n) in old_tools.iter().zip(new_tools.iter()) {
                assert_eq!(
                    o["inputSchema"], n["inputSchema"],
                    "{ctx}: inputSchema for {} mismatch",
                    o["name"]
                );
            }
        }
        _ => {
            // Tool-call result: structural line-set equality.
            let o = &old_result["content"][0];
            let n = &new_result["content"][0];
            assert_eq!(o["type"], n["type"], "{ctx}: content type mismatch");
            // Normalize isError: the hand-rolled ToolResult omits it on success
            // (None), while rmcp's CallToolResult::success sets Some(false) — both
            // mean "not an error". Only `true` is a real error signal.
            let is_err = |r: &Value| -> bool { r.get("isError") == Some(&json!(true)) };
            assert_eq!(
                is_err(old_result),
                is_err(new_result),
                "{ctx}: isError mismatch"
            );
            let ot = o["text"].as_str().unwrap_or("");
            let nt = n["text"].as_str().unwrap_or("");
            // Status has a documented DB-size diff; compare excluding that line.
            let (ol, nl) = if fixture == "codegraph_status" {
                let strip = |t: &str| -> Vec<String> {
                    let mut v: Vec<String> = t
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .filter(|l| !l.starts_with("**Database size:**"))
                        .map(str::to_string)
                        .collect();
                    v.sort();
                    v
                };
                (strip(ot), strip(nt))
            } else if fixture == "codegraph_explore" {
                let strip = |t: &str| -> Vec<String> {
                    let mut v: Vec<String> = t
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .filter(|l| !l.starts_with("Found "))
                        .map(|l| normalize_header_line(l.to_string()))
                        .collect();
                    v.sort();
                    v
                };
                (strip(ot), strip(nt))
            } else {
                (normalized_lines(ot), normalized_lines(nt))
            };
            assert_eq!(
                ol, nl,
                "{ctx}: text line-set differs\nOLD:\n{ot}\n\nNEW:\n{nt}"
            );
        }
    }
}

pub fn load_golden(name: &str) -> (Value, Value) {
    let path = workspace_root()
        .join("reference/golden/mcp")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let entry: Value = serde_json::from_str(&raw).expect("golden json");
    (entry["request"].clone(), entry["response"].clone())
}

/// The 15 golden MCP fixtures (L2 invariant set).
pub const GOLDEN_FIXTURES: [&str; 15] = [
    "initialize",
    "tools_list",
    "codegraph_search",
    "codegraph_callers",
    "codegraph_callees",
    "codegraph_impact",
    "codegraph_node",
    "codegraph_node_1",
    "codegraph_status",
    "codegraph_files",
    "codegraph_files_1",
    "codegraph_files_2",
    "codegraph_explore",
    "error_unknown_tool",
    "error_missing_arg",
];
