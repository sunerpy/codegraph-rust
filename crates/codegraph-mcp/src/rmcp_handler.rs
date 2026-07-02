//! rmcp `ServerHandler` for CodeGraph — the official-SDK stdio transport
//! (Phase A), running ALONGSIDE the hand-rolled [`crate::McpServer`].
//!
//! Reuses the sync engine + rendering layer verbatim: [`CodeGraphEngine`],
//! [`crate::schemas`], [`crate::instructions::SERVER_INSTRUCTIONS`], and the
//! `roots` project resolution. Only the transport/session shell is rmcp.
//!
//! ## Sync engine bridge (Decision 10)
//!
//! [`CodeGraphEngine`] wraps a `rusqlite::Connection` — `Send + !Sync`. rmcp
//! handler futures are `Send + 'static`, so a `&CodeGraphEngine` borrowed
//! through the cache mutex may NOT cross an `.await`, and a `spawn_blocking`
//! closure (`'static + Send`) cannot borrow `&self`. `call_tool` therefore does
//! the WHOLE "open-or-get-cached engine + execute + render to an OWNED
//! [`ToolResult`]" inside ONE `spawn_blocking` closure: it moves in an `Arc`
//! clone of the cache + owned project path + owned args and returns an owned
//! result. No engine borrow crosses the closure boundary.
//!
//! ## Panic isolation (Decision 9 / Q5-unwind)
//!
//! With `[profile.release] panic = "unwind"`, a panic inside the
//! `spawn_blocking` closure surfaces as `JoinError::is_panic()`, which this maps
//! to an `isError` [`CallToolResult`] — a tool bug returns an error and the
//! process/runtime stays alive (parity with the sync stdio server).
#![cfg(feature = "rmcp")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{json, Value};

use crate::engine::CodeGraphEngine;
use crate::instructions::SERVER_INSTRUCTIONS;
use crate::protocol::ToolResult;
use crate::roots::db_path_for;
use crate::schemas;

const SERVER_NAME: &str = "codegraph";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Test-only tool name that forces the `spawn_blocking` closure to panic, so
/// the Q5-unwind panic→error mapping can be exercised (see `tests/rmcp_l3.rs`).
#[cfg(test)]
const PANIC_TOOL: &str = "__panic__";

type EngineCache = Arc<Mutex<HashMap<PathBuf, CodeGraphEngine>>>;

/// rmcp handler state: the shared engine cache plus the default project /
/// cwd used to resolve a per-call `projectPath`. `no_roots` mirrors the
/// [`crate::McpServer::http`] pin (roots adoption is Phase B; Phase A always
/// runs pinned/no-roots for parity testing).
pub struct CodeGraphHandler {
    engines: EngineCache,
    default_project: Option<PathBuf>,
    cwd: Option<PathBuf>,
    #[allow(dead_code)]
    no_roots: bool,
}

impl CodeGraphHandler {
    pub fn new(default_project: Option<PathBuf>) -> Self {
        Self {
            engines: Arc::new(Mutex::new(HashMap::new())),
            default_project,
            cwd: std::env::current_dir().ok(),
            no_roots: true,
        }
    }

    /// Streamable-HTTP constructor (Phase C): the project is PINNED via
    /// `--path` and roots adoption is OFF (`no_roots`), mirroring
    /// [`crate::McpServer::http`]. Identical state to [`Self::new`] with the
    /// default project set; named separately to make the no_roots/pinned intent
    /// explicit at the HTTP serve site.
    pub fn http(project: PathBuf) -> Self {
        Self::new(Some(project))
    }

    /// Test-only constructor with an explicit cwd (mirrors
    /// [`crate::McpServer::new_with_cwd`]) so the resolution candidates are
    /// exercised deterministically.
    #[doc(hidden)]
    pub fn new_with_cwd(default_project: Option<PathBuf>, cwd: Option<PathBuf>) -> Self {
        Self {
            engines: Arc::new(Mutex::new(HashMap::new())),
            default_project,
            cwd,
            no_roots: true,
        }
    }

    /// Whether the default project has an on-disk index — selects the
    /// `tools/list` schema variant (`has_default_codegraph`, server.rs:249).
    fn has_default_codegraph(&self) -> bool {
        self.default_project
            .as_ref()
            .is_some_and(|p| db_path_for(p).is_file())
    }

