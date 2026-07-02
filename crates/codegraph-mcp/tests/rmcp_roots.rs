//! Phase B — roots-adoption behavior for the rmcp `CodeGraphHandler`.
//!
//! The Phase-0 spike established that the old wire-frame test (two frames from
//! one `run()`) CANNOT be mirrored over rmcp's owned read loop, so adoption is
//! asserted at the BEHAVIOR level via a real rmcp client↔server pair over
//! `tokio::io::duplex`:
//!
//!   - POSITIVE: a NON-pinned handler (default_project = an UNINDEXED cwd,
//!     `no_roots = false`) plus a client that declares `capabilities.roots` and,
//!     when the server requests roots, answers with an INDEXED project root →
//!     `tools/call codegraph_search {query:"McpServer"}` with NO projectPath
//!     returns a NON-EMPTY, non-error result — proving the server ADOPTED the
//!     client's indexed root.
//!   - NEGATIVE: the same handler, but the client reports an UNINDEXED root →
//!     NO adoption (the tool call falls through to the no-project error, NOT the
//!     wrong project).
//!   - GUARD: an HTTP/`no_roots` handler (`CodeGraphHandler::http`) never
//!     requests roots — even a roots-capable client leaves it unadopted, so a
//!     tool call with NO projectPath against an unindexed pin errors.
//!
//! RED until `CodeGraphHandler` grows the non-pinned adoption path
//! (`on_initialized` → `peer.list_roots()` → `roots::` adoption rules).
#![cfg(feature = "rmcp")]

#[path = "support/parity.rs"]
mod parity;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_mcp::rmcp_handler::CodeGraphHandler;
use rmcp::handler::client::ClientHandler;
#[allow(deprecated)]
// ListRootsResult/Root: SEP-2577 roots wire types (rmcp 2.1, no replacement).
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, ListRootsResult,
    ProtocolVersion, Root,
};
use rmcp::service::{RequestContext, RoleClient};
use rmcp::ServiceExt;
use serde_json::json;

use parity::{setup_mini_project, TestProject};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Create a real on-disk, UNINDEXED directory (no `.codegraph/codegraph.db`) so
/// `canonicalize` succeeds for the `== cwd` adoption compare.
fn unindexed_dir(tag: &str) -> TestProject {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "cg-mcp-rmcp-roots-{tag}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    TestProject::from_path(path)
}

/// A minimal rmcp client that DECLARES `capabilities.roots` and, when the server
/// requests roots via `roots/list`, answers with exactly `reported_root` — the
/// Zed-local shape (roots-capable client that reports its indexed workspace).
#[derive(Clone)]
struct RootsClient {
    reported_root: PathBuf,
}

impl ClientHandler for RootsClient {
    fn get_info(&self) -> ClientInfo {
        // Declare the roots capability so the server's `should_request_roots`
        // gate fires (matches `default_is_adoptable` + client-declares-roots).
        // `ClientInfo` is `#[non_exhaustive]`, so build via the constructor.
        #[allow(deprecated)]
        let capabilities = ClientCapabilities::builder().enable_roots().build();
        ClientInfo::new(capabilities, Implementation::new("roots-test-client", "0"))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
    }

