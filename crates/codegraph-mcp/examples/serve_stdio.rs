//! Minimal stdio MCP server runner for live round-trip QA.
//!
//! Reads the default project root from `$CODEGRAPH_PROJECT` (or the first CLI
//! arg) and runs the JSON-RPC stdio loop. The real `serve --mcp` wiring lives
//! in codegraph-cli (Task 23); this example exists only to prove a live stdio
//! round-trip for Task 22 evidence.

use std::io::{BufReader, stdin, stdout};
use std::path::PathBuf;

use codegraph_mcp::McpServer;

fn main() -> anyhow::Result<()> {
    let default_project = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("CODEGRAPH_PROJECT").ok())
        .map(PathBuf::from);
    let mut server = McpServer::new(default_project);
    let reader = BufReader::new(stdin().lock());
    server.run(reader, stdout().lock())
}
