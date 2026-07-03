//! L3 property / adversarial tests for the rmcp `CodeGraphHandler` (Phase A).
//!
//! RED-first proofs for the risk areas the spike flagged:
//! (a) capabilities serialize to EXACTLY `{"tools":{}}`;
//! (b) the NEGOTIATED initialize protocolVersion is `2024-11-05`;
//! (c) an unknown tool → `error.code == -32602`;
//! (d) a handler whose engine call PANICS returns an `isError` result AND the
//!     runtime/process survives (Q5-unwind);
//! (e) dynamic tools/list: indexed default → full surface; no indexed default →
//!     the projectPath-required variant.
#![cfg(feature = "rmcp")]

#[path = "support/parity.rs"]
mod parity;

use codegraph_mcp::rmcp_handler::CodeGraphHandler;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{CallToolRequestParams, PaginatedRequestParams};
use serde_json::json;

use parity::{TestProject, golden_client, setup_mini_project, workspace_root};

async fn connect(
    handler: CodeGraphHandler,
) -> (
    rmcp::service::RunningService<rmcp::RoleClient, rmcp::model::ClientInfo>,
    tokio::task::JoinHandle<()>,
) {
    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let server_task = tokio::spawn(async move {
        if let Ok(running) = handler.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = golden_client()
        .serve(client_io)
        .await
        .expect("rmcp client handshake");
    (client, server_task)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn call(
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> CallToolRequestParams {
    let params = CallToolRequestParams::new(name.to_string());
    match arguments {
        Some(map) => params.with_arguments(map),
        None => params,
    }
}

fn paginated() -> Option<PaginatedRequestParams> {
    Some(PaginatedRequestParams::default())
}

/// (a) capabilities from `get_info()` serialize to EXACTLY `{"tools":{}}`.
#[test]
fn get_info_capabilities_serialize_to_exactly_tools_empty() {
    let handler = CodeGraphHandler::new(None);
    let info = handler.get_info();
    let value = serde_json::to_value(&info).expect("serialize server info");
    assert_eq!(
        value["capabilities"],
        json!({ "tools": {} }),
        "capabilities must be exactly {{\"tools\":{{}}}}, got {}",
        value["capabilities"]
    );
}

/// (b) the NEGOTIATED initialize protocolVersion (from the real handshake, not
/// just get_info) is `2024-11-05`.
#[test]
fn negotiated_protocol_version_is_2024_11_05() {
    rt().block_on(async {
        let project = setup_mini_project();
        let handler = CodeGraphHandler::new(Some(project.path().to_path_buf()));
        let (client, task) = connect(handler).await;
        let info = client.peer_info().expect("negotiated server info");
        let value = serde_json::to_value(&*info).expect("serialize");
        assert_eq!(
            value["protocolVersion"],
            json!("2024-11-05"),
            "negotiated protocolVersion must be 2024-11-05, got {}",
            value["protocolVersion"]
        );
        let _ = client.cancel().await;
        let _ = task.await;
    });
}

/// (c) an unknown tool → JSON-RPC `-32602` (invalid_params), NOT the built-in
/// `-32601` method-not-found.
#[test]
fn unknown_tool_returns_minus_32602() {
    rt().block_on(async {
        let project = setup_mini_project();
        let handler = CodeGraphHandler::new(Some(project.path().to_path_buf()));
        let (client, task) = connect(handler).await;
        let err = client
            .call_tool(call("nonexistent_tool", None))
            .await
            .expect_err("unknown tool must be an error");
        let code = match &err {
            rmcp::ServiceError::McpError(data) => i64::from(data.code.0),
            other => panic!("expected McpError, got {other:?}"),
        };
        assert_eq!(code, -32602, "unknown tool must map to -32602");
        let _ = client.cancel().await;
        let _ = task.await;
    });
}

/// (d) Q5-unwind: a tool whose engine work PANICS returns an `isError` result
/// (or an error) AND the runtime survives to serve the next call.
#[test]
fn panicking_tool_returns_error_and_runtime_survives() {
    rt().block_on(async {
        let project = setup_mini_project();
        let handler = CodeGraphHandler::new(Some(project.path().to_path_buf()));
        let (client, task) = connect(handler).await;

        // `__panic__` is a test-only tool name the handler treats as "known" and
        // panics inside its spawn_blocking closure; the handler must map the
        // JoinError to an isError result instead of aborting the process.
        let result = client.call_tool(call("__panic__", None)).await;
        match result {
            Ok(tool_result) => assert_eq!(
                tool_result.is_error,
                Some(true),
                "a panicking tool must return an isError result"
            ),
            Err(rmcp::ServiceError::McpError(_)) => {}
            Err(other) => panic!("unexpected transport error: {other:?}"),
        }

        // The runtime survived: a NORMAL call still works after the panic.
        let ok = client
            .call_tool(call(
                "codegraph_search",
                json!({ "query": "add" }).as_object().cloned(),
            ))
            .await
            .expect("post-panic call must succeed (runtime alive)");
        assert_ne!(ok.is_error, Some(true), "post-panic search must not error");
        let _ = client.cancel().await;
        let _ = task.await;
    });
}

/// (e1) dynamic tools/list: an INDEXED default project → the full 4-tool default
/// surface with projectPath OPTIONAL.
#[test]
fn tools_list_indexed_default_serves_optional_project_path() {
    rt().block_on(async {
        let project = setup_mini_project();
        let handler = CodeGraphHandler::new(Some(project.path().to_path_buf()));
        let (client, task) = connect(handler).await;
        let listed = client.list_tools(paginated()).await.expect("list_tools");
        let value = serde_json::to_value(&listed).expect("serialize");
        let tools = value["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 4, "indexed default → 4-tool default surface");
        for tool in tools {
            let has_pp = tool["inputSchema"]["required"]
                .as_array()
                .map(|r| r.iter().any(|v| v == "projectPath"))
                .unwrap_or(false);
            assert!(
                !has_pp,
                "indexed default keeps projectPath OPTIONAL for {}",
                tool["name"]
            );
        }
        let _ = client.cancel().await;
        let _ = task.await;
    });
}

/// (e2) dynamic tools/list: NO indexed default → the same 4 tools with
/// projectPath REQUIRED in each schema.
#[test]
fn tools_list_no_indexed_default_marks_project_path_required() {
    rt().block_on(async {
        let base = std::env::temp_dir().join(format!(
            "cg-mcp-l3-noidx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let guard = TestProject::from_path(base.clone());
        let handler = CodeGraphHandler::new(Some(base));
        let (client, task) = connect(handler).await;
        let listed = client.list_tools(paginated()).await.expect("list_tools");
        let value = serde_json::to_value(&listed).expect("serialize");
        let tools = value["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 4, "unindexed default still lists 4 tools");
        for tool in tools {
            let required = tool["inputSchema"]["required"]
                .as_array()
                .unwrap_or_else(|| panic!("tool {} has required array", tool["name"]));
            assert!(
                required.iter().any(|v| v == "projectPath"),
                "tool {} must mark projectPath required when unindexed",
                tool["name"]
            );
        }
        let _ = client.cancel().await;
        let _ = task.await;
        drop(guard);
    });
}

#[test]
fn workspace_root_exists() {
    assert!(workspace_root().join("reference/golden/mcp").is_dir());
}
