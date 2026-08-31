---
name: codegraph
description: >
  Use CodeGraph for ALL codebase navigation and code research on any indexed
  project. Reach for it even when the user doesn't say "codegraph" — for
  "how does X work", "who calls X", "what breaks if I change X", "where is X
  defined", tracing a call flow, onboarding/surveying an area, or
  whole-project analysis: "analyze the project", "explain the architecture",
  "what does this project do". Also trigger when asked to index/initialize a
  codebase, or when .codegraph/ is present. Prefer codegraph_* tools over
  grep, find, or Read on source files — one call returns verbatim source plus
  the structural graph, replacing dozens of grep+read round-trips. Chinese
  triggers: "分析项目代码", "分析这个仓库", "代码分析", "这个项目是做什么的".
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

### Check index health first

Do not infer readiness from the presence of `.codegraph/` alone: the directory
may contain a current index, an interrupted build, or legacy/corrupt state.
Use `codegraph_status` (MCP) or the CLI before deciding what to run:

```bash
codegraph status /path/to/project --json
```

The status result is authoritative for initialization, extraction compatibility,
pending changes, and any explicit recovery command.

### Initialize, sync, or rebuild

```bash
# Preferred: one-line installer (downloads prebuilt binary, no Rust needed)
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
# Windows PowerShell
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex

# Fallback: build from source (requires Rust toolchain)
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs

# Choose from status + intent
codegraph init /path/to/project          # no usable index exists
codegraph sync /path/to/project          # ordinary added/changed/deleted files
codegraph index /path/to/project         # intentional full rebuild
codegraph index --force /path/to/project # only when status/CLI explicitly prescribes it
```

Use `sync` for routine manual catch-up. Supported extraction-version upgrades may
also escalate through `sync`; do not jump to a forced rebuild merely because the
CLI version changed. Follow the exact recovery command printed by `status` or the
failed command.

Missing index-state slots and an all-`OwnerMismatch` index after a project move
or copy are supported `init` recovery cases. When `status`, `sync`, or another
failed command explicitly prints `codegraph init ...`, run that exact command to
replace the stale index; do not keep retrying `sync`. Do not generalize this to
mixed or other corruption when the CLI does not prescribe `init`.

Extraction is fail-closed per file at a deterministic logical named-node depth
of 256. If an adversarial or generated file exceeds that limit, CodeGraph records
one file error, contributes no partial nodes/edges/references for that file, and
continues indexing the rest of the project. A later `sync` also removes any graph
previously produced by that file. Do not retry with `index --force`: a full
rebuild intentionally produces the same file-level result.

### Start the MCP server

```bash
codegraph serve --mcp            # resolves project from cwd (recommended)
codegraph serve --mcp -p /path   # pin to a specific project
```

The server speaks newline-delimited JSON-RPC over stdio. It watches the project
directory and incrementally syncs file changes after a debounce. When the watcher
is healthy, do not run a manual sync after every edit. Each project still needs
one usable index, normally created with `codegraph init`.

Auto-register into all detected agents (Claude Code, Cursor, Codex, opencode,
Hermes, Gemini CLI, Antigravity, Kiro, Trae, Qoder, Zed, Zuno, VS Code,
Copilot CLI, JetBrains):

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

These are MCP tool names. When MCP is unavailable and you are invoking the
binary directly, use `codegraph search "<symbol>" -p /path/to/project` for
symbol lookup. `codegraph query` remains a compatibility alias; prefer
`codegraph search` in new commands and guidance.

### `codegraph_explore` — start here

For a whole-project overview ("analyze the project", "explain the architecture",
"what does this project do"), start with `codegraph_explore` (architecture
survey) rather than reading files one by one — one call maps the landscape;
follow up only for specific symbols that need deeper detail.

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

CLI equivalent:

```bash
codegraph search "<symbol>" -p /path/to/project
```

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
before starting a large research session. If the watcher is expected, wait for
its debounce and re-check status. If no watcher is running, use `codegraph sync`;
do not default to a full `codegraph index` for ordinary source changes.

---

## Fallback rules — when to use Read/grep instead

CodeGraph indexes source code. Fall back to the Read tool or grep for:

- Config files, TOML, YAML, JSON, Markdown, lock files, data files
- Files flagged in the stale-index banner
- Anything outside the indexed source tree (the `.codegraph/` dir itself,
  build artifacts, vendored binaries)

For everything else in an indexed project, prefer the codegraph tools. The
token savings compound quickly across a long session.
