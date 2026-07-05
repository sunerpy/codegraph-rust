//! Minimal stdio MCP server runner for live round-trip QA.
//!
//! Reads the default project root from `$CODEGRAPH_PROJECT` (or the first CLI
//! arg) and runs the rmcp stdio serve loop (the sole MCP transport). The real
//! `serve --mcp` wiring lives in codegraph-cli; this example exists only to
//! prove a live stdio round-trip.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let default_project = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("CODEGRAPH_PROJECT").ok())
        .map(PathBuf::from);
    codegraph_mcp::serve_stdio_rmcp(default_project)
}
