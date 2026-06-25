---
name: codegraph
description: >
  Use CodeGraph for ALL codebase navigation and code research on any indexed
  project — "how does X work", "who calls X", "what breaks if I change X",
  "where is X defined", tracing a flow, onboarding an unfamiliar repo, or
  surveying an area. PREFER the codegraph_* tools over grep, find, or the
  Read tool whenever source files are involved, even if the user doesn't say
  "codegraph". One codegraph call beats dozens of grep+read round-trips: it
  returns verbatim source plus the structural graph in a single response. Also
  trigger when the user asks to index or initialize a codebase for an agent,
  or when .codegraph/ is present in the repo root.
---

# CodeGraph — Agent Skill

CodeGraph is a **deterministic** code knowledge graph built on tree-sitter and
SQLite/FTS5. It parses a codebase, extracts symbols and their relationships, and
persists everything to a per-project `.codegraph/` database. There is no AI, no
LLM, no vector store, and no embeddings anywhere inside the binary — output is
byte-stable and fully reproducible.

Use it to answer structural questions (call graph, blast radius, symbol location,
architecture) in one sub-millisecond query rather than dozens of grep + file
reads. You get more accurate context in far fewer tokens and round-trips.

---

## Part A — Onboarding / Initialization

### Detect a live index

A project is indexed when a `.codegraph/` directory exists at its root. Check
for it before reaching for grep or Read on source files:

```
.codegraph/   ← present = indexed; absent = not yet indexed
```

Call `codegraph_status` to confirm the index is ready and to see how many
files, nodes, and edges are loaded.

### Create or refresh an index

```bash
# Preferred: one-line installer (downloads prebuilt binary, no Rust needed)
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
# Windows PowerShell
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex

# Fallback: build from source (requires Rust toolchain)
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs

# Index the project
codegraph init  /path/to/project   # create .codegraph/ and run first index
codegraph index /path/to/project   # re-index after large changes
```

`codegraph init` is idempotent — safe to rerun.

### Start the MCP server

```bash
codegraph serve --mcp            # resolves project from cwd (recommended)
codegraph serve --mcp -p /path   # pin to a specific project
```

The server speaks newline-delimited JSON-RPC over stdio. It watches the project
directory and auto-updates the index when files change, with roughly a 1-second
debounce lag. One server config works for all your projects — each just needs
its own `codegraph index` run.

Auto-register into all detected agents (Claude Code, Cursor, Codex, opencode,
Hermes, Gemini CLI, Antigravity, Kiro):

```bash
codegraph install --yes
```

---

## Part B — Daily Code Research

### Tool selection workflow

Pick your entry point based on what you're trying to answer:

| Question                            | First tool          |
| ----------------------------------- | ------------------- |
| "How does this feature/area work?"  | `codegraph_explore` |
| "Where is symbol X defined?"        | `codegraph_search`  |
| "Show me the source + callers of X" | `codegraph_node`    |
| "Who calls X?"                      | `codegraph_callers` |
| "What does X call?"                 | `codegraph_callees` |
| "What breaks if I change X?"        | `codegraph_impact`  |
| "What files are under path P?"      | `codegraph_files`   |
| "Is the index up to date?"          | `codegraph_status`  |

### `codegraph_explore` — start here

Call this first for any open-ended question: architecture, a flow, "how does
indexing work", "show me the auth layer". One call returns the symbols most
relevant to your query, their verbatim source grouped by file, and the
call/impact graph connecting them. It replaces the grep-then-read-then-grep loop
that would otherwise take 10-20 round-trips.

Trust the results — they come from a full AST parse, not text matching. Don't
re-verify with grep.

### `codegraph_search` — locate a symbol by name

Returns kind, file path, line number, and signature. Use it when you know (or
suspect) a symbol's name but not where it lives. The results are FTS5-scored
across multiple signals (name, kind, path). This is the closest thing to
"semantic" lookup available, but it is **fully deterministic full-text scoring**
— there are no embeddings, no vector index, no neural model of any kind.

### `codegraph_node` — read source + graph trail

Pass a symbol ID (from a `search` result) or a file path. A symbol ID returns
the symbol's verbatim source plus its direct callers and callees. A file path
returns the file's line-numbered source — use this instead of the Read tool for
any indexed source file. It's faster, and the output is pre-annotated with
structural context.

Supports `offset` and `limit` for large files, matching Read's pagination
interface.

### `codegraph_callers` / `codegraph_callees` — directed edges

Use these when you need focused traversal in one direction: all callers of a
function, or all functions a module calls. For broader "what's connected"
questions, `codegraph_explore` is usually more efficient than chaining callers
and callees manually.

### `codegraph_impact` — blast radius before a refactor

Pass a symbol; get back the full transitive set of callers and dependents —
every symbol that would need to change or be verified if you modify the queried
one. Run this before any non-trivial refactor instead of walking the call graph
by hand.

### `codegraph_files` — directory listing

Returns the indexed files under a path as a tree. Useful for orienting yourself
in an unfamiliar project layout without opening the filesystem directly.

### `codegraph_status` — index health

Returns file count, node count, edge count, DB size, and any pending (stale)
files. Call it when in doubt about whether the index reflects the current state
of the codebase.

---

## Stale-index handling

When a tool response begins with:

```
⚠️  N file(s) edited since the last index sync: path/a.rs, path/b.rs
```

those specific files may be out of date. Read them directly with the Read tool
for accurate content. Every file **not** listed in the banner is fresh and can
be trusted from the index without re-reading.

`codegraph_status` also lists any pending files if you want a proactive check
before starting a large research session.

---

## Fallback rules — when to use Read/grep instead

CodeGraph indexes source code. Fall back to the Read tool or grep for:

- Config files, TOML, YAML, JSON, Markdown, lock files, data files
- Files flagged in the stale-index banner
- Anything outside the indexed source tree (the `.codegraph/` dir itself,
  build artifacts, vendored binaries)

For everything else in an indexed project, prefer the codegraph tools. The
token savings compound quickly across a long session.
