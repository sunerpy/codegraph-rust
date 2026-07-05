//! rmcp `ServerHandler` for CodeGraph — the official-SDK stdio transport,
//! running ALONGSIDE the hand-rolled [`crate::McpServer`].
//!
//! Reuses the sync engine + rendering layer verbatim: [`CodeGraphEngine`],
//! [`crate::schemas`], [`crate::instructions::SERVER_INSTRUCTIONS`], and the
//! `roots` project resolution. Only the transport/session shell is rmcp.
//!
//! ## Sync engine bridge
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
//! ## Panic isolation
//!
//! With `[profile.release] panic = "unwind"`, a panic inside the
//! `spawn_blocking` closure surfaces as `JoinError::is_panic()`, which this maps
//! to an `isError` [`CallToolResult`] — a tool bug returns an error and the
//! process/runtime stays alive (parity with the sync stdio server).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use serde_json::{Value, json};

use crate::engine::CodeGraphEngine;
use crate::instructions::SERVER_INSTRUCTIONS;
use crate::protocol::ToolResult;
use crate::roots::{WorkspaceRoots, db_path_for};
use crate::schemas;

const SERVER_NAME: &str = "codegraph";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Test-only tool name that forces the `spawn_blocking` closure to panic, so
/// the Q5-unwind panic→error mapping can be exercised (see `tests/rmcp_l3.rs`).
/// Gated on the `test-hooks` feature (auto-enabled for this crate's dev/test
/// builds) because `cfg!(test)` is false in the lib when linked by an
/// integration-test binary.
#[cfg(feature = "test-hooks")]
const PANIC_TOOL: &str = "__panic__";

/// Test-only tool name whose `spawn_blocking` closure SLEEPS well past any sane
/// per-call timeout, so the [`tokio::time::timeout`] wrapper in `call_tool` can
/// be exercised (see `tests/rmcp_l3.rs`). The blocking sleep intentionally
/// out-lives the timeout so the client-side `Elapsed` path fires first.
#[cfg(feature = "test-hooks")]
const SLEEP_TOOL: &str = "__sleep__";

/// Environment variable naming the per-tool-call wall-clock timeout, in whole
/// seconds. See [`parse_tool_timeout`] for the parse contract and
/// [`tool_timeout`] for the effective value.
pub const TOOL_TIMEOUT_ENV: &str = "CODEGRAPH_MCP_TOOL_TIMEOUT_SECS";

/// Default per-tool-call timeout when [`TOOL_TIMEOUT_ENV`] is unset or invalid.
/// 60s is generous — `explore` on a large repo can legitimately take a few
/// seconds — while still bounding the 2h+ wedged-call pathology.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 60;

/// Parse a raw [`TOOL_TIMEOUT_ENV`] value into an optional timeout. Kept pure
/// (no env access) so it is unit-tested without env-race flakiness.
///
/// - `Some("0")` (after trimming) => `None`: the explicit opt-out escape hatch,
///   meaning NO timeout (unbounded, the pre-fix behavior for users who want it);
/// - `Some(n)` for a valid `u64` `n > 0` => `Some(Duration::from_secs(n))`;
/// - `None`, empty, whitespace-only, or unparseable => the
///   [`DEFAULT_TOOL_TIMEOUT_SECS`] default (a timeout, never unbounded on a typo).
///
/// The asymmetry is deliberate: unbounded is opt-in ONLY via a literal `0`, so
/// a fat-fingered value degrades to the safe bounded default rather than hanging.
pub fn parse_tool_timeout(raw: Option<&str>) -> Option<Duration> {
    let default = Some(Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS));
    let Some(raw) = raw else {
        return default;
    };
    let trimmed = raw.trim();
    match trimmed.parse::<u64>() {
        Ok(0) => None,
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => default,
    }
}

/// Effective per-tool-call timeout, read from [`TOOL_TIMEOUT_ENV`] via
/// [`parse_tool_timeout`]. `None` means unbounded (the `0` opt-out).
fn tool_timeout() -> Option<Duration> {
    parse_tool_timeout(std::env::var(TOOL_TIMEOUT_ENV).ok().as_deref())
}

type EngineCache = Arc<Mutex<HashMap<PathBuf, CodeGraphEngine>>>;

/// The default project may be DISPLACED at runtime by roots adoption
/// (`on_initialized`, non-pinned mode), and adoption runs through a `&self`
/// handler, so it lives behind a `Mutex` for interior mutability. In pinned /
/// `no_roots` mode it never changes after construction.
type DefaultProject = Arc<Mutex<Option<PathBuf>>>;

/// rmcp handler state: the shared engine cache plus the default project /
/// cwd used to resolve a per-call `projectPath`. `no_roots` mirrors the
/// [`crate::McpServer::http`] pin — when set, roots adoption is OFF (HTTP /
/// pinned mode); when clear, `on_initialized` may adopt an indexed client root.
pub struct CodeGraphHandler {
    engines: EngineCache,
    default_project: DefaultProject,
    cwd: Option<PathBuf>,
    no_roots: bool,
}

