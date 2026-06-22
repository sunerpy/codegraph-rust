# CodeGraph-Rust

[![CI](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

> A deterministic **code knowledge graph**: tree-sitter parsing persisted to
> SQLite/FTS5, exposing traversable symbol / call / dependency relationships to
> AI coding agents and developers.

> 中文说明 / Chinese: [`docs/readme/README.zh-CN.md`](docs/readme/README.zh-CN.md)

CodeGraph reads a codebase, extracts symbols and their relationships with
tree-sitter, persists them to a per-project SQLite database (FTS5 search
included), and exposes the result through a CLI and an MCP stdio server.
No AI/LLM anywhere inside the binary — output is byte-stable and deterministic.

---

## Quickstart

**Install (Linux / macOS):**

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

**Install (Windows, PowerShell):**

```powershell
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

**Index a project and query it:**

```bash
codegraph init  /path/to/project          # create .codegraph/ and run the first index
codegraph query "<symbol>" -p /path/to/project   # full-text search
codegraph serve --mcp --path /path/to/project    # MCP server for AI agents
```

---

## Installation

The CLI package is **`codegraph-rs`** — a single binary named `codegraph`. SQLite
is compiled in (no system SQLite needed). Not published to crates.io.

<details>
<summary><b>🤖 For AI agents / LLMs — read this first</b></summary>

You are an AI coding agent. CodeGraph is a deterministic code knowledge graph
(tree-sitter + SQLite/FTS5) that answers structural questions about a codebase —
"who calls X", "what does changing X break", "where is X", "how does this area
work" — in **one sub-millisecond query instead of dozens of grep + file reads**.
It returns verbatim source plus the caller/callee/impact graph, so you get more
accurate context in far fewer tokens and round-trips. No AI/LLM inside it — it is
pure pre-computed structure for _you_ to consume.

**Install + index a project:**

```bash
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs   # binary: `codegraph`
codegraph init  /path/to/project     # create the index DB (.codegraph/)
codegraph index /path/to/project     # parse + build the graph
```

**Use it as an MCP server (recommended for agents).** It speaks MCP over stdio:

```bash
codegraph serve --mcp --path /path/to/project
```

Auto-register it into your agent's config (Claude Code, Cursor, Codex CLI,
opencode, Hermes, Gemini CLI, Antigravity, Kiro):

```bash
codegraph install --yes              # detects installed agents and wires them up
```

**MCP tools you can call** (prefer these over grep/read for indexed source):

| tool                                      | use it for                                                                                                                           |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `codegraph_explore`                       | PRIMARY — "how does X work", architecture, a flow, surveying an area. One call returns the relevant symbols' source grouped by file. |
| `codegraph_search`                        | locate a symbol by name (kind + file:line + signature)                                                                               |
| `codegraph_node`                          | read a symbol/file's verbatim source + its caller/callee trail (a smarter `Read`)                                                    |
| `codegraph_callers` / `codegraph_callees` | who calls this / what this calls                                                                                                     |
| `codegraph_impact`                        | blast radius of changing a symbol (transitive)                                                                                       |
| `codegraph_files` / `codegraph_status`    | list a directory / check index readiness                                                                                             |

**Rules of thumb:** reach for `codegraph_explore` _before_ reading files; trust
its results (full AST parse — don't re-verify with grep); use `codegraph_impact`
for refactor blast-radius rather than walking callers by hand. The index lags
file writes by ~1s; tool responses flag any stale files.

</details>

### One-liner install (scripts)

The quickest path — the scripts detect your platform, download the right binary,
and put it on your PATH:

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh

# Windows (PowerShell 5.1+)
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

Set `CODEGRAPH_VERSION=v0.3.0` to pin a specific release instead of the latest.

### Prebuilt binaries

Download an archive from the
[Releases](https://github.com/sunerpy/codegraph-rust/releases) page, extract it,
and place `codegraph` on your PATH. Assets are named
`codegraph-<version>-<target>.<ext>`:

| Platform | Arch                    | Target                       | Ext     |
| -------- | ----------------------- | ---------------------------- | ------- |
| Linux    | x86_64 (static, musl)   | `x86_64-unknown-linux-musl`  | .tar.gz |
| Linux    | aarch64 (static, musl)  | `aarch64-unknown-linux-musl` | .tar.gz |
| macOS    | x86_64                  | `x86_64-apple-darwin`        | .tar.gz |
| macOS    | aarch64 (Apple Silicon) | `aarch64-apple-darwin`       | .tar.gz |
| Windows  | x86_64                  | `x86_64-pc-windows-msvc`     | .zip    |
| Windows  | aarch64 (ARM64)         | `aarch64-pc-windows-msvc`    | .zip    |

Linux builds are statically linked (musl) — no glibc or SQLite system dependency.

### With cargo (from git)

```bash
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

For a full source build (optimized binary + developer targets), see
[`docs/architecture.md`](docs/architecture.md) or run `make help`.

---

## MCP Quick-Register

Add to your agent's MCP config (or run `codegraph install --yes` to do it
automatically):

```jsonc
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp", "-p", "/abs/path/to/your/project"],
    },
  },
}
```

Supported agents: Claude Code, Cursor, Codex CLI, opencode, Hermes Agent,
Gemini CLI, Antigravity IDE, Kiro.

```bash
codegraph install --yes                          # auto-detect installed agents
codegraph install --target=claude,cursor --yes   # explicit list
codegraph install --target=auto --local          # project-local config
```

Full MCP tool reference: [`docs/mcp.md`](docs/mcp.md).

---

## Using CodeGraph with an LLM

CodeGraph has **no built-in LLM**, but it's designed to feed one. The division of
labor: CodeGraph produces deterministic structural facts (call graph, blast radius,
centrality) at sub-millisecond speed; your LLM reasons over the already-located
small context.

Two patterns: **MCP** (agent calls codegraph tools directly) or **backend
orchestration** (your service calls `export`/`explore`, assembles a prompt, feeds
your LLM). A runnable example:

```bash
python examples/llm_orchestration.py --repo . --query "how does indexing work"
```

See [`examples/llm_orchestration.py`](examples/llm_orchestration.py) for the full
script. Neither pattern touches the no-AI guardrail — that only bars embedding an
LLM library _inside_ the codegraph binary itself.

---

## CLI Subcommands

Core commands: `init`, `index`, `sync`, `query`, `files`, `status`, `serve`,
`callers`, `callees`, `impact`, `affected`, `check`, `export`, `unlock`.

Agent / install commands: `install`, `uninstall`, `self-update`, `completions` (`--install` sets up Tab completion for bash/zsh/fish/powershell/elvish).

> **Full reference with flags:** [`docs/cli.md`](docs/cli.md)

Path convention: most traversal commands (`init`/`index`/`status`/`callers`/etc.)
accept the project path as a positional argument or `-p/--path`; `query`/`files`/
`serve` use `-p/--path`.

---

## What CodeGraph Does (and Doesn't)

**Does:** deterministic code-structure extraction across ~28 languages (TypeScript,
Python, Go, Rust, Java, C/C++, C#, Vue, Svelte, and more — see
[`docs/grammar-manifest.md`](docs/grammar-manifest.md)), cross-file resolution,
graph traversal, FTS5 search, whole-graph export with deterministic PageRank
centrality, MCP/CLI surfaces, and golden byte-stable output.

**Doesn't:** no AI / vector / embedding / LLM anywhere inside the binary (hard
constraint, `scripts/guardrail.sh`-enforced); no semantic search; no languages
beyond the fixed `LANGUAGES` set.

---

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — crate dependency graph,
  extraction/resolution/traversal/search pipelines, daemon/watch lifecycle.
- [`docs/data-model.md`](docs/data-model.md) — SQLite/FTS5 storage contract.
- [`docs/equivalence.md`](docs/equivalence.md) — the 3-tier equivalence oracle,
  golden regeneration, `KNOWN_DIFFS.md` format.
- [`docs/grammar-manifest.md`](docs/grammar-manifest.md) /
  [`docs/embedded-extraction.md`](docs/embedded-extraction.md) — language support
  and extraction tiers.
- [`docs/cli.md`](docs/cli.md) — full CLI subcommand reference (20 subcommands,
  all flags).
- [`docs/mcp.md`](docs/mcp.md) — MCP server protocol, all 10 tools, JSON-RPC
  details.
- [`examples/`](examples/) — codegraph + LLM orchestration example.
- [`docs/readme/README.zh-CN.md`](docs/readme/README.zh-CN.md) — 中文说明.

---

## License

MIT — see [`LICENSE-MIT`](LICENSE-MIT).
