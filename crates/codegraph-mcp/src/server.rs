//! Stdio JSON-RPC 2.0 server loop.
//!
//! Ports the session dispatch (`upstream mcp/session.ts:117-232`)
//! and the newline-delimited stdio transport
//! (`upstream mcp/transport.ts:276-309`). One JSON object per line;
//! NOT LSP `Content-Length` framing (`transport.ts:4-5`).
//!
//! The loop is intentionally synchronous: it reads stdin line-by-line, handles
//! each message, and writes one response line. Tool logic stays sync (rusqlite)
//! — no async runtime is required (Task spec §5: async only if load-bearing).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::engine::CodeGraphEngine;
use crate::instructions::SERVER_INSTRUCTIONS;
use crate::protocol::{error_codes, JsonRpcRequest, JsonRpcResponse, ToolResult};
use crate::schemas;

/// `PROTOCOL_VERSION` (`session.ts:34`).
const PROTOCOL_VERSION: &str = "2024-11-05";
/// `SERVER_INFO.name` (`session.ts:28-31`).
const SERVER_NAME: &str = "codegraph";
/// `SERVER_INFO.version` — follows the real crate version (`CARGO_PKG_VERSION`),
/// so it auto-tracks release-please bumps instead of drifting.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The relative `.codegraph/codegraph.db` path under a project root, honoring
/// the `CODEGRAPH_DIR` override (mirrors [`McpServer::has_default_codegraph`]).
fn db_path_for(project_path: &std::path::Path) -> PathBuf {
    let dir = std::env::var("CODEGRAPH_DIR").unwrap_or_else(|_| ".codegraph".to_string());
    project_path.join(dir).join("codegraph.db")
}

/// Stable identity of the on-disk database file, used to tell a REPLACEMENT
/// (a fresh file at the same path) apart from an in-place write. Keyed on the
/// filesystem inode (unix) / `(len, creation-time)` (windows), NOT
/// modified-time: an in-place WAL write bumps mtime while keeping the same
/// inode, and FAT's 2s mtime granularity can miss a fast replace.
///
/// On windows we deliberately avoid the nightly-only unstable
/// `MetadataExt::file-index` accessor and rely on the stable
/// `(len, creation_time())` pair: deleting and recreating the db file resets
/// its creation time, which preserves the #925 replacement-detection intent on
/// stable toolchains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DbIdentity {
    #[cfg(unix)]
    ino: u64,
    /// `(len, creation-time)` identity for non-unix targets. On windows
    /// `creation-time` is `meta.creation_time()` (stable); on other non-unix
    /// targets it is `0` (len only).
    #[cfg(not(unix))]
    fallback: (u64, u64),
}

impl DbIdentity {
    /// Identity of the db file, or `None` when it is missing — which the caller
    /// treats as "must reopen".
    fn read(db_path: &std::path::Path) -> Option<Self> {
        let meta = std::fs::metadata(db_path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self { ino: meta.ino() })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            Some(Self {
                fallback: (meta.len(), meta.creation_time()),
            })
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            Some(Self {
                fallback: (meta.len(), 0),
            })
        }
    }
}

/// A cached engine plus the db-file identity recorded when it was opened.
struct CachedEngine {
    engine: CodeGraphEngine,
    identity: DbIdentity,
}