impl CodeGraphHandler {
    pub fn new(default_project: Option<PathBuf>) -> Self {
        Self {
            engines: Arc::new(Mutex::new(HashMap::new())),
            default_project: Arc::new(Mutex::new(default_project)),
            cwd: std::env::current_dir().ok(),
            no_roots: true,
        }
    }

    /// Streamable-HTTP constructor: the project is PINNED via
    /// `--path` and roots adoption is OFF (`no_roots`), mirroring
    /// [`crate::McpServer::http`]. Identical state to [`Self::new`] with the
    /// default project set; named separately to make the no_roots/pinned intent
    /// explicit at the HTTP serve site.
    pub fn http(project: PathBuf) -> Self {
        Self::new(Some(project))
    }

    /// Non-pinned stdio constructor: `no_roots = false`, so
    /// `on_initialized` requests the client's roots and adopts an indexed one
    /// when the current default is displaceable (`roots::` adoption rules). This
    /// is the bare-`serve --mcp` / Zed-local case where `default_project` is a
    /// cwd-derived, possibly unindexed dir.
    pub fn serve_with_roots(default_project: Option<PathBuf>, cwd: Option<PathBuf>) -> Self {
        Self {
            engines: Arc::new(Mutex::new(HashMap::new())),
            default_project: Arc::new(Mutex::new(default_project)),
            cwd,
            no_roots: false,
        }
    }

    /// Test-only constructor with an explicit cwd (mirrors
    /// [`crate::McpServer::new_with_cwd`]) so the resolution candidates are
    /// exercised deterministically.
    #[doc(hidden)]
    pub fn new_with_cwd(default_project: Option<PathBuf>, cwd: Option<PathBuf>) -> Self {
        Self {
            engines: Arc::new(Mutex::new(HashMap::new())),
            default_project: Arc::new(Mutex::new(default_project)),
            cwd,
            no_roots: true,
        }
    }