    /// Resolve a caller's `projectPath` to an INDEXED project dir, byte-for-byte
    /// the same candidate order as [`crate::McpServer`]'s `resolve_project_arg`
    /// (server.rs:568): absolute raw → cwd-join → bare raw → default-by-basename;
    /// `None` raw → the indexed default. Returns `None` when nothing resolves.
    fn resolve_project_arg(&self, raw: Option<&str>) -> Option<PathBuf> {
        let Some(raw) = raw else {
            return self
                .default_project
                .clone()
                .filter(|p| db_path_for(p).is_file());
        };
        let raw_path = PathBuf::from(raw);
        let mut candidates: Vec<PathBuf> = Vec::new();
        if raw_path.is_absolute() {
            candidates.push(raw_path.clone());
        } else {
            if let Some(cwd) = &self.cwd {
                candidates.push(cwd.join(&raw_path));
            }
            candidates.push(raw_path.clone());
        }
        if let Some(default) = &self.default_project {
            if raw_path.file_name() == default.file_name() {
                candidates.push(default.clone());
            }
        }
        candidates
            .into_iter()
            .find(|candidate| db_path_for(candidate).is_file())
    }
}

/// Convert the raw schema JSON array into a `Vec<rmcp::model::Tool>`, feeding
/// each tool's `inputSchema` in verbatim (NO schemars derive — the `macros`
/// feature is intentionally off).
fn tools_from_schema(tools_json: Value) -> Vec<Tool> {
    let Value::Array(arr) = tools_json else {
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|mut tool| {
            let obj = tool.as_object_mut()?;
            let name = obj.get("name").and_then(Value::as_str)?.to_string();
            let description = obj
                .get("description")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            let input_schema = match obj.remove("inputSchema") {
                Some(Value::Object(map)) => map,
                _ => JsonObject::new(),
            };
            let mut built = Tool::new(
                name,
                description.unwrap_or_default(),
                Arc::new(input_schema),
            );
            if let Some(Value::Object(annotations)) = obj.remove("annotations") {
                built = built.annotate(
                    serde_json::from_value(Value::Object(annotations)).unwrap_or_default(),
                );
            }
            Some(built)
        })
        .collect()
}

/// Map an owned engine [`ToolResult`] to an rmcp [`CallToolResult`], preserving
/// the text content and the `isError` flag (parity with the hand-rolled path).
fn tool_result_to_call_result(result: &ToolResult) -> CallToolResult {
    let content: Vec<ContentBlock> = result
        .content
        .iter()
        .map(|c| ContentBlock::text(c.text.clone()))
        .collect();
    if result.is_error == Some(true) {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// Open-or-get-cached engine + execute + render — the ENTIRE Decision-10 unit,
/// run inside a `spawn_blocking` closure. Takes owned inputs (an `Arc` cache
/// clone, owned project path, tool name, args) and returns an owned
/// [`ToolResult`]; no `&self` / engine borrow crosses the closure boundary.
fn execute_owned(
    engines: &EngineCache,
    project_path: &Path,
    tool_name: &str,
    args: &Value,
) -> ToolResult {
    #[cfg(test)]
    if tool_name == PANIC_TOOL {
        panic!("simulated tool handler panic (Q5-unwind test)");
    }

    let mut guard = engines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !guard.contains_key(project_path) {
        match CodeGraphEngine::open(project_path) {
            Ok(engine) => {
                guard.insert(project_path.to_path_buf(), engine);
            }
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to open project at {}: {e}",
                    project_path.display()
                ));
            }
        }
    }
    let engine = guard.get(project_path).expect("engine present after open");
    engine.execute(tool_name, args)
}

