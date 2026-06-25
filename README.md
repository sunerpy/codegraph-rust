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
codegraph serve --mcp --path /path/to/project    # MCP server (--path optional, defaults to cwd)
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

**Install + index a project** — prefer the one-line installer (downloads a
prebuilt binary; no Rust toolchain, no compile wait):

```bash
# Linux / macOS — one-click install (recommended)
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
# Windows (PowerShell 5.1+)
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex

# Fallback — build from source (only if you have a Rust toolchain)
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs   # binary: `codegraph`

codegraph init  /path/to/project     # create the index DB (.codegraph/)
codegraph index /path/to/project     # parse + build the graph
```

> 中文：优先用上面的一键安装脚本（直接下载预编译二进制，无需 Rust 工具链、无需编译等待）；
> `cargo install --git` 仅作为有 Rust 环境时的回退方案。

**Use it as an MCP server (recommended for agents).** It speaks MCP over stdio:

```bash
codegraph serve --mcp                        # defaults to cwd (recommended: use codegraph install)
codegraph serve --mcp --path /path/to/project  # optional: pin to a specific project
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

```bash
codegraph install --yes                          # auto-detect installed agents
codegraph install --target=claude,cursor --yes   # explicit list
codegraph install --target=auto --local          # project-local config
```

Full MCP tool reference: [`docs/mcp.md`](docs/mcp.md).

---

## Install the Agent Skill (`codegraph skill`)

Beyond wiring the MCP server, CodeGraph can install a `SKILL.md` directly into
each agent's skill directory. The skill teaches your agent to use CodeGraph for
code research and project onboarding — reach for `codegraph_explore` before
grep/read, use `codegraph_node` instead of a plain file read on indexed source,
and run `codegraph init` when no `.codegraph/` index is present yet.

```bash
codegraph skill install --yes              # install into all detected agents (global)
codegraph skill install --target=claude,cursor --yes  # explicit list
codegraph skill install --target=auto --local         # project-local skill dirs
codegraph skill update                     # refresh skill if unchanged by user
codegraph skill update --force             # overwrite even locally-modified files
codegraph skill uninstall --target=claude --yes       # remove from one agent
codegraph skill status                     # show state for all detected agents
```

All eight supported agents have a skill directory (Claude Code, Cursor, Codex
CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro). Default
location is `--global`; pass `--local` to write into the project tree. Hermes
supports global only.

**Update behavior.** `skill update` compares the installed file's content hash
against the embedded version. An unmodified file is refreshed automatically; a
hand-edited file is skipped with a "locally modified" note (pass `--force` to
overwrite). A small sidecar file (`.codegraph-skill.json`) records the installed
hash so the update check can distinguish "outdated" from "locally modified".

Full reference including per-agent skill paths: [`docs/cli.md`](docs/cli.md).

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

## Daemon, Watch & Configuration

### Shared detached daemon

When you run `codegraph serve --mcp` in a project that has a `.codegraph/`
directory, CodeGraph automatically spawns a single shared background daemon
instead of running in-process. Every MCP client that connects to the same project
(multiple terminal tabs, multiple agents) shares that one daemon via a Unix socket
(`.codegraph/daemon.sock`). The daemon exits on its own once all clients have
disconnected and the idle timeout elapses.

**Logs and stale locks.** The daemon appends stdout and stderr to
`.codegraph/daemon.log`. If a daemon crashes and leaves a stale lock, run:

```bash
codegraph unlock [path]   # clears the stale lock; live daemon pids are preserved
```

**Skipping the daemon.** Set `CODEGRAPH_NO_DAEMON=1` to force foreground
(direct) mode regardless of project state. Useful in CI or scripts where you
don't want a background process.

### Live file watch

The daemon watches your project for file changes and re-indexes automatically.
The debounce window defaults to 2 s (`CODEGRAPH_WATCH_DEBOUNCE_MS`). On WSL2,
watching files under `/mnt/` is auto-disabled (recursive `fs.watch` is too slow
on those paths); set `CODEGRAPH_FORCE_WATCH=1` to override that. To disable
watching entirely, pass `--no-watch` or set `CODEGRAPH_NO_WATCH=1`.

### Environment variables

| Variable                           | Default   | Clamp range  | Meaning                                                        |
| ---------------------------------- | --------- | ------------ | -------------------------------------------------------------- |
| `CODEGRAPH_NO_DAEMON`              | —         | —            | Force foreground direct mode; never spawn/proxy a daemon       |
| `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` | `300000`  | 1000–3600000 | Exit after this long with no connected clients                 |
| `CODEGRAPH_DAEMON_MAX_IDLE_MS`     | `1800000` | 1000–3600000 | Hard cap on total daemon lifetime when idle                    |
| `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS` | `30000`   | 50–600000    | How often the daemon sweeps for dead clients                   |
| `CODEGRAPH_WATCH_DEBOUNCE_MS`      | `2000`    | 100–60000    | File-change debounce before re-index triggers                  |
| `CODEGRAPH_NO_WATCH`               | —         | —            | Disable the live file watcher (equiv. `serve --no-watch`)      |
| `CODEGRAPH_FORCE_WATCH`            | —         | —            | Override WSL2 `/mnt/` auto-disable (won't override `NO_WATCH`) |

Full launcher decision order and deeper reference: [`docs/cli.md`](docs/cli.md#daemon-watch--environment-variables).

### Custom extension mapping (`.codegraph/codegraph.json`)

To tell CodeGraph how to parse files with non-standard extensions, add a config
file at `.codegraph/codegraph.json`:

```jsonc
{
  "extensions": {
    ".x": "lua",
    ".blade": "php",
  },
}
```

Keys are dot-stripped and lowercased before matching. Unknown language names are
silently skipped. When multiple configs exist up the directory tree, the nearest
one wins. A malformed file is ignored (logged).

### Opt-in Claude prompt hook

`codegraph install --prompt-hook` writes a `UserPromptSubmit` hook into Claude
Code's config. Before each prompt, the hook calls `codegraph prompt-hook`, which
runs `codegraph_explore` against the nearest index and prepends relevant context
to the prompt automatically. This is **off by default** and is never implied by
`--yes`. Other agents are unaffected.

```bash
codegraph install --prompt-hook          # add the hook to Claude Code only
codegraph install --yes --prompt-hook    # wire all agents + add Claude hook
```

---

## CLI Subcommands

Core commands: `init`, `index`, `sync`, `query`, `files`, `status`, `serve`,
`callers`, `callees`, `impact`, `affected`, `check`, `export`, `unlock`.

Agent / install commands: `install`, `uninstall`, `skill`, `self-update`, `completions` (`--install` sets up Tab completion for bash/zsh/fish/powershell/elvish).

> **Full reference with flags:** [`docs/cli.md`](docs/cli.md)

Path convention: most traversal commands (`init`/`index`/`status`/`callers`/etc.)
accept the project path as a positional argument or `-p/--path`; `query`/`files`/
`serve` use `-p/--path`.

---

## What CodeGraph Does (and Doesn't)

**Does:** deterministic code-structure extraction across 32 languages (TypeScript,
Python, Go, Rust, Java, C/C++, C#, Vue, Svelte, GDScript, and more — see
[`docs/languages.md`](docs/languages.md)), cross-file resolution including
Godot scene/resource/autoload graphs (see [`docs/godot.md`](docs/godot.md)),
graph traversal, FTS5 search, whole-graph export with deterministic PageRank
centrality, MCP/CLI surfaces, and golden byte-stable output.

**Doesn't:** no AI / vector / embedding / LLM anywhere inside the binary (hard
constraint, `scripts/guardrail.sh`-enforced); no semantic search; no languages
beyond the fixed `LANGUAGES` set.

---

## Supported Languages

CodeGraph supports **32 languages** grouped by extraction depth:

**Tier 1 — Full symbol extraction (23):** TypeScript, TSX, JavaScript, JSX,
Python, Go, Rust, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Dart, Scala, Lua,
Luau, Objective-C, R, GDScript, Pascal.

**Tier 2 — Embedded / template extraction (6):** Vue (delegates `<script>` to
TS/JS), Svelte (delegates script blocks), Astro, Razor/`.cshtml`, Liquid
(Shopify templates + sections), XML/MyBatis mapper.

**Tier 3 — File-level only (3):** YAML, Twig, Properties — indexed as file
nodes; no symbol extraction.

Full list with extensions and per-language notes: [`docs/languages.md`](docs/languages.md).

### Godot projects

For Godot projects (detected by the presence of `project.godot`), CodeGraph
statically understands scene/resource/autoload structure beyond plain GDScript:
`project.godot` autoload singletons, `.tscn` node trees and signal connections,
`.tres` resource references, and dynamic GDScript dispatch patterns
(`connect`/`get_node`/`$`/`%`/`call`/group methods). Computed targets that
cannot be confirmed statically are surfaced as `godot:dynamic:…` sentinels
rather than fabricated edges. CodeGraph does static impact analysis only — it
does not run the engine or verify runtime behavior. See
[`docs/godot.md`](docs/godot.md) for the full reference and the
static-vs-runtime division of labor.

---

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — crate dependency graph,
  extraction/resolution/traversal/search pipelines, daemon/watch lifecycle.
- [`docs/data-model.md`](docs/data-model.md) — SQLite/FTS5 storage contract.
- [`docs/equivalence.md`](docs/equivalence.md) — the 3-tier equivalence oracle,
  golden regeneration, `KNOWN_DIFFS.md` format.
- [`docs/languages.md`](docs/languages.md) — full supported-language list grouped
  by extraction depth.
- [`docs/godot.md`](docs/godot.md) — Godot static analysis: what CodeGraph
  extracts from `.tscn`/`.tres`/`project.godot`/`.gd`, the static-vs-runtime
  boundary, and honesty signals for dynamic reachability.
- [`docs/grammar-manifest.md`](docs/grammar-manifest.md) /
  [`docs/embedded-extraction.md`](docs/embedded-extraction.md) — language support
  and extraction tiers (engineering ABI detail).
- [`docs/cli.md`](docs/cli.md) — full CLI subcommand reference (22 subcommands,
  all flags).
- [`docs/mcp.md`](docs/mcp.md) — MCP server protocol, all 10 tools, JSON-RPC
  details.
- [`examples/`](examples/) — codegraph + LLM orchestration example.
- [`docs/readme/README.zh-CN.md`](docs/readme/README.zh-CN.md) — 中文说明.

---

## License

MIT — see [`LICENSE-MIT`](LICENSE-MIT).