    fn default_project_snapshot(&self) -> Option<PathBuf> {
        self.default_project
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the default project has an on-disk index — selects the
    /// `tools/list` schema variant (`has_default_codegraph`, server.rs:249).
    fn has_default_codegraph(&self) -> bool {
        self.default_project_snapshot()
            .as_ref()
            .is_some_and(|p| db_path_for(p).is_file())
    }

    /// Resolve a caller's `projectPath` to an INDEXED project dir, byte-for-byte
    /// the same candidate order as [`crate::McpServer`]'s `resolve_project_arg`
    /// (server.rs:568): absolute raw → cwd-join → bare raw → default-by-basename;
    /// `None` raw → the indexed default. Returns `None` when nothing resolves.
    fn resolve_project_arg(&self, raw: Option<&str>) -> Option<PathBuf> {
        let default_project = self.default_project_snapshot();
        let Some(raw) = raw else {
            return default_project.filter(|p| db_path_for(p).is_file());
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
        if let Some(default) = &default_project
            && raw_path.file_name() == default.file_name()
        {
            candidates.push(default.clone());
        }
        candidates
            .into_iter()
            .find(|candidate| db_path_for(candidate).is_file())
    }

    /// Adopt an indexed client workspace root when the current default is
    /// displaceable — the Phase-B behavior. Feeds a `ListRootsResult` (already
    /// serialized to the `{"roots":[…]}` shape) through the SAME `roots::`
    /// rules the hand-rolled server uses (`should_request_roots` gate +
    /// `adopt_from_roots_result`), mutating `default_project` in place. Returns
    /// the adopted root (for the debug trace), or `None` when nothing adopts.
    fn adopt_client_roots(&self, roots_result: &Value) -> Option<PathBuf> {
        let mut guard = self
            .default_project
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old_default = guard.clone();
        let adopted = WorkspaceRoots::new().adopt_from_roots_result(
            &mut guard,
            self.cwd.as_deref(),
            Some(roots_result),
        );
        if let Some(adopted) = &adopted {
            let was = old_default
                .as_deref()
                .map_or_else(|| "none".to_string(), |p| p.display().to_string());
            tracing::debug!(
                adopted = %adopted.display(),
                was = %was,
                "roots: adopted workspace root"
            );
        }
        adopted
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
    #[cfg(feature = "test-hooks")]
    if tool_name == PANIC_TOOL {
        panic!("simulated tool handler panic (Q5-unwind test)");
    }

    #[cfg(feature = "test-hooks")]
    if tool_name == SLEEP_TOOL {
        let secs = args.get("seconds").and_then(Value::as_u64).unwrap_or(30);
        std::thread::sleep(Duration::from_secs(secs));
        return ToolResult::text(format!("slept {secs}s"));
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

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        // HTTP / pinned (`no_roots`) mode NEVER requests roots — leave the pin
        // as-is. Only the non-pinned stdio path adopts.
        if self.no_roots {
            return;
        }

        // The client's declared capabilities live in the negotiated peer info
        // (`context.peer.peer_info()`). Gate on `should_request_roots`, reusing
        // the SAME rule the hand-rolled server uses (adoptable default + the
        // client declaring `capabilities.roots`).
        let Some(peer_info) = context.peer.peer_info() else {
            return;
        };
        let capabilities = serde_json::to_value(&peer_info.capabilities).unwrap_or(Value::Null);
        let init_params = json!({ "capabilities": capabilities });
        let default_project = self.default_project_snapshot();
        if !WorkspaceRoots::new().should_request_roots(
            default_project.as_ref(),
            self.cwd.as_deref(),
            Some(&init_params),
        ) {
            return;
        }

        // `Peer::list_roots` is `#[deprecated]` (SEP-2577); it is still THE
        // mechanism in rmcp 2.1 for a server to ask the client for its roots and
        // has no non-deprecated replacement, so the deprecation is allowed at
        // this one call site (rmcp pinned to 2.1.x; revisit on upgrade).
        #[allow(deprecated)]
        let roots = match context.peer.list_roots().await {
            Ok(result) => result,
            Err(_) => return,
        };
        let roots_value = serde_json::to_value(&roots).unwrap_or(Value::Null);
        let _ = self.adopt_client_roots(&roots_value);
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
        let known = schemas::is_known_tool(&tool_name)
            || (cfg!(feature = "test-hooks")
                && matches!(tool_name.as_str(), "__panic__" | "__sleep__"));
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

        // Handler half of the debug split (paired with the `debug_log_requests`
        // middleware): logs the resolved tool + projectPath — values already in
        // hand here, so no request-body buffering is needed. Emitted at debug
        // level; the EnvFilter/RUST_LOG (and CODEGRAPH_DEBUG back-compat) gates
        // it. The result `isError` is logged after execution below.
        tracing::debug!(
            "{}",
            crate::roots::format_tool_debug_line(
                &tool_name,
                raw_project,
                Some(project_path.as_path()),
                self.cwd.as_deref(),
                self.default_project_snapshot().as_deref(),
            )
        );

        // open+execute+render entirely inside ONE spawn_blocking
        // closure returning an OWNED ToolResult; nothing borrows &self.
        //
        // Per-request timeout (Kiro 2h-hang fix): the JoinHandle is a Future, so
        // `tokio::time::timeout` bounds how long the CLIENT waits for it. CAVEAT:
        // an elapsed timeout does NOT kill the blocking OS thread — the closure
        // keeps running on the blocking pool until it finishes and its result is
        // dropped. That is intentional: we bound the client wait, not the CPU
        // work; interrupting sync SQLite mid-read is unsafe. The client is
        // unblocked fast with an isError result; the orphaned thread drains
        // on its own. `tool_timeout() == None` (env `0`) opts out entirely.
        let engines = Arc::clone(&self.engines);
        let outcome_tool = tool_name.clone();
        let join_future = tokio::task::spawn_blocking(move || {
            execute_owned(&engines, &project_path, &tool_name, &args)
        });

        let timed_out;
        let join = match tool_timeout() {
            Some(dur) => match tokio::time::timeout(dur, join_future).await {
                Ok(join_result) => {
                    timed_out = false;
                    join_result
                }
                Err(_elapsed) => {
                    timed_out = true;
                    Ok(ToolResult::error(format!(
                        "tool call timed out after {}s (raise {TOOL_TIMEOUT_ENV} or narrow the query)",
                        dur.as_secs()
                    )))
                }
            },
            None => {
                timed_out = false;
                join_future.await
            }
        };

        // A panic inside the closure surfaces as a
        // JoinError; map it to an isError result so the process survives.
        let result = match join {
            Ok(result) => result,
            Err(join_err) if join_err.is_panic() => ToolResult::error("tool handler panicked"),
            Err(join_err) => {
                return Err(ErrorData::internal_error(
                    format!("tool task failed: {join_err}"),
                    None,
                ));
            }
        };
        // Handler-half outcome line: tool + isError, the per-call result the
        // HTTP middleware envelope cannot see. EnvFilter/RUST_LOG gates it.
        tracing::debug!(
            tool = %outcome_tool,
            is_error = result.is_error == Some(true),
            timed_out,
            "tool call complete"
        );
        Ok(tool_result_to_call_result(&result))
    }
}

/// Serve `CodeGraphHandler` over stdio via rmcp, building a multi-thread tokio
/// Serve `CodeGraphHandler` over stdio via rmcp, building a multi-thread tokio
/// runtime (the sync engine work runs on `spawn_blocking` pool threads). Blocks
/// until the client disconnects (EOF). This is the CLI `serve --mcp` direct
/// path (the sole stdio transport): the handler runs in roots-adoption mode
/// (`no_roots = false`), so `on_initialized` requests the client's roots and
/// adopts an indexed one when the cwd-derived default is displaceable — parity
/// with the hand-rolled `McpServer::new` direct serve.
pub fn serve_stdio_rmcp(project: Option<PathBuf>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let cwd = std::env::current_dir().ok();
        let handler = CodeGraphHandler::serve_with_roots(project, cwd);
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

/// axum middleware logging one line per HTTP request at debug level: method,
/// path, `Host` header, response status, and elapsed time. Always attached to
/// the streamable-HTTP router (see [`serve_http`]); the debug event is a no-op
/// unless the EnvFilter/RUST_LOG (or CODEGRAPH_DEBUG back-compat) enables debug,
/// so it produces no output when debug is off.
///
/// It deliberately does NOT read the request body: an axum `Request` body is a
/// stream, so parsing `projectPath` / the tool name here would mean buffering
/// the whole body and reconstructing the `Request` — added fragility for data
/// the handler ([`CodeGraphHandler::call_tool`]) already has in hand and logs
/// itself. Middleware = HTTP envelope; handler = tool + project + outcome.
pub async fn debug_log_requests(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let started = std::time::Instant::now();
    let resp = next.run(req).await;
    tracing::debug!(
        %method,
        %path,
        %host,
        status = resp.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "http request"
    );
    resp
}

/// Serve `CodeGraphHandler` over streamable-HTTP via rmcp's
/// [`StreamableHttpService`] in `no_roots` mode. Builds a multi-thread tokio
/// runtime, binds an axum listener on `addr`, and blocks until the process is
/// signalled.
///
/// `default_project` selects the mode: `Some(project)` PINS the server to one
/// project (the Zed-remote / single-project path), so a call without
/// `projectPath` resolves that pinned default. `None` is the GLOBAL mode: no
/// pinned default, every tool call MUST carry its own `projectPath`, and one
/// server serves many projects (the HTTP analog of the Kiro/Qoder bare global
/// entry). HTTP can never adopt client roots, so both modes stay `no_roots`.
///
/// The service runs in stateless `json_response` mode: every POST to `/mcp`
/// returns a single `application/json` body (no SSE). That is sound here because
/// no_roots mode never emits a server-initiated message — so there is nothing to
/// stream — and it is the shape a plain MCP url client (e.g. Zed's `url` entry)
/// consumes directly. The listening address is logged to STDERR (never stdout,
/// which stays pure protocol).
///
/// # DNS-rebinding host guard
///
/// The host guard is **OPEN by default** — with no environment set, any `Host`
/// header is accepted, so the MCP Inspector (`Host: code-server:12025`), Zed,
/// and curl all connect out of the box. Strictness is **opt-in** via a single
/// env var [`ALLOWED_HOSTS_ENV`] (`CODEGRAPH_HTTP_ALLOWED_HOSTS`):
///
/// - unset / empty / whitespace  => allow ALL hosts (same as `*`);
/// - a comma list containing a `*` entry => allow ALL hosts;
/// - a comma list of concrete hosts (e.g. `localhost,code-server:12025`) =>
///   STRICT: rmcp's [`StreamableHttpServerConfig`] enforces an allowlist built
///   from the loopback defaults (`localhost`, `127.0.0.1`, `::1`) PLUS the
///   actual bind authority PLUS the listed hosts; every other `Host` => 403.
///
/// The strict allowlist reuses the actual bind `addr`, so a local client that
/// sends the exact `Host: <bind>:<port>` authority is accepted while arbitrary
/// hosts are rejected — DNS-rebinding protection when you ask for it.
///
/// Back-compat: the legacy [`ALLOW_ANY_HOST_ENV`] (`CODEGRAPH_HTTP_ALLOW_ANY_HOST`)
/// is still honored but now only matters as a lower-precedence signal — see
/// [`host_guard_from_env`]. With nothing set at all the guard is OPEN.
pub fn serve_http(
    default_project: Option<PathBuf>,
    addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::StreamableHttpService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match &default_project {
            Some(project) => {
                let db = db_path_for(project);
                tracing::info!(
                    %addr,
                    project = %project.display(),
                    db = %db.display(),
                    db_exists = db.is_file(),
                    "streamable-HTTP serving on http://{addr}/mcp"
                );
            }
            None => {
                tracing::info!(
                    %addr,
                    "streamable-HTTP serving (global, per-call projectPath) on http://{addr}/mcp"
                );
            }
        }

        let guard = host_guard_from_env();
        match &guard {
            HostGuard::AllowAny => tracing::info!(
                "host guard: OPEN (all hosts) — set {ALLOWED_HOSTS_ENV}=localhost,<host> to restrict"
            ),
            HostGuard::Strict(hosts) => tracing::info!(
                allowed = %hosts.join(", "),
                "host guard: strict"
            ),
        }

        let handler_default = default_project.clone();
        let service: StreamableHttpService<CodeGraphHandler, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(CodeGraphHandler::new(handler_default.clone())),
                Arc::new(LocalSessionManager::default()),
                build_http_config(addr, guard),
            );

        let router = axum::Router::new().nest_service("/mcp", service);
        // Two-part debug split, both emitted at debug level (EnvFilter/RUST_LOG,
        // plus CODEGRAPH_DEBUG back-compat, gates them): THIS middleware logs the
        // HTTP request/response envelope (method/path/Host/status/timing);
        // `call_tool` logs the resolved tool + projectPath + isError. The layer
        // is always attached; its event is a no-op unless debug is enabled, so
        // debug-off produces no HTTP-log output.
        let router = router.layer(axum::middleware::from_fn(debug_log_requests));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("binding streamable-HTTP listener on {addr}: {e}"))?;
        axum::serve(listener, router)
            .await
            .map_err(|e| anyhow::anyhow!("streamable-HTTP serve failed: {e}"))?;
        Ok::<(), anyhow::Error>(())
    })
}

