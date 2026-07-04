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

use codegraph_mcp::rmcp_handler::HostGuard;

/// Spawn the `StreamableHttpService` for the mini fixture on an ephemeral port,
/// returning the bound base URL (`http://127.0.0.1:PORT/mcp`) plus a guard whose
/// drop cancels the server. Runs inside the caller's tokio runtime. Uses the
/// OPEN default guard (mirrors a no-env production launch).
async fn spawn_http_server(
    project: std::path::PathBuf,
) -> (String, SocketAddr, tokio_util::sync::CancellationToken) {
    spawn_http_server_cfg(project, HostGuard::AllowAny).await
}

/// Builds the config through the production `build_http_config` path so the
/// guard under test is the real one; returns the bound `SocketAddr` so callers
/// can craft explicit `Host:` headers.
async fn spawn_http_server_cfg(
    project: std::path::PathBuf,
    guard: HostGuard,
) -> (String, SocketAddr, tokio_util::sync::CancellationToken) {
    spawn_http_server_declared(project, guard, None).await
}

/// Binds an ephemeral loopback listener but builds the host-guard from a
/// `declared` bind address (defaulting to the real one). A non-loopback
/// `declared` addr reproduces the production case where the bind is
/// `0.0.0.0`/a real interface: the bare loopback defaults do NOT cover a
/// `Host: <declared>` authority, so the guard must have learned it from the
/// bind address — exactly what `build_http_config` adds under `Strict`.
async fn spawn_http_server_declared(
    project: std::path::PathBuf,
    guard: HostGuard,
    declared: Option<SocketAddr>,
) -> (String, SocketAddr, tokio_util::sync::CancellationToken) {
    use rmcp::transport::streamable_http_server::StreamableHttpService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let guard_addr = declared.unwrap_or(addr);

    let ct = tokio_util::sync::CancellationToken::new();
    let config = codegraph_mcp::rmcp_handler::build_http_config(guard_addr, guard)
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

    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });

    (format!("http://{addr}/mcp"), addr, ct)
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

async fn post_json_with_host(
    client: &reqwest::Client,
    url: &str,
    host: &str,
    body: Value,
) -> reqwest::Response {
    client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Host", host)
        .body(serde_json::to_string(&body).unwrap())
        .send()
        .await
        .expect("http post")
}

fn initialize_body() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "inspector", "version": "0" }
        }
    })
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
        let (url, _addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
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
        let (url, _addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
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
        let (url, _addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
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
        let (url, _addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
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

/// Reproduces the MCP Inspector 403: an explicit `Host: 127.0.0.1:<port>` (the
/// bind authority WITH its port) must be accepted, returning a 200 + JSON-RPC
/// initialize result — not "Forbidden: Host header is not allowed".
#[test]
fn http_explicit_host_ip_port_is_allowed() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let host = format!("127.0.0.1:{}", addr.port());
        let resp = post_json_with_host(&client, &url, &host, initialize_body()).await;
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert_eq!(
            status, 200,
            "Host: {host} must be accepted (got {status}): {text}"
        );
        let body: Value = serde_json::from_str(&text).expect("json body");
        assert_eq!(
            body["result"]["protocolVersion"],
            json!("2024-11-05"),
            "explicit-Host initialize must negotiate 2024-11-05: {body}"
        );

        ct.cancel();
    });
}

/// The Inspector also sends `Host: localhost:<port>`; the loopback bind must
/// accept the `localhost:<port>` authority too.
#[test]
fn http_explicit_host_localhost_port_is_allowed() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let host = format!("localhost:{}", addr.port());
        let resp = post_json_with_host(&client, &url, &host, initialize_body()).await;
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert_eq!(
            status, 200,
            "Host: {host} must be accepted (got {status}): {text}"
        );
        let body: Value = serde_json::from_str(&text).expect("json body");
        assert_eq!(
            body["result"]["protocolVersion"],
            json!("2024-11-05"),
            "explicit-Host initialize must negotiate 2024-11-05: {body}"
        );

        ct.cancel();
    });
}

