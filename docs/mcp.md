# MCP Server Reference

`codegraph serve --mcp` runs a newline-delimited JSON-RPC MCP server over
stdin/stdout. It does **not** use LSP `Content-Length` framing.

Protocol handshake: `initialize` returns
`protocolVersion: "2024-11-05"`, `serverInfo.name: "codegraph"`.

---

## Quick-Register

Add to your agent's MCP config file, or run `codegraph install --yes` to write
it automatically:

```jsonc
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp"],
    },
  },
}
```

**Default (no `-p`):** the MCP server resolves the project from the agent's
working directory, so one config works for all your projects — each just needs
to be indexed with `codegraph index`. **Optional `-p <path>` / `--path <path>`:**
pin the server to one fixed project regardless of cwd (e.g.
`"args": ["serve", "--mcp", "-p", "/abs/path/to/project"]`).

Supported agents: Claude Code, Cursor, Codex CLI, opencode, Hermes Agent,
Gemini CLI, Antigravity IDE, Kiro.

---

## Default vs. Full Tool Set

`tools/list` surfaces only the **4 default tools** by default
(`explore`, `node`, `search`, `callers` — the `DEFAULT_MCP_TOOLS` set). All 10
tools remain callable via `tools/call`. To expose additional tools in `tools/list`,
set the `CODEGRAPH_MCP_TOOLS` environment variable to a comma-separated list of
short names, e.g.:

```bash
CODEGRAPH_MCP_TOOLS=explore,node,search,callers,impact,check codegraph serve --mcp
```

---

## All 10 Tools

| Tool                | Purpose                                                                                                                                 |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `codegraph_explore` | PRIMARY tool: blast radius + relationship map + dynamic-dispatch boundaries + source blocks (output is size-adaptive to project scale). |
| `codegraph_search`  | FTS5 + multi-signal scored symbol search.                                                                                               |
| `codegraph_node`    | Node detail (symbol view) or file view (line-numbered source). A smarter `Read`.                                                        |
| `codegraph_callers` | Callers of a symbol (along calls/references/imports edges).                                                                             |
| `codegraph_callees` | Targets a symbol calls.                                                                                                                 |
| `codegraph_impact`  | Blast radius of changing a symbol (transitive incoming deps).                                                                           |
| `codegraph_status`  | Index status summary (files/nodes/edges/DB size/stale files).                                                                           |
| `codegraph_files`   | List/tree indexed files under a path.                                                                                                   |
| `codegraph_check`   | Circular-dependency detection. Returns each cycle as `a.ts -> b.ts -> a.ts`.                                                            |
| `codegraph_export`  | Whole-graph NetworkX node-link JSON export with optional PageRank centrality.                                                           |

---

## Tool Usage Notes

**`codegraph_explore`** is the primary entry point for agent queries. One call
returns the symbols relevant to a query, their verbatim source grouped by file,
plus the call/impact graph around them. Prefer it over individual `callers`/
`callees` chains when surveying an unfamiliar area.

**`codegraph_node`** accepts either a symbol ID (from a `search` result) or a
file path. When given a file path it returns the file's source with line numbers,
which is a more accurate alternative to a plain `Read` tool call.

**`codegraph_impact`** returns the transitive incoming dependency set — every
symbol that would break if the queried symbol changed. Use it before a refactor
to understand the blast radius instead of walking callers manually.

**`codegraph_check`** returns cycles as ordered lists of file paths. It's
additive: most projects have zero cycles; run it after a large dependency
restructuring to confirm no new cycles were introduced.

**`codegraph_export`** dumps the complete graph as NetworkX node-link JSON.
Useful for external visualization tools, custom analysis scripts, or feeding an
LLM a high-level structural summary of the entire codebase.

---

## Error Channels

Two distinct error channels:

- **Unknown tool name** — JSON-RPC error `-32602` (invalid params).
- **Missing or invalid required argument** — tool result with
  `{content: ..., isError: true}` and `Error: <message>` body.

---

## Stale Index Warning

If indexed files are out of date (the file watcher has not yet caught up), tool
responses include a stale-file warning in the result. The index typically lags
file writes by ~1 second when the daemon is running. Re-run `codegraph index` or
wait for the watcher to sync if you see stale warnings on a hot codebase.