/// Legacy environment name for the host-guard escape hatch. Retained for
/// back-compat and now a lower-precedence signal behind [`ALLOWED_HOSTS_ENV`].
pub const ALLOW_ANY_HOST_ENV: &str = "CODEGRAPH_HTTP_ALLOW_ANY_HOST";

/// Environment name for the opt-in host allowlist. Unset/empty (or a value
/// containing a `*`) leaves the guard OPEN; a concrete comma list turns it strict.
pub const ALLOWED_HOSTS_ENV: &str = "CODEGRAPH_HTTP_ALLOWED_HOSTS";

/// Host-guard policy resolved from the environment.
///
/// `AllowAny` accepts any `Host`; `Strict` restricts to the loopback + bind
/// defaults plus the carried hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostGuard {
    AllowAny,
    Strict(Vec<String>),
}

/// Parse a raw [`ALLOWED_HOSTS_ENV`] value into a [`HostGuard`]. Kept pure (no
/// env access) so parsing is unit-tested without env-race flakiness.
///
/// - `None`, empty, or whitespace-only => [`HostGuard::AllowAny`];
/// - a comma list with ANY trimmed entry equal to `*` => [`HostGuard::AllowAny`];
/// - otherwise => [`HostGuard::Strict`] carrying the trimmed, non-empty entries.
pub fn parse_allowed_hosts(raw: Option<&str>) -> HostGuard {
    let Some(raw) = raw else {
        return HostGuard::AllowAny;
    };
    let entries: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect();
    if entries.is_empty() || entries.iter().any(|entry| entry == "*") {
        return HostGuard::AllowAny;
    }
    HostGuard::Strict(entries)
}