/// NEW DEFAULT: the host guard is OPEN. With no env / `HostGuard::AllowAny`, an
/// arbitrary non-loopback `Host` (e.g. the MCP Inspector's `code-server:12025`
/// or `evil.example.com`) must be ACCEPTED — 200, not 403. This is the reversal
/// of the previous strict default: strictness is now opt-in via
/// `CODEGRAPH_HTTP_ALLOWED_HOSTS` (see the strict-mode tests below).
#[test]
fn http_arbitrary_host_allowed_by_default() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, _addr, ct) = spawn_http_server(project.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let resp = post_json_with_host(&client, &url, "evil.example.com", initialize_body()).await;
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert_eq!(
            status, 200,
            "OPEN default must accept an arbitrary Host (got {status}): {text}"
        );

        ct.cancel();
    });
}

/// STRICT opt-in: with `HostGuard::Strict` and no extra hosts, an arbitrary
/// non-loopback `Host` is rejected with 403 — DNS-rebinding protection is intact
/// once opted in — while a loopback `Host` matching the bind authority is 200.
#[test]
fn http_strict_mode_rejects_unlisted_host() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, addr, ct) =
            spawn_http_server_cfg(project.path().to_path_buf(), HostGuard::Strict(vec![])).await;
        let client = reqwest::Client::new();

        let evil = post_json_with_host(&client, &url, "evil.example.com", initialize_body()).await;
        assert_eq!(
            evil.status(),
            403,
            "strict mode must reject an unlisted arbitrary Host"
        );

        let host = format!("127.0.0.1:{}", addr.port());
        let ok = post_json_with_host(&client, &url, &host, initialize_body()).await;
        assert_eq!(
            ok.status(),
            200,
            "strict mode must still accept the loopback bind authority"
        );

        ct.cancel();
    });
}

/// THE user's scenario: a strict allowlist carrying `code-server:12025` accepts
/// `Host: code-server:12025` (200) while still rejecting an unlisted host (403).
#[test]
fn http_allowed_hosts_list_accepts_listed_host() {
    rt().block_on(async {
        let project = setup_mini_project();
        let guard = HostGuard::Strict(vec!["code-server:12025".to_string()]);
        let (url, _addr, ct) = spawn_http_server_cfg(project.path().to_path_buf(), guard).await;
        let client = reqwest::Client::new();

        let listed =
            post_json_with_host(&client, &url, "code-server:12025", initialize_body()).await;
        let status = listed.status();
        let text = listed.text().await.expect("body");
        assert_eq!(
            status, 200,
            "a listed Host: code-server:12025 must be accepted (got {status}): {text}"
        );

        let evil = post_json_with_host(&client, &url, "evil.example.com", initialize_body()).await;
        assert_eq!(
            evil.status(),
            403,
            "an unlisted Host must still be rejected under a concrete allowlist"
        );

        ct.cancel();
    });
}

/// A `*` entry parses to `HostGuard::AllowAny`, so any `Host` is accepted (200).
#[test]
fn http_allowed_hosts_star_allows_any() {
    rt().block_on(async {
        let project = setup_mini_project();
        let guard = codegraph_mcp::rmcp_handler::parse_allowed_hosts(Some("a,*,b"));
        assert_eq!(guard, HostGuard::AllowAny, "'a,*,b' must parse to AllowAny");
        let (url, _addr, ct) = spawn_http_server_cfg(project.path().to_path_buf(), guard).await;
        let client = reqwest::Client::new();

        let resp = post_json_with_host(&client, &url, "evil.example.com", initialize_body()).await;
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert_eq!(
            status, 200,
            "a '*' allowlist must accept an arbitrary Host (got {status}): {text}"
        );

        ct.cancel();
    });
}

/// With the escape hatch enabled (mapped to `AllowAny`), even an arbitrary
/// `Host` is accepted (for users fronting the server with their own proxy/auth).
#[test]
fn http_allow_any_host_env_disables_guard() {
    rt().block_on(async {
        let project = setup_mini_project();
        let (url, _addr, ct) =
            spawn_http_server_cfg(project.path().to_path_buf(), HostGuard::AllowAny).await;
        let client = reqwest::Client::new();

        let resp = post_json_with_host(&client, &url, "evil.example.com", initialize_body()).await;
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert_eq!(
            status, 200,
            "allow-any-host must accept an arbitrary Host (got {status}): {text}"
        );

        ct.cancel();
    });
}

