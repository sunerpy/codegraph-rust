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
/// `SERVER_INFO.version` — the upstream package version at the pinned commit
/// (captured live: `serverInfo.version` == "0.9.9").
const SERVER_VERSION: &str = "0.9.9";

/// Holds the default project path and a per-path engine cache (mirrors
/// `ToolHandler.projectCache`, `tools.ts:591`).
pub struct McpServer {
    default_project: Option<PathBuf>,
    engines: HashMap<PathBuf, CodeGraphEngine>,
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
        let dir = std::env::var("CODEGRAPH_DIR").unwrap_or_else(|_| ".codegraph".to_string());
        project.join(dir).join("codegraph.db").is_file()
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
    /// (`ToolHandler.getCodeGraph`, `tools.ts`).
    fn engine_for(&mut self, project_path: &PathBuf) -> anyhow::Result<&CodeGraphEngine> {
        if !self.engines.contains_key(project_path) {
            let engine = CodeGraphEngine::open(project_path)?;
            self.engines.insert(project_path.clone(), engine);
        }
        Ok(self.engines.get(project_path).expect("just inserted"))
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