/// Read the [`ALLOW_ANY_HOST_ENV`] escape hatch. `1`/`true`/`yes`/`on`
/// (case-insensitive) resolve to `true`; anything else (including unset) `false`.
pub fn http_allow_any_host_from_env() -> bool {
    matches!(
        std::env::var(ALLOW_ANY_HOST_ENV)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Resolve the effective [`HostGuard`] from the environment. Precedence:
/// [`ALLOWED_HOSTS_ENV`], when set and non-empty, wins via [`parse_allowed_hosts`].
/// Otherwise the guard is OPEN — which subsumes the legacy [`ALLOW_ANY_HOST_ENV`]
/// escape hatch (its only effect was `AllowAny`, now the default), so existing
/// `CODEGRAPH_HTTP_ALLOW_ANY_HOST=1` users keep the open behavior they had.
pub fn host_guard_from_env() -> HostGuard {
    let allowed = std::env::var(ALLOWED_HOSTS_ENV).ok();
    let raw = allowed.as_deref().map(str::trim).filter(|v| !v.is_empty());
    match raw {
        Some(value) => parse_allowed_hosts(Some(value)),
        None => HostGuard::AllowAny,
    }
}

/// Build the streamable-HTTP config for [`serve_http`], centralizing the
/// `allowed_hosts` DNS-rebinding guard so tests exercise the exact production
/// list. rmcp compares each entry as a normalized authority: a bare host matches
/// any port, a `host:port` entry matches that port exactly (IPv6 brackets/case
/// are normalized away). A [`HostGuard::Strict`] list therefore includes the bare
/// loopback hosts, the explicit `host:port` authorities for the actual bind
/// `addr`, and the user-listed hosts verbatim so `Host: <bind>:<port>` and each
/// listed host are accepted. [`HostGuard::AllowAny`] disables the guard entirely.
pub fn build_http_config(
    addr: std::net::SocketAddr,
    guard: HostGuard,
) -> rmcp::transport::streamable_http_server::StreamableHttpServerConfig {
    use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;

    let base = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None);

    let extra = match guard {
        HostGuard::AllowAny => return base.disable_allowed_hosts(),
        HostGuard::Strict(extra) => extra,
    };

    let port = addr.port();
    let defaults = [
        addr.to_string(),
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    base.with_allowed_hosts(defaults.into_iter().chain(extra))
}

#[cfg(test)]
mod timeout_tests {
    use super::{DEFAULT_TOOL_TIMEOUT_SECS, parse_tool_timeout};
    use std::time::Duration;

    #[test]
    fn unset_yields_default() {
        assert_eq!(
            parse_tool_timeout(None),
            Some(Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS))
        );
    }

    #[test]
    fn empty_and_whitespace_yield_default() {
        let default = Some(Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS));
        assert_eq!(parse_tool_timeout(Some("")), default);
        assert_eq!(parse_tool_timeout(Some("   ")), default);
    }

    #[test]
    fn zero_opts_out_of_timeout() {
        assert_eq!(parse_tool_timeout(Some("0")), None);
        assert_eq!(parse_tool_timeout(Some("  0  ")), None);
    }

    #[test]
    fn positive_secs_parse() {
        assert_eq!(parse_tool_timeout(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_tool_timeout(Some(" 120 ")),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn invalid_falls_back_to_default() {
        let default = Some(Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS));
        assert_eq!(parse_tool_timeout(Some("abc")), default);
        assert_eq!(parse_tool_timeout(Some("-1")), default);
        assert_eq!(parse_tool_timeout(Some("3.5")), default);
    }
}

#[cfg(test)]
mod handler_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_dir(tag: &str) -> TempDir {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cg-mcp-h-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    /// Write a placeholder (non-SQLite) db file so `db_path_for(p).is_file()` is
    /// true — resolution treats the dir as indexed, but a real engine open fails.
    fn placeholder_indexed(tag: &str) -> TempDir {
        let dir = unique_dir(tag);
        let db = db_path_for(&dir.path);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"not a real sqlite db").unwrap();
        dir
    }

    #[test]
    fn host_guard_from_env_open_when_unset_or_blank() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: serialized by ENV_LOCK; single-threaded within this body.
        unsafe {
            std::env::remove_var(ALLOWED_HOSTS_ENV);
        }
        assert_eq!(host_guard_from_env(), HostGuard::AllowAny);
        unsafe {
            std::env::set_var(ALLOWED_HOSTS_ENV, "   ");
        }
        assert_eq!(host_guard_from_env(), HostGuard::AllowAny, "blank is open");
        unsafe {
            std::env::remove_var(ALLOWED_HOSTS_ENV);
        }
    }

    #[test]
    fn host_guard_from_env_strict_when_concrete_list() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: serialized by ENV_LOCK; single-threaded within this body.
        unsafe {
            std::env::set_var(ALLOWED_HOSTS_ENV, "localhost,code-server:12025");
        }
        assert_eq!(
            host_guard_from_env(),
            HostGuard::Strict(vec![
                "localhost".to_string(),
                "code-server:12025".to_string()
            ])
        );
        unsafe {
            std::env::remove_var(ALLOWED_HOSTS_ENV);
        }
    }

    #[test]
    fn host_guard_from_env_star_is_open() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: serialized by ENV_LOCK; single-threaded within this body.
        unsafe {
            std::env::set_var(ALLOWED_HOSTS_ENV, "a,*,b");
        }
        assert_eq!(host_guard_from_env(), HostGuard::AllowAny);
        unsafe {
            std::env::remove_var(ALLOWED_HOSTS_ENV);
        }
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn build_http_config_allow_any_disables_guard() {
        let addr: std::net::SocketAddr = "127.0.0.1:9100".parse().unwrap();
        let cfg = build_http_config(addr, HostGuard::AllowAny);
        assert!(
            cfg.allowed_hosts.is_empty(),
            "AllowAny must disable the allowlist"
        );
    }

    #[test]
    fn build_http_config_strict_carries_bind_and_loopback_defaults() {
        let addr: std::net::SocketAddr = "0.0.0.0:12026".parse().unwrap();
        let cfg = build_http_config(
            addr,
            HostGuard::Strict(vec!["code-server:12025".to_string()]),
        );
        let hosts = &cfg.allowed_hosts;
        assert!(!hosts.is_empty(), "strict has an allowlist");
        assert!(hosts.iter().any(|h| h == "0.0.0.0:12026"), "bind authority");
        assert!(
            hosts.iter().any(|h| h == "localhost:12026"),
            "loopback:port"
        );
        assert!(hosts.iter().any(|h| h == "127.0.0.1:12026"));
        assert!(hosts.iter().any(|h| h == "[::1]:12026"));
        assert!(hosts.iter().any(|h| h == "localhost"));
        assert!(hosts.iter().any(|h| h == "127.0.0.1"));
        assert!(hosts.iter().any(|h| h == "::1"));
        assert!(
            hosts.iter().any(|h| h == "code-server:12025"),
            "user host preserved"
        );
    }

    #[test]
    fn tool_result_to_call_result_maps_success_and_error() {
        let ok = tool_result_to_call_result(&ToolResult::text("hello"));
        assert_ne!(ok.is_error, Some(true), "text result is not an error");
        assert_eq!(
            ok.content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect::<String>(),
            "hello"
        );

        let err = tool_result_to_call_result(&ToolResult::error("boom"));
        assert_eq!(err.is_error, Some(true), "error result flags isError");
        let text: String = err
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect();
        assert!(text.contains("boom"), "error text preserved: {text}");
    }

    #[test]
    fn tools_from_schema_empty_on_non_array() {
        assert!(tools_from_schema(json!({ "not": "an array" })).is_empty());
        assert!(tools_from_schema(json!(null)).is_empty());
    }

    #[test]
    fn tools_from_schema_builds_tool_with_annotations() {
        let tools = tools_from_schema(json!([
            {
                "name": "t1",
                "description": "d1",
                "inputSchema": { "type": "object" },
                "annotations": { "title": "T1", "readOnlyHint": true }
            },
            { "name": "t2" }
        ]));
        assert_eq!(tools.len(), 2);
        assert_eq!(&*tools[0].name, "t1");
        assert_eq!(tools[0].description.as_deref(), Some("d1"));
        assert_eq!(&*tools[1].name, "t2");
    }

    #[test]
    fn tools_from_schema_skips_entries_without_name() {
        let tools = tools_from_schema(json!([
            { "description": "no name here" },
            { "name": "keep" }
        ]));
        assert_eq!(tools.len(), 1);
        assert_eq!(&*tools[0].name, "keep");
    }

    #[test]
    fn execute_owned_open_failure_returns_error_result() {
        let project = placeholder_indexed("exec-open-fail");
        let engines: EngineCache = Arc::new(Mutex::new(HashMap::new()));
        let result = execute_owned(&engines, &project.path, "codegraph_search", &json!({}));
        assert_eq!(result.is_error, Some(true));
        let text: String = result.content.iter().map(|c| c.text.clone()).collect();
        assert!(
            text.contains("Failed to open project at"),
            "open-failure message, got: {text}"
        );
    }

    #[test]
    fn new_sets_no_roots_and_snapshots_default() {
        let project = placeholder_indexed("new-default");
        let handler = CodeGraphHandler::new(Some(project.path.clone()));
        assert!(handler.no_roots, "new() pins no_roots");
        assert_eq!(
            handler.default_project_snapshot().as_deref(),
            Some(project.path.as_path())
        );
        assert!(handler.has_default_codegraph(), "placeholder db is indexed");
    }

    #[test]
    fn http_constructor_pins_project() {
        let project = placeholder_indexed("http-ctor");
        let handler = CodeGraphHandler::http(project.path.clone());
        assert!(handler.no_roots);
        assert_eq!(
            handler.default_project_snapshot().as_deref(),
            Some(project.path.as_path())
        );
    }

    #[test]
    fn serve_with_roots_clears_no_roots() {
        let handler = CodeGraphHandler::serve_with_roots(None, Some(PathBuf::from("/tmp")));
        assert!(!handler.no_roots, "serve_with_roots enables adoption");
    }

    #[test]
    fn has_default_codegraph_false_without_index() {
        let dir = unique_dir("no-index");
        let handler = CodeGraphHandler::new(Some(dir.path.clone()));
        assert!(!handler.has_default_codegraph(), "no db file → not indexed");
        let none = CodeGraphHandler::new(None);
        assert!(!none.has_default_codegraph(), "no default → not indexed");
    }

    #[test]
    fn resolve_project_arg_none_returns_indexed_default() {
        let project = placeholder_indexed("resolve-none");
        let handler = CodeGraphHandler::new_with_cwd(Some(project.path.clone()), None);
        assert_eq!(
            handler.resolve_project_arg(None).as_deref(),
            Some(project.path.as_path())
        );
    }

    #[test]
    fn resolve_project_arg_none_unindexed_default_is_none() {
        let dir = unique_dir("resolve-none-unidx");
        let handler = CodeGraphHandler::new_with_cwd(Some(dir.path.clone()), None);
        assert_eq!(handler.resolve_project_arg(None), None);
    }

    #[test]
    fn resolve_project_arg_absolute_indexed_resolves() {
        let project = placeholder_indexed("resolve-abs");
        let raw = project.path.display().to_string();
        let handler = CodeGraphHandler::new_with_cwd(None, None);
        assert_eq!(
            handler.resolve_project_arg(Some(&raw)).as_deref(),
            Some(project.path.as_path())
        );
    }

    #[test]
    fn resolve_project_arg_relative_joins_cwd() {
        let project = placeholder_indexed("resolve-rel");
        let parent = project.path.parent().unwrap().to_path_buf();
        let name = project.path.file_name().and_then(|s| s.to_str()).unwrap();
        let handler = CodeGraphHandler::new_with_cwd(None, Some(parent));
        assert_eq!(
            handler.resolve_project_arg(Some(name)).as_deref(),
            Some(project.path.as_path())
        );
    }

    #[test]
    fn resolve_project_arg_bare_basename_matches_default() {
        let project = placeholder_indexed("resolve-basename");
        let name = project
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_string();
        let cwd = std::env::temp_dir().join("cg-mcp-h-basename-elsewhere");
        let handler = CodeGraphHandler::new_with_cwd(Some(project.path.clone()), Some(cwd));
        assert_eq!(
            handler.resolve_project_arg(Some(&name)).as_deref(),
            Some(project.path.as_path())
        );
    }

    #[test]
    fn resolve_project_arg_bogus_is_none() {
        let project = placeholder_indexed("resolve-bogus");
        let handler = CodeGraphHandler::new_with_cwd(Some(project.path.clone()), None);
        assert_eq!(
            handler.resolve_project_arg(Some("no-such-project-xyz")),
            None
        );
    }

    #[test]
    fn adopt_client_roots_adopts_indexed_root_and_returns_it() {
        let project = placeholder_indexed("adopt-ok");
        let handler = CodeGraphHandler::serve_with_roots(None, None);
        let roots = json!({
            "roots": [
                { "uri": format!("file://{}", project.path.display()), "name": "p" }
            ]
        });
        let adopted = handler.adopt_client_roots(&roots);
        assert_eq!(adopted.as_deref(), Some(project.path.as_path()));
        assert_eq!(
            handler.default_project_snapshot().as_deref(),
            Some(project.path.as_path()),
            "default_project mutated in place"
        );
    }

    #[test]
    fn adopt_client_roots_returns_none_when_no_indexed_root() {
        let handler = CodeGraphHandler::serve_with_roots(None, None);
        let roots = json!({ "roots": [] });
        assert_eq!(handler.adopt_client_roots(&roots), None);
    }

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .canonicalize()
            .expect("workspace root")
    }

    /// Materialize a real indexed project (golden mini db + fixture sources) so
    /// `CodeGraphEngine::open` succeeds — the only way to reach the cache-insert
    /// + `engine.execute` success arm of `execute_owned`.
    fn real_indexed_project(tag: &str) -> TempDir {
        let dir = unique_dir(tag);
        let db = db_path_for(&dir.path);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let root = workspace_root();
        std::fs::copy(root.join("reference/golden/mini/colby.db"), &db).unwrap();
        let fixtures = root.join("crates/codegraph-bench/fixtures/mini");
        for rel in ["src/app.ts", "src/math.ts"] {
            let dst = dir.path.join(rel);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(fixtures.join(rel), &dst).unwrap();
        }
        dir
    }

    #[test]
    fn execute_owned_success_caches_engine_and_runs_tool() {
        let project = real_indexed_project("exec-ok");
        let engines: EngineCache = Arc::new(Mutex::new(HashMap::new()));
        let first = execute_owned(
            &engines,
            &project.path,
            "codegraph_search",
            &json!({ "query": "add" }),
        );
        assert_ne!(first.is_error, Some(true), "search on indexed project");
        assert!(
            engines
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains_key(&project.path),
            "engine cached after first open"
        );
        let second = execute_owned(
            &engines,
            &project.path,
            "codegraph_search",
            &json!({ "query": "add" }),
        );
        assert_ne!(second.is_error, Some(true), "cached engine reused");
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    async fn connect(
        handler: CodeGraphHandler,
    ) -> (
        rmcp::service::RunningService<rmcp::RoleClient, rmcp::model::ClientInfo>,
        tokio::task::JoinHandle<()>,
    ) {
        use rmcp::ServiceExt;
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let server_task = tokio::spawn(async move {
            if let Ok(running) = handler.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let client = rmcp::model::ClientInfo::default()
            .serve(client_io)
            .await
            .expect("rmcp client handshake");
        (client, server_task)
    }

    fn call_params(name: &str, args: Value) -> CallToolRequestParams {
        let params = CallToolRequestParams::new(name.to_string());
        match args.as_object().cloned() {
            Some(map) => params.with_arguments(map),
            None => params,
        }
    }

    #[test]
    fn call_tool_bad_project_path_returns_error_result() {
        rt().block_on(async {
            // A pinned indexed default keeps `list_tools` happy, but a bogus
            // absolute `projectPath` argument resolves to nothing → the
            // `Some(raw)` error-message arm of `call_tool`.
            let project = real_indexed_project("call-bad-pp");
            let handler = CodeGraphHandler::new(Some(project.path.clone()));
            let (client, task) = connect(handler).await;
            let result = client
                .call_tool(call_params(
                    "codegraph_search",
                    json!({ "query": "add", "projectPath": "/no/such/indexed/project/xyz" }),
                ))
                .await
                .expect("bad projectPath yields an isError result, not a transport error");
            assert_eq!(result.is_error, Some(true));
            let text: String = result
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect();
            assert!(
                text.contains("No indexed project found for projectPath"),
                "bad-projectPath message, got: {text}"
            );
            let _ = client.cancel().await;
            let _ = task.await;
        });
    }

    #[test]
    fn call_tool_timeout_disabled_runs_unbounded_path() {
        let _env = TIMEOUT_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rt().block_on(async {
            // `CODEGRAPH_MCP_TOOL_TIMEOUT_SECS=0` opts out of the timeout, so
            // `call_tool` takes the `None => join_future.await` (unbounded) arm.
            // SAFETY: serialized by TIMEOUT_ENV_LOCK; single-threaded here.
            unsafe {
                std::env::set_var(TOOL_TIMEOUT_ENV, "0");
            }
            let project = real_indexed_project("call-no-timeout");
            let handler = CodeGraphHandler::new(Some(project.path.clone()));
            let (client, task) = connect(handler).await;
            let ok = client
                .call_tool(call_params("codegraph_search", json!({ "query": "add" })))
                .await
                .expect("unbounded call still returns a result");
            assert_ne!(ok.is_error, Some(true), "search on indexed project");
            let _ = client.cancel().await;
            let _ = task.await;
            // SAFETY: serialized by TIMEOUT_ENV_LOCK; restore to unset.
            unsafe {
                std::env::remove_var(TOOL_TIMEOUT_ENV);
            }
        });
    }

    static TIMEOUT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
