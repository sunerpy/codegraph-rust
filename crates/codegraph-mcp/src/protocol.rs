//! JSON-RPC 2.0 wire types for the MCP stdio transport.
//!
//! Ports the message/error shapes from
//! `upstream mcp/transport.ts:23-65` (newline-delimited JSON-RPC).
//! The transport framing is one JSON object per line (NOT LSP Content-Length);
//! see `transport.ts:4-5` and the readline loop at `transport.ts:276-309`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC error codes used by the upstream transport
/// (`upstream mcp/transport.ts:59-65`).
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// An incoming JSON-RPC message. Requests carry an `id`; notifications omit it.
///
/// Mirrors `JsonRpcRequest` (`transport.ts:23-28`); `id` and `params` are
/// optional so the same struct decodes both requests and notifications.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// A JSON-RPC error object (`transport.ts:43-47`).
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// An outgoing JSON-RPC response (`transport.ts:33-38`). Exactly one of
/// `result`/`error` is populated, matching the upstream `sendResult`/`sendError`.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcErrorObject>,
}

impl JsonRpcResponse {
    /// `transport.sendResult` (`transport.ts:131-133`): `{jsonrpc,id,result}`.
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// `transport.sendError` (`transport.ts:144-146`): `{jsonrpc,id,error}`.
    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcErrorObject {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// The MCP tool-call result content payload
/// (`upstream mcp/tools.ts:353-359`).
///
/// Success: `{ content: [{ type: 'text', text }] }`.
/// Error:   `{ content: [{ type: 'text', text: "Error: <msg>" }], isError: true }`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
    /// Structured "the lookup found nothing" signal for a non-error result — set
    /// at the engine's genuine not-found/empty branches so callers can gate on
    /// it (`--strict`) WITHOUT substring-matching the rendered text, which would
    /// misfire on a matched node whose source body contains a sentinel phrase.
    /// Skipped when None, so the MCP wire shape is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "notFound")]
    pub not_found: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

impl ToolResult {
    /// `ToolHandler.textResult` (`tools.ts:3432-3436`).
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                content_type: "text",
                text: text.into(),
            }],
            is_error: None,
            not_found: None,
        }
    }

    /// A non-error result whose text is a genuine "found nothing" sentinel;
    /// flags `not_found` so `--strict` callers gate on it without inspecting the
    /// text. Wire shape matches [`Self::text`] (the flag is skipped when None).
    pub fn not_found_text(text: impl Into<String>) -> Self {
        Self {
            not_found: Some(true),
            ..Self::text(text)
        }
    }

    /// `ToolHandler.errorResult` (`tools.ts:3438-3442`): prefixes `Error: `
    /// and sets `isError: true`.
    pub fn error(message: impl std::fmt::Display) -> Self {
        Self {
            content: vec![ToolContent {
                content_type: "text",
                text: format!("Error: {message}"),
            }],
            is_error: Some(true),
            not_found: None,
        }
    }
}