/// The genuine non-loopback bind reproduction (`0.0.0.0:<port>`, the "serve for a
/// remote client behind SSH-forward/proxy" case) under STRICT mode. The bare
/// loopback defaults do NOT cover `Host: 0.0.0.0:<port>`, so without the bind
/// address in the allowlist this is a 403. `build_http_config` adds
/// `addr.to_string()` to the strict defaults, so the exact bind authority is 200.
#[test]
fn http_non_loopback_bind_authority_is_allowed() {
    rt().block_on(async {
        let project = setup_mini_project();
        let declared: SocketAddr = "0.0.0.0:12026".parse().unwrap();
        let (url, _addr, ct) = spawn_http_server_declared(
            project.path().to_path_buf(),
            HostGuard::Strict(vec![]),
            Some(declared),
        )
        .await;
        let client = reqwest::Client::new();

        let resp = post_json_with_host(&client, &url, "0.0.0.0:12026", initialize_body()).await;
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert_eq!(
            status, 200,
            "Host matching the non-loopback bind authority must be accepted (got {status}): {text}"
        );

        ct.cancel();
    });
}

/// Unit tests for the pure `parse_allowed_hosts` — kept env-free to avoid the
/// process-global env-race flakiness. Covers every branch of the parsing rules.
#[test]
fn parse_allowed_hosts_covers_all_branches() {
    use codegraph_mcp::rmcp_handler::parse_allowed_hosts;

    assert_eq!(parse_allowed_hosts(None), HostGuard::AllowAny, "unset");
    assert_eq!(parse_allowed_hosts(Some("")), HostGuard::AllowAny, "empty");
    assert_eq!(
        parse_allowed_hosts(Some("   ")),
        HostGuard::AllowAny,
        "whitespace-only"
    );
    assert_eq!(parse_allowed_hosts(Some("*")), HostGuard::AllowAny, "star");
    assert_eq!(
        parse_allowed_hosts(Some("a,*,b")),
        HostGuard::AllowAny,
        "any '*' entry wins"
    );
    assert_eq!(
        parse_allowed_hosts(Some("localhost,code-server:12025")),
        HostGuard::Strict(vec![
            "localhost".to_string(),
            "code-server:12025".to_string()
        ]),
        "concrete list"
    );
    assert_eq!(
        parse_allowed_hosts(Some(" a , b ")),
        HostGuard::Strict(vec!["a".to_string(), "b".to_string()]),
        "entries are trimmed and empties dropped"
    );
}

#[test]
fn allow_any_host_env_parses_truthy_values() {
    use codegraph_mcp::rmcp_handler::{ALLOW_ANY_HOST_ENV, http_allow_any_host_from_env};

    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for (raw, expect) in [
        ("1", true),
        ("true", true),
        ("YES", true),
        ("on", true),
        ("0", false),
        ("false", false),
        ("", false),
        ("banana", false),
    ] {
        // SAFETY: serialized by ENV_LOCK; single-threaded within this test body.
        unsafe {
            if raw.is_empty() {
                std::env::remove_var(ALLOW_ANY_HOST_ENV);
            } else {
                std::env::set_var(ALLOW_ANY_HOST_ENV, raw);
            }
        }
        assert_eq!(
            http_allow_any_host_from_env(),
            expect,
            "CODEGRAPH_HTTP_ALLOW_ANY_HOST={raw:?} must resolve to {expect}"
        );
    }
    // SAFETY: serialized by ENV_LOCK; restore to unset so no other test observes it.
    unsafe {
        std::env::remove_var(ALLOW_ANY_HOST_ENV);
    }
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The per-request debug middleware must be a TRANSPARENT passthrough: it logs
/// (side effect on STDERR) but returns `next.run(req)`'s response byte-identical
/// — same status, same headers, same body. This unit test drives
/// `debug_log_requests` directly (no server spawn) and asserts the status the
/// inner handler produced survives the layer unchanged. RED before the
/// middleware exists.
#[test]
fn debug_log_requests_is_transparent_passthrough() {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::StatusCode;
    use axum::routing::post;
    use tower::util::ServiceExt;

    rt().block_on(async {
        // A tiny inner router that returns a fixed 418 body; the middleware must
        // pass that exact status + body through untouched.
        let app = axum::Router::new()
            .route(
                "/mcp",
                post(|| async { (StatusCode::IM_A_TEAPOT, "brewed") }),
            )
            .layer(axum::middleware::from_fn(
                codegraph_mcp::rmcp_handler::debug_log_requests,
            ));

        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("Host", "127.0.0.1:9")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.expect("middleware passthrough");
        assert_eq!(
            resp.status(),
            StatusCode::IM_A_TEAPOT,
            "middleware must pass the inner status through unchanged"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            &bytes[..],
            b"brewed",
            "middleware must pass the inner body through unchanged"
        );
    });
}
