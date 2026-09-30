<div align="center">

# CodeGraph-Rust

**Deterministic code intelligence for agents and developers.**

Tree-sitter extraction, SQLite/FTS5 search, graph traversal, CLI, and MCP in one
native binary. No AI or vector runtime inside the indexer.

[![CI](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sunerpy/codegraph-rust)](https://github.com/sunerpy/codegraph-rust/releases)
[![Codecov](https://codecov.io/gh/sunerpy/codegraph-rust/branch/main/graph/badge.svg)](https://codecov.io/gh/sunerpy/codegraph-rust)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)

[English](README.md) · [简体中文](docs/readme/README.zh-CN.md) ·
[Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md)

</div>

## Why CodeGraph

CodeGraph turns a source tree into a local knowledge graph: symbols become nodes;
calls, imports, inheritance, ownership, references, and type relationships become
edges. The graph is persisted per project and can be queried without asking an
LLM to rediscover structure through repeated text searches.

- **Deterministic:** no model calls, embeddings, or vector search; canonical graph
  output is guarded by byte-stable golden fixtures.
- **Source-aware:** search, callers/callees, impact, file source, and multi-file
  exploration use one indexed representation.
- **Agent-ready:** an MCP server exposes the same graph and verbatim source used by
  the CLI.
- **Local-first:** the index lives under the project; the shared daemon and HTTP
  transport are local processes.
- **Broad language coverage:** grammar-backed languages, embedded/template files,
  and framework-aware Godot/Tauri/JS ecosystem relationships share one schema.
- **Incremental:** `sync` and the watcher update changed files while preserving the
  same final canonical result as a clean index.

Exact language and static-analysis boundaries are documented in
[`docs/languages.md`](docs/languages.md).

## Install

### Verified release installer

The installers select the platform archive, require its entry in `SHA256SUMS`,
verify the checksum, and then install the binary.

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

```powershell
# Windows PowerShell 5.1+
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

Pin an exact release when reproducibility matters:

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/vX.Y.Z/scripts/install.sh \
  | CODEGRAPH_VERSION=vX.Y.Z sh
```

```powershell
$env:CODEGRAPH_VERSION = "vX.Y.Z"
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/vX.Y.Z/scripts/install.ps1 | iex
```

### Prebuilt archives

GitHub Releases publish these archive families:

| Platform            | Target                       | Archive   |
| ------------------- | ---------------------------- | --------- |
| Linux x86_64        | `x86_64-unknown-linux-musl`  | `.tar.gz` |
| Linux ARM64         | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS Intel         | `x86_64-apple-darwin`        | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin`       | `.tar.gz` |
| Windows x86_64      | `x86_64-pc-windows-msvc`     | `.zip`    |
| Windows ARM64       | `aarch64-pc-windows-msvc`    | `.zip`    |

Each release also includes `SHA256SUMS`. GitHub CLI users can additionally verify
an archive's build provenance:

```bash
gh attestation verify codegraph-X.Y.Z-x86_64-unknown-linux-musl.tar.gz \
  --repo sunerpy/codegraph-rust \
  --signer-workflow sunerpy/codegraph-rust/.github/workflows/release.yml \
  --deny-self-hosted-runners
```

### Build from Git

The project is not published to crates.io:

```bash
cargo install --locked --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

The installed executable is `codegraph`; SQLite is bundled.

## Quickstart

Create an index, check its state, then ask a structural question:

```bash
cd /path/to/project
codegraph init .
codegraph status . --json
codegraph search "main" -p .
codegraph explore "startup and configuration flow" -p .
```

Lifecycle commands take a positional project path. Research commands take one
query or target plus `-p/--path`. For example:

```bash
codegraph sync .
codegraph node "ReferenceResolver" -p .
codegraph callers "ReferenceResolver" -p .
codegraph impact "ReferenceResolver" -p .
```

If a command rejects an argument, run `codegraph <command> --help`; do not abandon
an available index because a lifecycle and research command use different path
syntax.

## CLI

Common workflows:

```bash
# Index lifecycle
codegraph status . --json
codegraph sync .
codegraph index .                 # full rebuild of the selected project

# Research
codegraph search "symbol" -p .
codegraph explore "area or flow" -p .
codegraph node "symbol-or-id" -p .
codegraph files -p . --format tree

# Relationships
codegraph callers "symbol" -p .
codegraph callees "symbol" -p .
codegraph impact "symbol" -p .

# Operations
codegraph serve --mcp --path .
codegraph serve --http --path .
codegraph mcp list
codegraph http list
```

`query` remains an alias for `search`. For every command and flag, see
[`docs/cli.md`](docs/cli.md).

## MCP

Register the stdio server manually:

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

Or let the installer update supported agent configurations:

```bash
codegraph install --yes
codegraph install --yes --init
codegraph install --target=codex,claude,kiro --yes
```

A server launched without `--path` can serve an existing index selected by an
explicit per-call `projectPath`, client roots, or deterministic workspace
adoption. Pin `--path` when a project should be the default rather than
supplied per call.

The default visible MCP surface emphasizes exploration, file/symbol reads,
search, and callers. Additional known tools can be enabled with
`CODEGRAPH_MCP_TOOLS`. Tool schemas, project resolution, stdio/HTTP behavior,
and protocol compatibility are in [`docs/mcp.md`](docs/mcp.md).

## Agents and IDEs

`codegraph install` supports common coding agents and IDE integrations. Some
clients can expand a workspace variable in global configuration; others require a
project-local absolute path for live watching. The installer prints the relevant
caveat rather than guessing.

```bash
codegraph install --target=auto --global --yes
codegraph install --target=auto --local --yes
codegraph init --target=kiro .
codegraph init --target=zed .
```

The optional embedded skill teaches an agent to use CodeGraph before grep/read:

```bash
codegraph skill install --yes
codegraph skill status
codegraph skill update --dry-run --diff
```

<details>
<summary>Compact guidance for coding agents</summary>

1. Run `codegraph status <project> --json` before research.
2. Use `codegraph_explore` first for architecture, a bug, or a flow.
3. Use `codegraph_search` to locate one name and `codegraph_node` to read one
   symbol or indexed file with its caller/callee trail.
4. Use `codegraph_impact` before changing a shared symbol.
5. Trust the structural index; re-read only files explicitly reported stale.
6. Use `codegraph sync <project>` for ordinary catch-up. Rebuild only when status
   or a requested operation requires it.

</details>

Full target and configuration matrices:
[`docs/cli.md`](docs/cli.md), [`docs/mcp.md`](docs/mcp.md), and
[`editors/zed/README.md`](editors/zed/README.md).

## Determinism and safety

The compatibility contract includes stable node IDs, canonical golden artifacts,
SQLite schema parity, deterministic resolution and ordering, project-contained
filesystem access, and fail-closed ambiguity. Incremental output is tested against
a clean full index.

CodeGraph reports static evidence, not runtime certainty. Reflection, registries,
event buses, framework conventions, generated code, and dynamic dispatch can
continue beyond a known edge. Absence of a static reference is not proof that code
is dead.

For schema and oracle details, read [`docs/data-model.md`](docs/data-model.md) and
[`docs/equivalence.md`](docs/equivalence.md). Security reports should follow
[`SECURITY.md`](SECURITY.md).

## Performance

Performance is measured by the committed benchmark harness on pinned corpora. A
valid result records the implementation commits, environment, cache policy, run
count, median, dispersion, and query percentiles. This README intentionally makes
no floating latency claim.

Methodology and the current result status are in
[`docs/benchmark.md`](docs/benchmark.md) and
[`docs/benchmark-results.md`](docs/benchmark-results.md).

## Development

The pinned Rust toolchain and locked dependency graph are repository contracts.
Start with:

```bash
git clone https://github.com/sunerpy/codegraph-rust.git
cd codegraph-rust
make hooks
make check
```

`make check` and `make ci` run the same complete local quality path; `make pre-ci` adds a local package/unpack/execute smoke. Focused targets are available through `make help`. Contributors should read
[`CONTRIBUTING.md`](CONTRIBUTING.md) and the canonical agent contract in
[`AGENTS.md`](AGENTS.md).

## Documentation

- [`docs/README.md`](docs/README.md) — documentation map
- [`docs/architecture.md`](docs/architecture.md) — workspace and runtime design
- [`docs/cli.md`](docs/cli.md) — complete command reference
- [`docs/mcp.md`](docs/mcp.md) — MCP transports, tools, and clients
- [`docs/languages.md`](docs/languages.md) — language coverage and boundaries
- [`docs/equivalence.md`](docs/equivalence.md) — deterministic golden contract
- [`docs/upstream-sync/UPSTREAM.md`](docs/upstream-sync/UPSTREAM.md) — upstream ledger
- [`docs/troubleshooting.md`](docs/troubleshooting.md) — diagnostic workflow

## License

MIT — see [`LICENSE-MIT`](LICENSE-MIT).
