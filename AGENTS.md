# AGENTS.md — codegraph-rs

A deterministic tree-sitter + SQLite/FTS5 **code knowledge graph**: it parses a codebase,
extracts symbols and their relationships, persists them to a per-project SQLite database
(with an FTS5 search index), and exposes the result through a CLI and an MCP (Model Context
Protocol) stdio server. No AI / vector / LLM anywhere in the binary — output is byte-stable.

## Hard invariants (never break)

- **Golden `.schema` byte-stability** — verified by `crates/codegraph-bench/tests/equivalence.rs`
  against the fixed golden artifacts under `reference/golden/`.
- **node-id formula**: `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}`; file nodes are the
  literal `file:{relpath}`; lines are 1-based; paths relative with `/`.
- **No AI / vector / LLM crates** — enforced by `scripts/guardrail.sh` (CI gate):
  no surrealdb / rig / qdrant / lancedb / candle / onnx / ort.
- **Deterministic** extraction + resolution; sync output must equal `index --force` byte-for-byte.

## Workspace layout (10 crates)

`codegraph-core` (types/config/logger) · `codegraph-store` (SQLite+FTS5) · `codegraph-extract`
(tree-sitter walker + embedded + custom extractors) · `codegraph-graph` (traversal + FTS search) ·
`codegraph-resolve` (import + name matcher + FrameworkResolver extension point) · `codegraph-mcp`
(stdio JSON-RPC) · `codegraph-cli` (single binary, owns logger; also hosts the `install`/`uninstall`
agent-config installer in `src/installer/`) · `codegraph-daemon` ·
`codegraph-watch` · `codegraph-bench` (benchmark harness + golden oracle).

The published crate is `codegraph-rs` (the `codegraph-cli` package); the installed binary is
`codegraph`. The library crates publish as `codegraph-{core,store,extract,graph,resolve,mcp,daemon,watch}`.
`codegraph-bench` is `publish = false`.

## Agent installer (`codegraph install` / `uninstall`)

`codegraph install` writes the codegraph MCP-server entry into each supported agent's config
(Claude Code, Cursor, Codex CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro);
`uninstall` reverses it. The written command launches the binary (`command: "codegraph"`,
`args: ["serve", "--mcp"]`). Non-interactive, flag-driven (`--target`, `--global`/`--local`/`--location`,
`--yes`, `--no-permissions`, `--print-config`); the config-writing logic (paths/keys/marker sections,
idempotent upsert, uninstall removal) is CLI-only and additive — it does NOT touch
extraction/golden equivalence.

## Verification gates (run before every commit)

```
make ci          # fmt-check + clippy + test + guardrail
# or individually:
cargo test --workspace          # incl. golden oracle + sync equivalence
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/guardrail.sh
```

## CI, hooks & release

- **Pre-push hook** (`.githooks/pre-push`): runs fmt + clippy + test + guardrail
  on `git push` (never on commit). Enable once per clone with `make hooks`
  (sets `core.hooksPath`). Local green ⇒ CI green.
- **CI** (`.github/workflows/ci.yml`): `Test` (fmt/clippy/test/guardrail) +
  `Security Audit` (cargo-audit) + `CI Success` gate, on push/PR to `main`.
- **Release** (`.github/workflows/release-please.yml`): release-please opens a
  release PR; merging it cuts a `v<version>` tag and triggers the pipeline —
  4-platform binaries (linux musl x86_64/aarch64 via cargo-zigbuild, macOS
  x86_64/aarch64), git-cliff release notes, GitHub Release
  assets, and topological crates.io publish of the 9 publishable crates
  (`core → … → codegraph-rs`; `codegraph-bench` is skipped). Version bumps are
  owned by release-please via `.release-please-manifest.json` — never bump by
  hand. Requires repo secret `CARGO_REGISTRY_TOKEN`.
- **Commits are English Conventional Commits.** `feat`→minor, `fix`→patch.
  The end-to-end release runbook lives in the `codegraph-release` skill.
- **Docs** are formatted with `oxfmt` (`make fmt`); `.oxfmtignore` excludes
  golden fixtures, embedded JSON, and auto-generated files.