    #[allow(deprecated)] // ListRootsResult/Root: SEP-2577; the roots wire type in rmcp 2.1.
    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, rmcp::ErrorData> {
        let uri = format!("file://{}", self.reported_root.display());
        Ok(ListRootsResult::new(vec![Root::new(uri).with_name("proj")]))
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// Drive a `RootsClient` against `handler` over an in-memory duplex, waiting for
/// the initialize/initialized handshake to complete (so the server's
/// `on_initialized` roots round-trip has happened), then run `tools/call
/// codegraph_search {query}` with NO projectPath and return the first content
/// text (empty string on missing content).
async fn search_after_adoption(
    handler: CodeGraphHandler,
    reported_root: PathBuf,
    query: &str,
) -> (bool, String) {
    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let server_task = tokio::spawn(async move {
        if let Ok(running) = handler.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });

    let client = RootsClient { reported_root }
        .serve(client_io)
        .await
        .expect("rmcp client handshake");

    // Give the server's post-initialized roots round-trip time to complete
    // before the first tool call (client sends initialized → server calls
    // peer.list_roots() → adopts). A short bounded await, not a race.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // Probe: once adoption happened, a search resolves non-empty. We break
        // early on success to keep the test fast; otherwise we exhaust the
        // budget and assert on the final attempt.
        let call = CallToolRequestParams::new("codegraph_search".to_string())
            .with_arguments(json!({ "query": query }).as_object().cloned().unwrap());
        if let Ok(result) = client.call_tool(call).await {
            let is_error = result.is_error == Some(true);
            let text = result
                .content
                .first()
                .and_then(|c| c.as_text().map(|t| t.text.clone()))
                .unwrap_or_default();
            if !is_error && !text.trim().is_empty() && !text.contains("No indexed project") {
                let _ = client.cancel().await;
                let _ = server_task.await;
                return (is_error, text);
            }
        }
    }

    // Final attempt — return whatever the server resolves so the assertion can
    // report the actual (failing or passing) content.
    let call = CallToolRequestParams::new("codegraph_search".to_string())
        .with_arguments(json!({ "query": query }).as_object().cloned().unwrap());
    let (is_error, text) = match client.call_tool(call).await {
        Ok(result) => {
            let is_error = result.is_error == Some(true);
            let text = result
                .content
                .first()
                .and_then(|c| c.as_text().map(|t| t.text.clone()))
                .unwrap_or_default();
            (is_error, text)
        }
        Err(err) => (true, format!("{err}")),
    };
    let _ = client.cancel().await;
    let _ = server_task.await;
    (is_error, text)
}

/// POSITIVE: a non-pinned handler launched from an UNINDEXED cwd + a roots-
/// capable client that reports an INDEXED root → the server adopts the indexed
/// root and `codegraph_search` (no projectPath) resolves NON-EMPTY.
#[test]
fn rmcp_adopts_indexed_client_root_and_resolves_tool_call() {
    rt().block_on(async {
        let indexed = setup_mini_project();
        let cwd = unindexed_dir("pos-cwd");

        // Non-pinned mode: default = an unindexed cwd, no_roots = false (the
        // bare-serve Zed case). Adoption must displace the unindexed cwd default.
        let handler = CodeGraphHandler::serve_with_roots(
            Some(cwd.path().to_path_buf()),
            Some(cwd.path().to_path_buf()),
        );

        let (is_error, text) =
            search_after_adoption(handler, indexed.path().to_path_buf(), "McpServer").await;

        assert!(!is_error, "adopted-root search must not error, got: {text}");
        assert!(
            !text.trim().is_empty(),
            "adopted-root search must be non-empty, got: {text:?}"
        );
        assert!(
            !text.contains("No indexed project"),
            "must adopt the client's indexed root, not fall through: {text}"
        );
    });
}

/// NEGATIVE: the same non-pinned handler, but the client reports an UNINDEXED
/// root → NO adoption; the tool call (no projectPath) falls through to the
/// no-project error (NOT the wrong project's results).
#[test]
fn rmcp_does_not_adopt_unindexed_client_root() {
    rt().block_on(async {
        let cwd = unindexed_dir("neg-cwd");
        let reported = unindexed_dir("neg-reported");

        let handler = CodeGraphHandler::serve_with_roots(
            Some(cwd.path().to_path_buf()),
            Some(cwd.path().to_path_buf()),
        );

        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let server_task = tokio::spawn(async move {
            if let Ok(running) = handler.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let client = RootsClient {
            reported_root: reported.path().to_path_buf(),
        }
        .serve(client_io)
        .await
        .expect("rmcp client handshake");

        // Allow the post-initialized roots round-trip to run.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let call = CallToolRequestParams::new("codegraph_search".to_string()).with_arguments(
            json!({ "query": "McpServer" })
                .as_object()
                .cloned()
                .unwrap(),
        );
        let result = client.call_tool(call).await.expect("tools/call");
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();

        // No adoption ⇒ the unindexed default resolves nothing ⇒ the no-project
        // error message (isError result), NOT a wrong project's results.
        assert!(
            text.contains("No indexed project"),
            "unindexed client root must NOT be adopted; expected no-project error, got: {text}"
        );

        let _ = client.cancel().await;
        let _ = server_task.await;
    });
}

/// GUARD: an HTTP/`no_roots` handler never requests roots. Even a roots-capable
/// client that reports an INDEXED root leaves the unindexed HTTP pin unadopted,
/// so a tool call with NO projectPath errors (parity with Phase C's no_roots).
#[test]
fn rmcp_http_no_roots_never_adopts() {
    rt().block_on(async {
        let indexed = setup_mini_project();
        // HTTP handler PINNED to an unindexed dir in no_roots mode. `http()`
        // must never issue a roots request, so the indexed client root is
        // ignored and the unindexed pin cannot resolve.
        let unindexed_pin = unindexed_dir("http-pin");
        let handler = CodeGraphHandler::http(unindexed_pin.path().to_path_buf());

        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let server_task = tokio::spawn(async move {
            if let Ok(running) = handler.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let client = RootsClient {
            reported_root: indexed.path().to_path_buf(),
        }
        .serve(client_io)
        .await
        .expect("rmcp client handshake");

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let call = CallToolRequestParams::new("codegraph_search".to_string()).with_arguments(
            json!({ "query": "McpServer" })
                .as_object()
                .cloned()
                .unwrap(),
        );
        let result = client.call_tool(call).await.expect("tools/call");
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();

        assert!(
            text.contains("No indexed project"),
            "http no_roots mode must NOT adopt the client's indexed root: {text}"
        );

        let _ = client.cancel().await;
        let _ = server_task.await;
    });
}
