//! Phase C — streamable-HTTP integration for the rmcp `CodeGraphHandler`.
//!
//! Starts an rmcp `StreamableHttpService` bound on `127.0.0.1:0` (ephemeral)
//! serving `CodeGraphHandler::http(<mini indexed fixture>)` in no_roots/pinned
//! mode, then drives it over raw HTTP POSTs to `/mcp` (the curl / Zed-url shape)
//! and asserts:
//!   (a) `initialize` → negotiated protocolVersion == "2024-11-05" and
//!       capabilities == `{"tools":{}}`;
//!   (b) `tools/call codegraph_search {"query":"McpServer"}` with NO projectPath
//!       (relies on the http-pinned default) → NON-EMPTY, non-error result;
//!   (c) `initialize` triggers NO server→client `roots/list` request (http mode
//!       is no_roots — a single JSON body comes back, never a roots request);
//!   (d) an unknown tool → `error.code == -32602`.
//!
//! The server runs in stateless `json_response` mode: every POST returns a
//! single `application/json` body (no SSE), because no_roots mode never emits a
//! server-initiated message — the exact shape a plain MCP url client consumes.
// rmcp is the sole MCP transport (Phase E); this test exercises it unconditionally.

#[path = "support/parity.rs"]
mod parity;

use std::net::SocketAddr;
use std::time::Duration;

use parity::setup_mini_project;
use serde_json::{Value, json};

/// Spawn the `StreamableHttpService` for the mini fixture on an ephemeral port,
/// returning the bound base URL (`http://127.0.0.1:PORT/mcp`) plus a guard whose
/// drop cancels the server. Runs inside the caller's tokio runtime.
async fn spawn_http_server(
    project: std::path::PathBuf,
) -> (String, tokio_util::sync::CancellationToken) {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };

    let ct = tokio_util::sync::CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_cancellation_token(ct.child_token());

    let service: StreamableHttpService<
        codegraph_mcp::rmcp_handler::CodeGraphHandler,
        LocalSessionManager,
    > = StreamableHttpService::new(
        move || {
            Ok(codegraph_mcp::rmcp_handler::CodeGraphHandler::http(
                project.clone(),
            ))
        },
        std::sync::Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });

    (format!("http://{addr}/mcp"), ct)
}

async fn post_json(client: &reqwest::Client, url: &str, body: Value) -> reqwest::Response {
    client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(serde_json::to_string(&body).unwrap())
        .send()
        .await
        .expect("http post")
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// (a) + (c) initialize over HTTP: negotiated protocolVersion 2024-11-05,
/// capabilities exactly `{"tools":{}}`, and the response is a SINGLE JSON
/// initialize result — no `roots/list` request frame (http mode is no_roots).
#[test]
fn http_initialize_negotiates_2024_11_05_and_no_roots() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "http-test", "version": "0" }
            }
        });
        let resp = post_json(&client, &url, init).await;
        assert_eq!(resp.status(), 200, "initialize must return 200");
        let ct_hdr = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            ct_hdr.contains("application/json"),
            "initialize response must be application/json (no SSE), got {ct_hdr}"
        );
        let body: Value = resp.json().await.expect("json body");

        // (c) the single frame IS the initialize result — NOT a server-initiated
        // roots/list request. A roots request would have method == "roots/list".
        assert_ne!(
            body["method"],
            json!("roots/list"),
            "http (no_roots) mode must NOT request roots/list"
        );
        assert_eq!(body["id"], json!(1), "response id echoes the request");

        let result = &body["result"];
        assert_eq!(
            result["protocolVersion"],
            json!("2024-11-05"),
            "negotiated protocolVersion must be 2024-11-05, got {}",
            result["protocolVersion"]
        );
        assert_eq!(
            result["capabilities"],
            json!({ "tools": {} }),
            "capabilities must be exactly {{\"tools\":{{}}}}, got {}",
            result["capabilities"]
        );

        ct.cancel();
    });
}

/// (b) tools/call codegraph_search with NO projectPath resolves against the
/// http-pinned default project → NON-EMPTY, non-error result.
#[test]
fn http_tools_call_search_uses_pinned_default_non_empty() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let call = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "codegraph_search",
                "arguments": { "query": "McpServer" }
            }
        });
        let resp = post_json(&client, &url, call).await;
        assert_eq!(resp.status(), 200, "tools/call must return 200");
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(body["id"], json!(2), "response id echoes the request");

        let result = &body["result"];
        assert_ne!(
            result["isError"],
            json!(true),
            "search must not be an error: {body}"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !text.trim().is_empty(),
            "search result text must be non-empty: {body}"
        );
        assert!(
            !text.contains("No indexed project resolved"),
            "pinned default must resolve without projectPath: {text}"
        );

        ct.cancel();
    });
}

/// (d) an unknown tool → JSON-RPC error with code -32602.
#[test]
fn http_unknown_tool_returns_minus_32602() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let call = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "nonexistent_tool", "arguments": {} }
        });
        let resp = post_json(&client, &url, call).await;
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body["error"]["code"],
            json!(-32602),
            "unknown tool must map to -32602, got {body}"
        );

        ct.cancel();
    });
}

/// Sanity: the ephemeral server is actually reachable within a short deadline
/// (guards against a bind/serve regression producing a hang).
#[test]
fn http_server_binds_and_is_reachable() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
        });
        let resp = post_json(&client, &url, init).await;
        assert_eq!(resp.status(), 200);
        ct.cancel();
    });
}