impl ServerHandler for CodeGraphHandler {
    fn get_info(&self) -> ServerInfo {
        // capabilities = exactly {"tools":{}} (enable_tools, NO list_changed);
        // protocolVersion forced to V_2024_11_05 (rmcp defaults to LATEST);
        // serverInfo{name,version=crate}; instructions reused verbatim.
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(SERVER_NAME, SERVER_VERSION))
            .with_instructions(SERVER_INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let defs = if self.has_default_codegraph() {
            schemas::visible_tool_definitions()
        } else {
            schemas::visible_tool_definitions_requiring_project_path()
        };
        Ok(ListToolsResult::with_all_items(tools_from_schema(defs)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tool_name = request.name.to_string();

        // Unknown tool → JSON-RPC -32602 (keep our own lookup so rmcp's built-in
        // -32601 method-not-found never fires); `__panic__` is test-only-known.
        let known = schemas::is_known_tool(&tool_name) || (cfg!(test) && tool_name == "__panic__");
        if !known {
            return Err(ErrorData::invalid_params(
                format!("Unknown tool: {tool_name}"),
                None,
            ));
        }

        let args = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));

        let raw_project = args.get("projectPath").and_then(Value::as_str);
        let project_path = match self.resolve_project_arg(raw_project) {
            Some(p) => p,
            None => {
                let message = match raw_project {
                    Some(raw) => format!(
                        "No indexed project found for projectPath {raw:?}. Pass an absolute path to an indexed project, or run `codegraph init` there."
                    ),
                    None => "No indexed project resolved. Pass a `projectPath` argument, run `codegraph init` in the project, or start the server with `--path <project>`.".to_string(),
                };
                return Ok(tool_result_to_call_result(&ToolResult::error(message)));
            }
        };

        // Decision 10: open+execute+render entirely inside ONE spawn_blocking
        // closure returning an OWNED ToolResult; nothing borrows &self.
        let engines = Arc::clone(&self.engines);
        let join = tokio::task::spawn_blocking(move || {
            execute_owned(&engines, &project_path, &tool_name, &args)
        })
        .await;

        // Decision 9 / Q5-unwind: a panic inside the closure surfaces as a
        // JoinError; map it to an isError result so the process survives.
        match join {
            Ok(result) => Ok(tool_result_to_call_result(&result)),
            Err(join_err) if join_err.is_panic() => Ok(tool_result_to_call_result(
                &ToolResult::error("tool handler panicked"),
            )),
            Err(join_err) => Err(ErrorData::internal_error(
                format!("tool task failed: {join_err}"),
                None,
            )),
        }
    }
}

/// Serve `CodeGraphHandler` over stdio via rmcp, building a multi-thread tokio
/// runtime (the sync engine work runs on `spawn_blocking` pool threads). Blocks
/// until the client disconnects (EOF). Phase A wires this behind the `rmcp`
/// feature; the CLI `--mcp2` integration is Phase D.
pub fn serve_stdio_rmcp(project: Option<PathBuf>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let handler = CodeGraphHandler::new(project);
        let running = handler
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| anyhow::anyhow!("rmcp stdio serve failed: {e}"))?;
        running
            .waiting()
            .await
            .map_err(|e| anyhow::anyhow!("rmcp stdio serve join failed: {e}"))?;
        Ok::<(), anyhow::Error>(())
    })
}

/// Serve `CodeGraphHandler` over streamable-HTTP via rmcp's
/// [`StreamableHttpService`], PINNED to `project` in `no_roots` mode (Phase C —
/// the Zed-remote path). Builds a multi-thread tokio runtime, binds an axum
/// listener on `addr`, and blocks until the process is signalled.
///
/// The service runs in stateless `json_response` mode: every POST to `/mcp`
/// returns a single `application/json` body (no SSE). That is sound here because
/// no_roots mode never emits a server-initiated message — so there is nothing to
/// stream — and it is the shape a plain MCP url client (e.g. Zed's `url` entry)
/// consumes directly. The listening address is logged to STDERR (never stdout,
/// which stays pure protocol).
pub fn serve_http(project: PathBuf, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let db = db_path_for(&project);
        eprintln!(
            "[CodeGraph MCP] streamable-HTTP serving on http://{addr}/mcp (project={}, db={}, db_exists={})",
            project.display(),
            db.display(),
            db.is_file(),
        );

        let handler_project = project.clone();
        let service: StreamableHttpService<CodeGraphHandler, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(CodeGraphHandler::http(handler_project.clone())),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default()
                    .with_stateful_mode(false)
                    .with_json_response(true)
                    .with_sse_keep_alive(None),
            );

        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("binding streamable-HTTP listener on {addr}: {e}"))?;
        axum::serve(listener, router)
            .await
            .map_err(|e| anyhow::anyhow!("streamable-HTTP serve failed: {e}"))?;
        Ok::<(), anyhow::Error>(())
    })
}
