//! codegraph-mcp — stdio JSON-RPC 2.0 MCP server.
//!
//! Implements `initialize` / `tools/list` / `tools/call` over newline-delimited
//! stdio (`upstream mcp/{session,transport}.ts`) and the 8 upstream
//! tools (`upstream mcp/tools.ts`): `codegraph_search`,
//! `codegraph_callers`, `codegraph_callees`, `codegraph_impact`,
//! `codegraph_node`, `codegraph_explore`, `codegraph_status`,
//! `codegraph_files`. Each tool delegates to the committed crates
//! (codegraph-graph traversal/query, codegraph-store) with tool logic kept
//! sync. Output is byte-aligned to the upstream at Tier-2 structural; the un-ported
//! explore heuristics are documented in `KNOWN_DIFFS.md`.

pub mod dynamic_boundaries;
pub mod engine;
pub mod explore_budget;
pub mod instructions;
pub mod protocol;
#[cfg(feature = "rmcp")]
pub mod rmcp_handler;
pub(crate) mod roots;
pub mod schemas;
pub mod server;

pub use engine::CodeGraphEngine;
pub use server::{initialize_result, McpServer, RunUntilAdoption};
