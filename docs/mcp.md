# MCP Server Reference

`codegraph serve --mcp` runs a newline-delimited JSON-RPC MCP server over
stdin/stdout. It does **not** use LSP `Content-Length` framing.

Protocol handshake: `initialize` returns
`protocolVersion: "2024-11-05"`, `serverInfo.name: "codegraph"`.
`serverInfo.version` reports the running binary's crate version (from
`CARGO_PKG_VERSION`), so it tracks releases automatically rather than being
hardcoded.

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

## Daemon & live watch

When the project is indexed (`.codegraph/` exists), `serve --mcp` does not run
inline. Instead it spawns — or proxies to — a single shared detached daemon
process per project. Multiple agent clients (e.g. Claude Code + Cursor open
simultaneously) all attach to the same daemon, so the index is loaded and
maintained once.

The daemon runs a file watcher (`codegraph-watch`) that live-reindexes changed
files. Events are debounced (default ~2 s; tunable via
`CODEGRAPH_WATCH_DEBOUNCE_MS`) so a burst of saves triggers one incremental
rebuild rather than many. The watcher is auto-disabled on WSL2 `/mnt/` drives
where recursive watch is too slow; set `CODEGRAPH_FORCE_WATCH=1` to override.
When the resolved root is exactly `$HOME` or the filesystem root (`/`), the
server first disables the daemon, the file watcher, AND catch-up sync — not just
the watcher. This happens when an IDE or agent (e.g. Kiro) launches
`codegraph serve --mcp` with no `--path` and its CWD is the home directory;
without the guard, the server would spawn a daemon that indexes the entire home
tree and peg a CPU at 99%. In this initial safe mode the server still answers all
tool queries off any existing `.codegraph` index, but it will not start
background services against `$HOME`. If the client advertises MCP roots support,
the server then sends `roots/list` and adopts the first indexed root from the
client's response. When that adopted root is indexed, CodeGraph also starts the
shared project daemon for that root, so a single global config can recover the
real project even when the launch CWD was home. `CODEGRAPH_FORCE_WATCH` does
**not** override this guard (it only overrides the WSL2 `/mnt/` disable). A real
project nested under `$HOME` (e.g. `~/projects/myapp`) is unaffected and gets the
full daemon, watcher, and catch-up. To guarantee per-project services for clients
that do not support roots, pin the root via `--path <project>` in the client's
MCP config args (e.g. a workspace-level `.kiro/settings/mcp.json`), or open the
project folder as the working directory.

When `serve --mcp` is started without an explicit `--path`, the server reads the
MCP `initialize` handshake sent by the client and adopts the workspace it
advertises (`rootUri`, `rootPath`, or `workspaceFolders[0].uri`) as its project
root — provided that path is already indexed. If the client does not include
those fields but advertises `capabilities.roots`, the server asks the client for
`roots/list` and adopts the first indexed root from that response. This means a
single global MCP config (one `serve --mcp` entry, no `--path`) correctly serves
whichever project window is connected, without any per-project config.

The daemon exits automatically after all clients disconnect and an idle timeout
elapses. Logs are appended to `.codegraph/daemon.log`. A stale lock (e.g. after
a crash) can be cleared with `codegraph unlock`.

On Unix, the detached daemon calls `setsid` to become a session leader, so when
the short-lived proxy that spawned it exits the daemon is reparented to `init`
and reaped automatically — no `<defunct>` zombie appears in the process table.
The daemon exits when its real host (the IDE or agent running `serve --mcp`)
dies, detected via `host_pid` liveness; raw parent-pid divergence is not used
for this check because a deliberately daemonized process legitimately reparents
to `init`.

To disable the daemon entirely and run the MCP server in the foreground, set
`CODEGRAPH_NO_DAEMON=1`. For the full set of env-var knobs — timeouts, sweep
intervals, watch settings — see [`docs/cli.md`](cli.md).

---

## Stale Index Warning

If indexed files are out of date (the file watcher has not yet caught up), tool
responses include a stale-file warning in the result. The index typically lags
file writes by ~1 second when the daemon is running. Re-run `codegraph index` or
wait for the watcher to sync if you see stale warnings on a hot codebase.