/// Process-global count of engine reopens (drop the cached engine + open a
/// fresh one because the db file went missing or was replaced). The first open
/// of a never-cached path is not a reopen. `tests/reopen.rs` reads it via
/// [`reopen_count`] to prove a same-inode project triggers no needless reopen.
static REOPEN_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number of engine reopens since process start. Test-observability hook for
/// the #925 replacement rule; cheap enough to keep unconditionally.
pub fn reopen_count() -> u64 {
    REOPEN_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Holds the default project path and a per-path engine cache (mirrors
/// `ToolHandler.projectCache`, `tools.ts:591`). Each cached engine carries the
/// db-file identity it was opened against, so [`McpServer::engine_for`] can
/// reopen when the database is REPLACED on disk (#925).
pub struct McpServer {
    default_project: Option<PathBuf>,
    engines: HashMap<PathBuf, CachedEngine>,
}

impl McpServer {
    pub fn new(default_project: Option<PathBuf>) -> Self {
        Self {
            default_project,
            engines: HashMap::new(),
        }
    }

    /// Whether the default project is indexed (its `.codegraph/codegraph.db`
    /// exists). An unindexed workspace serves an EMPTY `tools/list` — absence
    /// is the one signal an agent can't misread (`hasDefaultCodeGraph` /
    /// `session.ts:222-231`).
    fn has_default_codegraph(&self) -> bool {
        let Some(project) = &self.default_project else {
            return false;
        };
        db_path_for(project).is_file()
    }

    /// Run the stdio loop until EOF. Reads `reader` line-by-line, writes one
    /// response line per request to `writer`. Notifications (no `id`) produce no
    /// output (`session.ts:118` gates every reply on `isRequest`).
    pub fn run<R: BufRead, W: Write>(&mut self, reader: R, mut writer: W) -> anyhow::Result<()> {
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(&line) {
                let serialized = serde_json::to_string(&response)?;
                writeln!(writer, "{serialized}")?;
                writer.flush()?;
            }
        }
        Ok(())
    }

    /// Parse + dispatch one line. Returns `Some(response)` for a request,
    /// `None` for a notification or unparseable notification.
    fn handle_line(&mut self, line: &str) -> Option<JsonRpcResponse> {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => {
                // `transport.ts:167-171`: parse error with a null id.
                return Some(JsonRpcResponse::error(
                    Value::Null,
                    error_codes::PARSE_ERROR,
                    "Parse error: invalid JSON",
                ));
            }
        };
        let id = req.id.clone();
        match self.dispatch(&req) {
            Dispatch::Reply(value) => id.map(|id| JsonRpcResponse::result(id, value)),
            Dispatch::Err(code, msg) => id.map(|id| JsonRpcResponse::error(id, code, msg)),
            Dispatch::Notification => None,
        }
    }

    /// Method dispatch, mirroring `session.ts:119-156`.
    fn dispatch(&mut self, req: &JsonRpcRequest) -> Dispatch {
        let is_request = req.id.is_some();
        match req.method.as_str() {
            "initialize" if is_request => Dispatch::Reply(initialize_result()),
            "initialized" => Dispatch::Notification,
            "notifications/initialized" => Dispatch::Notification,
            "tools/list" if is_request => Dispatch::Reply(json!({
                "tools": if self.has_default_codegraph() {
                    schemas::visible_tool_definitions()
                } else {
                    Value::Array(Vec::new())
                }
            })),
            "tools/call" if is_request => self.handle_tools_call(req),
            "ping" if is_request => Dispatch::Reply(json!({})),
            "resources/list" if is_request => Dispatch::Reply(json!({ "resources": [] })),
            "resources/templates/list" if is_request => {
                Dispatch::Reply(json!({ "resourceTemplates": [] }))
            }
            "prompts/list" if is_request => Dispatch::Reply(json!({ "prompts": [] })),
            _ if is_request => Dispatch::Err(
                error_codes::METHOD_NOT_FOUND,
                format!("Method not found: {}", req.method),
            ),
            _ => Dispatch::Notification,
        }
    }

    /// `handleToolsCall` (`session.ts:204-232`). Validates the tool name; an
    /// unknown name is a JSON-RPC `-32602` error (NOT tool content).
    fn handle_tools_call(&mut self, req: &JsonRpcRequest) -> Dispatch {
        let params = req.params.clone().unwrap_or(Value::Null);
        let tool_name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => {
                return Dispatch::Err(error_codes::INVALID_PARAMS, "Missing tool name".to_string())
            }
        };
        if !schemas::is_known_tool(&tool_name) {
            return Dispatch::Err(
                error_codes::INVALID_PARAMS,
                format!("Unknown tool: {tool_name}"),
            );
        }
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let project_path = args
            .get("projectPath")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or_else(|| self.default_project.clone());

        let project_path = match project_path {
            Some(p) => p,
            None => {
                return Dispatch::Reply(
                    serde_json::to_value(ToolResult::error(
                        "No project path provided and no default project is configured. Pass `projectPath` or launch the server with a project root.",
                    ))
                    .expect("ToolResult serializes"),
                )
            }
        };

        let engine = match self.engine_for(&project_path) {
            Ok(e) => e,
            Err(e) => {
                return Dispatch::Reply(
                    serde_json::to_value(ToolResult::error(format!(
                        "Failed to open project at {}: {e}",
                        project_path.display()
                    )))
                    .expect("ToolResult serializes"),
                )
            }
        };

        let result = engine.execute(&tool_name, &args);
        Dispatch::Reply(serde_json::to_value(result).expect("ToolResult serializes"))
    }

    /// Open-on-demand + cache the engine for a project path
    /// (`ToolHandler.getCodeGraph`, `tools.ts`), reopening when the db file was
    /// REPLACED on disk (#925). Before returning a cached engine, re-stat the db
    /// path: reopen iff it is MISSING or its identity differs from the recorded
    /// one (inode/file-index changed). An in-place write keeps the same identity,
    /// so the common path returns the cached engine without reopening.
    fn engine_for(&mut self, project_path: &PathBuf) -> anyhow::Result<&CodeGraphEngine> {
        let db_path = db_path_for(project_path);
        let current = DbIdentity::read(&db_path);

        let stale = match self.engines.get(project_path) {
            None => true,
            Some(cached) => current != Some(cached.identity),
        };

        if stale {
            if self.engines.remove(project_path).is_some() {
                REOPEN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let engine = CodeGraphEngine::open(project_path)?;
            let identity = DbIdentity::read(&db_path).ok_or_else(|| {
                anyhow::anyhow!("database vanished after open at {}", db_path.display())
            })?;
            self.engines
                .insert(project_path.clone(), CachedEngine { engine, identity });
        }

        Ok(&self
            .engines
            .get(project_path)
            .expect("engine present after open")
            .engine)
    }
}

enum Dispatch {
    Reply(Value),
    Err(i64, String),
    Notification,
}

/// The `initialize` result (`session.ts:182-187`).
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        "instructions": SERVER_INSTRUCTIONS,
    })
}
