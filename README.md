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

**Default (no `-p`):** `tools/list` always returns the full tool surface, even
before a project is resolved. When the server resolves a default project — the
working directory is at or inside an indexed project (find-up), or the client
sends `rootUri`/`workspaceFolders`/`roots` — all tools work with `projectPath`
optional, so one config works for all your projects (each just needs to be
indexed with `codegraph index`). When it cannot resolve one (a roots-less client
launched from a fixed directory not inside any project, e.g. a shared global
config using the home directory as cwd), tools are still listed but `projectPath`
is marked required in each tool's schema; the agent must then pass it per call.
**Optional `-p <path>` / `--path <path>`:** pin the server to one fixed project
regardless of cwd — the simpler choice for single-project setups (e.g.
`"args": ["serve", "--mcp", "-p", "/abs/path/to/project"]`).

See [`docs/mcp.md`](docs/mcp.md#project-resolution) for the full three-case
breakdown.

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
How well the index stays live depends on whether the IDE expands `${workspaceFolder}`.

| IDE / Agent | Global config strategy                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Live watch                                                                                              |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------- |
| **Cursor**  | Global `~/.cursor/mcp.json` uses `--path ${workspaceFolder}` — one config auto-follows every project window.                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Live on save (daemon watcher).                                                                          |
| **Trae**    | Global config (`~/.trae-server/data/Machine/mcp.json` in server/remote mode, or `Trae/User/mcp.json` in desktop mode) uses `--path ${workspaceFolder}` — one config auto-follows every project window. Note: project-level `.trae/mcp.json` requires enabling **"Enable project-level MCP / 启用项目级 MCP"** in Trae settings.                                                                                                                                                                                                                              | Live on save (daemon watcher).                                                                          |
| **Kiro**    | Global `~/.kiro/settings/mcp.json` holds a bare `serve --mcp` entry (no `--path`). The agent passes the project path per tool call — tools work read-only off the existing index, but there is no live watch. The written `mcp.json` keeps **stdio as the primary, active transport** and also carries a `//`-commented HTTP alternative you can uncomment; Kiro requires `https` for remote MCP servers and allows `http` **only for localhost**, so that block uses `http://localhost:8111/mcp` (see [HTTP MCP server](#http-mcp-server-background-mode)). | Manual only (see below).                                                                                |
| **Qoder**   | Global entry (`<config_base>/QoderCN\|Qoder/<machineId>/SharedClientCache/mcp.json`) holds a bare `serve --mcp` entry. Tools work read-only off the existing index; the IDE does not expand `${workspaceFolder}` in this layout.                                                                                                                                                                                                                                                                                                                             | Manual only (see below).                                                                                |
| **Zed**     | Global `~/.config/zed/settings.json` (Linux/macOS) or `%APPDATA%\Zed\settings.json` (Windows) holds a bare `context_servers.codegraph` entry (no `--path`). Zed's global config has no `${workspaceFolder}` expansion — tools work read-only off the existing index globally.                                                                                                                                                                                                                                                                                | Manual/read-only globally; run `codegraph init --target=zed` per project for live per-project `--path`. |

**Getting live auto-update in Kiro, Qoder, or Zed.** Run `codegraph init --target=<ide>`
once inside each project:

```bash
cd /your/project
codegraph init --target=kiro    # or --target=qoder
codegraph init --target=zed     # writes .zed/settings.json with absolute --path
```

For Zed specifically, this writes a project-level `.zed/settings.json` with an
absolute `--path` — the **only** way to give Zed a per-project path, since Zed's
global `context_servers` config cannot inject one.

> **Zed Remote (SSH).** Zed runs MCP `context_servers` on the local client, not
> on the remote host — even when a remote SSH project is open. If codegraph tools
> return empty in a remote SSH session, use an `ssh` bridge command instead of
> `command: "codegraph"` directly. See
> [`docs/mcp.md` — Zed over SSH](docs/mcp.md#zed-over-ssh-remote-development)
> for the config and full explanation.

On a fresh (unindexed) project this builds the index and writes a project-level
config with the absolute `--path`. On an already-indexed project it writes (or
refreshes) the project-level config and relies on the daemon's file watcher and
startup catch-up to keep it live. Either way, once the local config is in place
the index stays current as you edit.

Without a project-level config, the index only updates when you run
`codegraph index` or `codegraph sync` manually. This mirrors upstream CodeGraph's
behavior: clients that cannot report a workspace root rely on startup catch-up and
manual reindex rather than per-call sync.

---

## CodeGraph for Zed (extension)

A standalone Zed extension lives under [`editors/zed/`](editors/zed/). It
registers CodeGraph as a `context_servers` context server inside Zed and
downloads the right platform binary automatically — no separate install step
needed.

### Install (dev extension)

1. Clone this repository.
2. In Zed, open the command palette and run **`zed: install dev extension`**.
3. Select the `editors/zed/` directory.

Zed compiles the extension to WebAssembly and registers a `codegraph` context
server. On first launch it downloads the latest CodeGraph release binary for your
platform.

### Auto-update

The extension never pins a CodeGraph version. On each launch it resolves the
**latest** `sunerpy/codegraph-rust` GitHub release, picks the asset matching your
platform, downloads and extracts it, and caches it under a version-stamped path
(`codegraph-<version>/codegraph`). When the CodeGraph CLI ships a new release the
extension picks up the new binary automatically — **no extension re-publish or
manual update required**.

If you already have `codegraph` installed via the CLI, or want to pin a specific
project path, add a `command` override in your project's `.zed/settings.json`:

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

> **Publishing.** The extension is not yet published to the
> [`zed-industries/extensions`](https://github.com/zed-industries/extensions)
> registry. Dev-install via the step above works today; marketplace publish is a
> later manual step.

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

**Filesystems that can't bind a socket.** On ExFAT/FAT, some network mounts,
and WSL DrvFs, binding an `AF_UNIX` socket inside the project directory fails.
The daemon then falls back through a deterministic candidate chain — the
project-dir `.codegraph/daemon.sock` first, then a hashed socket under the
system temp dir — and records the socket it actually bound in the lock file.
The pid/lock file always stays at `.codegraph/daemon.pid`, and clients read the
recorded socket from the lock, so they attach regardless of which candidate the
daemon chose.

### HTTP MCP server (background mode)

Besides the stdio transport, CodeGraph can serve MCP over streamable-HTTP for
web/remote clients (`serve --http`). HTTP servers are keyed by **bind address**
(not by project — a global server with no `--path` spans many projects), so they
have their own address-keyed registry, separate from the per-project daemon.

```bash
codegraph serve --http                                       # one command — binds 127.0.0.1:8111 (default)
codegraph serve --http --http-addr 127.0.0.1:8111            # foreground (blocks; default)
codegraph serve --http --http-addr 127.0.0.1:8111 --detach   # background; prints pid + log, then exits
codegraph http list                                          # table of running servers
codegraph http status [<addr>]                               # detail for one, or all
codegraph http stop 127.0.0.1:8111                           # terminate one by address
```

`codegraph serve --http` alone is the one-command start: `--http-addr` defaults
to `127.0.0.1:8111`, so no address flag is needed. To point an MCP client (e.g.
Kiro's commented alternative) at it, use `http://localhost:8111/mcp`.

**Foreground stays the default** — `serve --http` blocks and serves until you
stop it. Add `--detach` to run it in the background: the parent spawns a
detached child, records it in the registry, prints `started HTTP MCP server on
<addr> (pid N), logs: <path>`, and exits.

**Multi-instance by address.** Two servers on **different** addresses coexist;
starting a second one notes the others. Starting on an **address already in
use** is refused with an error that lists the running instance — stop it with
`codegraph http stop <addr>` or pick a different `--http-addr`.

**Self-healing registry.** One JSON file per running server lives under the
global state dir (`$XDG_STATE_HOME/codegraph/http/`, else
`~/.local/state/codegraph/http/`; `%LOCALAPPDATA%\codegraph\http\` on Windows;
override with `CODEGRAPH_HTTP_REGISTRY_DIR`). Detached-server logs are written
to `<registry_dir>/<addr>.log`. Each entry records the server's pid; on the next
`serve --http`, `http list`, or `http stop`, any entry whose process has died is
pruned automatically, so a crash never leaves a phantom conflict.

Beyond the default `ignore_dirs`, you can skip additional root-relative path
patterns by listing them under `[indexing] exclude` in
`.codegraph/config.toml`:

```toml
[indexing]
exclude = ["static/", "docs/generated", "gen*"]
```

Patterns use the same matcher as `.gitignore` (`static/` for a directory,
`gen*` for a prefix, `docs/generated` for an exact/suffix path) and are honored
by both `index` and `sync`. The list is empty by default, so an existing config
without `exclude` behaves identically.

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
[`docs/languages.md`](docs/languages.md)), cross-file resolution (including
Godot project graphs), graph traversal, FTS5 search, whole-graph export with
deterministic PageRank centrality, MCP/CLI surfaces, and golden byte-stable
output.

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
