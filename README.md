# CodeGraph-Rust

[![CI](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/sunerpy/codegraph-rust/branch/main/graph/badge.svg)](https://codecov.io/gh/sunerpy/codegraph-rust)
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

## Table of Contents

- [Quickstart](#quickstart)
- [Installation](#installation)
  - [One-liner install (scripts)](#one-liner-install-scripts)
  - [Prebuilt binaries](#prebuilt-binaries)
  - [With cargo (from git)](#with-cargo-from-git)
- [MCP Quick-Register](#mcp-quick-register)
- [Install the Agent Skill](#install-the-agent-skill-codegraph-skill)
- [Using CodeGraph in IDEs](#using-codegraph-in-ides)
- [CodeGraph for Zed (extension)](#codegraph-for-zed-extension)
- [Using CodeGraph with an LLM](#using-codegraph-with-an-llm)
- [Daemon, Watch & Configuration](#daemon-watch--configuration)
- [CLI Subcommands](#cli-subcommands)
- [Shell Completion](#shell-completion)
- [What CodeGraph Does (and Doesn't)](#what-codegraph-does-and-doesnt)
- [Supported Languages](#supported-languages)
- [Documentation](#documentation)
- [License](#license)

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
codegraph init  /path/to/project                 # create .codegraph/ and run the first index
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
opencode, Hermes, Gemini CLI, Antigravity, Kiro, Trae, Qoder, Zed):

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

**Default (no `-p`):** one config works for all your projects — each just needs
`codegraph index` first. The server resolves the project from the client's working
directory or `rootUri`/`workspaceFolders`/`roots`. When it can't resolve one,
`projectPath` is marked required per tool call.
**Optional `-p <path>`:** pin to one fixed project
(e.g. `"args": ["serve", "--mcp", "-p", "/abs/path/to/project"]`).

See [`docs/mcp.md`](docs/mcp.md#project-resolution) for the full three-case breakdown.

Supported agents: Claude Code, Cursor, Codex CLI, opencode, Hermes Agent,
Gemini CLI, Antigravity IDE, Kiro, Trae, Qoder, Zed.

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

Ten of the eleven install targets have a skill directory (Claude Code, Cursor,
Codex CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro, Trae,
Qoder; `zed` is MCP-only and has no skill directory).
Default location is `--global`; pass `--local` to write into the project tree.
Hermes supports global only.

**Update behavior.** `skill update` compares the installed file's content hash
against the embedded version. An unmodified file is refreshed automatically; a
hand-edited file is skipped with a "locally modified" note (pass `--force` to
overwrite). A small sidecar file (`.codegraph-skill.json`) records the installed
hash so the update check can distinguish "outdated" from "locally modified".

Full reference including per-agent skill paths: [`docs/cli.md`](docs/cli.md).

---

## Using CodeGraph in IDEs

`codegraph install` registers the MCP server entry for each supported agent/IDE.
Run it once and every indexed project is immediately available. How well the index
stays live depends on whether the IDE expands `${workspaceFolder}`:

- **Cursor / Trae** — global config uses `--path ${workspaceFolder}`, so one entry auto-follows every project window. Live watch enabled.
- **Kiro / Qoder** — global entry without `--path`; tools work read-only off the existing index. Run `codegraph init --target=kiro` (or `--target=qoder`) inside each project for live watch.
- **Zed** — global `settings.json` entry without `--path`. Run `codegraph init --target=zed` inside each project to write a `.zed/settings.json` with an absolute `--path` — the only way to give Zed a per-project path. The installer also writes `//`-commented HTTP and SSH alternatives for remote-development use.
- **Other agents** (Claude Code, Codex CLI, opencode, Hermes, Gemini CLI, Antigravity) — standard `mcpServers` entry; live watch where the daemon can reach the project.

> **Zed Remote (SSH).** Zed runs MCP `context_servers` on the local client, not on
> the remote host. If codegraph tools return empty in a remote SSH session, use the
> `//`-commented SSH bridge or HTTP alternative the installer wrote into your
> `settings.json`. HTTP (`codegraph serve --http`, then `http://localhost:8111/mcp`)
> is the recommended remote transport. See
> [`docs/mcp.md` — Zed over SSH](docs/mcp.md#zed-over-ssh-remote-development).

Full per-IDE config details and the `${workspaceFolder}` matrix: [`docs/mcp.md`](docs/mcp.md).

---

## CodeGraph for Zed (extension)

A standalone Zed extension lives under [`editors/zed/`](editors/zed/). It
registers CodeGraph as a `context_servers` context server inside Zed and
downloads the right platform binary automatically — no separate install step
needed.

### Install

**Preferred — official registry (once published):**

Search for **"CodeGraph"** in Zed's extension registry (`zed: extensions` from the
command palette) and click Install. The extension auto-downloads the CodeGraph
binary for your platform on first launch.

> The extension is being submitted to the
> [`zed-industries/extensions`](https://github.com/zed-industries/extensions)
> registry. Once accepted it will be searchable there. Until then, use the
> dev-install path below.

**Dev install (before publication / for local development):**

1. Clone this repository.
2. In Zed, open the command palette and run **`zed: install dev extension`**.
3. Select the `editors/zed/` directory.

Zed compiles the extension to WebAssembly and registers a `codegraph` context
server. On first launch it downloads the latest CodeGraph release binary for your
platform.

### Auto-update and binary cache location

The extension never pins a CodeGraph version. On each launch it resolves the
**latest** `sunerpy/codegraph-rust` GitHub release, picks the asset matching your
platform, downloads and extracts it, then caches the binary at:

```
codegraph-<version>/codegraph        # Linux / macOS
codegraph-<version>/codegraph.exe    # Windows
```

This path is **relative to the extension's working directory** that Zed manages
(inside Zed's extensions data directory, typically
`~/.local/share/zed/extensions/installed/codegraph/` on Linux,
`~/Library/Application Support/Zed/extensions/installed/codegraph/` on macOS, or
`%APPDATA%\Zed\extensions\installed\codegraph\` on Windows).

For example, after downloading version `v0.25.0` on Linux the binary lives at:

```
~/.local/share/zed/extensions/installed/codegraph/codegraph-v0.25.0/codegraph
```

When the CodeGraph CLI ships a new release the extension picks up the new binary
automatically on the next launch — **no extension re-publish or manual update
required**. If the GitHub API is unreachable, the extension falls back to the
newest cached binary it finds.

### Override with your own binary

If you already have `codegraph` installed via the CLI, or want to pin a specific
project path, add a `command` override in your project's `.zed/settings.json`.
The extension uses it verbatim and skips the download:

```jsonc
{
  "context_servers": {
    "codegraph": {
      "command": {
        "path": "codegraph",
        "args": ["serve", "--mcp", "--path", "/abs/path/to/project"],
        "env": {},
      },
    },
  },
}
```

Or let the installer write it for you:

```bash
cd /your/project
codegraph init --target=zed     # writes .zed/settings.json with absolute --path
```

See [`editors/zed/README.md`](editors/zed/README.md) for the full extension
reference, and [`docs/mcp.md`](docs/mcp.md#zed----context_servers-config) for the
Zed `context_servers` config shape.

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

CodeGraph spawns a shared background daemon for each indexed project when you run
`codegraph serve --mcp`. Multiple MCP clients (terminal tabs, agents) share that
one daemon via a Unix socket (`.codegraph/daemon.sock`). It exits once all clients
disconnect and the idle timeout elapses.

Key operations:

```bash
codegraph unlock [path]        # clear a stale daemon lock (keeps live pids)
codegraph serve --http         # HTTP MCP transport, binds 127.0.0.1:8111 by default
codegraph http list            # table of running HTTP servers
codegraph http stop <addr>     # terminate one HTTP server by address
```

Set `CODEGRAPH_NO_DAEMON=1` to force foreground mode (useful in CI). The daemon
watches files with a 2 s debounce; pass `--no-watch` or set `CODEGRAPH_NO_WATCH=1`
to disable. Custom extension mapping goes in `.codegraph/codegraph.json`; exclude
patterns in `.codegraph/config.toml` under `[indexing] exclude`.

Full env-var table, HTTP server details, filesystem fallback behavior, and the
Claude prompt-hook: [`docs/mcp.md`](docs/mcp.md) and
[`docs/cli.md`](docs/cli.md#daemon-watch--environment-variables).

---

## CLI Subcommands

Core commands: `init`, `index`, `sync`, `query`, `files`, `status`, `serve`,
`callers`, `callees`, `impact`, `affected`, `check`, `export`, `unlock`.

Agent / install commands: `install`, `uninstall`, `skill`, `self-update`,
`completions` (`--install` sets up tab completion for bash/zsh/fish/powershell/elvish).

Path convention: most traversal commands accept the project path as a positional
argument or `-p/--path`; `query`/`files`/`serve` use `-p/--path`.

> **Full reference with all flags:** [`docs/cli.md`](docs/cli.md)

---

## Shell Completion

```bash
codegraph completions bash --install        # Bash
codegraph completions zsh --install         # Zsh
codegraph completions fish --install        # Fish
codegraph completions powershell --install  # PowerShell
codegraph completions elvish --install      # Elvish
```

Omit `--install` to print to stdout. Full per-shell install paths and notes:
[`docs/cli.md`](docs/cli.md).

---

## What CodeGraph Does (and Doesn't)

**Does:** deterministic code-structure extraction across 35 languages (TypeScript,
Python, Go, Rust, Java, C/C++, C#, Vue, Svelte, GDScript, and more — see
[`docs/languages.md`](docs/languages.md)), cross-file resolution (including
Godot project graphs), graph traversal, FTS5 search, whole-graph export with
deterministic PageRank centrality, MCP/CLI surfaces, and golden byte-stable
output.

**Doesn't:** no AI / vector / embedding / LLM anywhere inside the binary (hard
constraint, `scripts/guardrail.sh`-enforced); no semantic search; no languages
beyond the fixed `LANGUAGES` set.

---

## Supported Languages

CodeGraph supports **35 languages** grouped by extraction depth. Quick overview:

- **Tier 1 — Full symbol extraction (26):** TypeScript, TSX, JavaScript, JSX, ArkTS, Python, Go, Rust, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Dart, Scala, Lua, Luau, Objective-C, R, Solidity, Nix, GDScript, Pascal.
- **Tier 2 — Embedded / template extraction (6):** Vue, Svelte, Astro, Razor/`.cshtml`, Liquid, XML/MyBatis mapper.
- **Tier 3 — File-level only (3):** YAML, Twig, Properties.

Full list with extensions and per-language notes: [`docs/languages.md`](docs/languages.md).

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
  details, IDE per-agent config matrix, HTTP server, env-var reference.
- [`examples/`](examples/) — codegraph + LLM orchestration example.
- [`editors/zed/README.md`](editors/zed/README.md) — Zed extension reference.
- [`docs/readme/README.zh-CN.md`](docs/readme/README.zh-CN.md) — 中文说明.

---

## License

MIT — see [`LICENSE-MIT`](LICENSE-MIT).
