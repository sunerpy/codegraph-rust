# AGENTS.md — codegraph-rs

A deterministic tree-sitter + SQLite/FTS5 **code knowledge graph**: it parses a codebase,
extracts symbols and their relationships, persists them to a per-project SQLite database
(with an FTS5 search index), and exposes the result through a CLI and an MCP (Model Context
Protocol) stdio server. No AI / vector / LLM anywhere in the binary — output is byte-stable.

## Hard invariants (never break)

- **Golden `.schema` byte-stability** — verified by `crates/codegraph-bench/tests/equivalence.rs`
  against the fixed golden artifacts under `reference/golden/`. Fixtures: the existing upstream
  corpus plus `reference/golden/godot/` (corpus `crates/codegraph-bench/fixtures/godot/`;
  guards F1 autoload-call edges + F2 signal-handler edges byte-for-byte) and
  `reference/golden/ruby/` (corpus `crates/codegraph-bench/fixtures/ruby/`; guards #1110
  Ruby `receiver.method` extraction — instance/class-method Calls, `Const.new` Instantiates,
  bare `include` Implements — byte-for-byte).
  Regen recipe: `docs/equivalence.md` "Godot fixture" / "Ruby fixture" sections.
- **node-id formula**: `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}`; file nodes are the
  literal `file:{relpath}`; lines are 1-based; paths relative with `/`.
- **No AI / vector / LLM crates** — enforced by `scripts/guardrail.sh` (CI gate):
  no surrealdb / rig / qdrant / lancedb / candle / onnx / ort.
- **Deterministic** extraction + resolution; sync output must equal `index --force` byte-for-byte.

## Workspace layout (10 crates)

`codegraph-core` (types/config/logger) · `codegraph-store` (SQLite+FTS5) · `codegraph-extract`
(tree-sitter walker + embedded + custom extractors) · `codegraph-graph` (traversal + FTS search) ·
`codegraph-resolve` (import + name matcher + FrameworkResolver; concrete `GodotResolver` impl — autoload-call + signal-handler resolution) · `codegraph-mcp`
(stdio JSON-RPC) · `codegraph-cli` (single binary, owns logger; also hosts the `install`/`uninstall`
agent-config installer in `src/installer/`) · `codegraph-daemon` ·
`codegraph-watch` · `codegraph-bench` (benchmark harness + golden oracle).

The published crate is `codegraph-rs` (the `codegraph-cli` package); the installed binary is
`codegraph`. The library crates publish as `codegraph-{core,store,extract,graph,resolve,mcp,daemon,watch}`.
`codegraph-bench` is `publish = false`.

## Godot framework resolver (`codegraph-resolve`)

The `GodotResolver` is the first concrete `FrameworkResolver` impl. It fires on
GDScript files and synthesizes edges that tree-sitter alone cannot produce. Three
behaviors are active:

- **F1 — autoload-call→func edges**: a call `Autoload.method()` in a `.gd` file
  emits a `Calls` edge to the UNIQUE same-named `func` in the autoload's bound
  target script (binding read from `project.godot` `[autoload]` section,
  `Name="*res://path.gd"` form only). Determinism rule: edge built ONLY when
  exactly one matching `func` exists in that script; 0 or ≥2 matches → no edge.
  Files: `crates/codegraph-resolve/src/frameworks/godot.rs`,
  `crates/codegraph-resolve/src/frameworks/godot_script.rs`.

- **F2 — signal handler extraction**: `connect_handler` now extracts handlers
  from `.connect(_h.bind(x))` (head segment before `.bind(`) and
  `Callable(self,"h")`/`Callable(this,"h")` forms, in addition to bare
  `.connect(_h)`. Other receivers, variable handlers, or non-literal method
  names stay dynamic sentinels (unresolved). File:
  `crates/codegraph-resolve/src/frameworks/godot_script.rs`.

- **F3 — impact/affected↔audit unification**: `codegraph impact` (file-node
  targets) and `codegraph affected` now also consume path-keyed `unresolved_refs`
  restricted to Godot `ReferenceSubkind`s (`script_attach`, `ext_resource`,
  `scene_instance`, `group_member`, `signal_method`, `autoload`), so their
  output agrees with `codegraph audit --impact`. Query-side only; zero extraction
  change. New function: `dependent_file_paths_unresolved` in
  `crates/codegraph-store/src/queries.rs`; CLI wired in
  `crates/codegraph-cli/src/main.rs`.

Full Godot static-analysis scope, static-vs-runtime boundary, and honesty signals:
[`docs/godot.md`](docs/godot.md).

## HTTP MCP server: background mode + addr-keyed registry

`serve --mcp` (stdio) uses the PER-PROJECT daemon (`.codegraph/daemon.pid` + socket). `serve --http`
(streamable-HTTP) is different: HTTP servers are keyed by BIND ADDR — a global server (no `--path`)
spans many projects — so they use a GLOBAL, addr-keyed registry, NOT `.codegraph/`. The registry
lives in `codegraph-daemon/src/http_registry.rs`: one `<addr-sanitized>.json` file per running server
(`HttpServerInfo { pid, addr, mode, project, started_at, version, log_file }`) under
`$XDG_STATE_HOME/codegraph/http` (else `~/.local/state/codegraph/http`; `%LOCALAPPDATA%\codegraph\http`
on Windows; `CODEGRAPH_HTTP_REGISTRY_DIR` overrides). Entries are pruned when their pid is dead
(self-heal, gated on `is_process_alive`).

`serve --http` stays FOREGROUND by default; `serve --http --detach` runs in the BACKGROUND via
`spawn::spawn_detached_http` (generalized from `spawn_detached_daemon` over the shared `detach()`
primitive; the child carries `CODEGRAPH_HTTP_DETACH_INTERNAL=1` so it runs the foreground serve path
and does NOT re-detach). On startup `serve --http` prunes dead entries, ERRORS on a live same-addr
conflict (listing the running instance), and notes any other live servers when the addr is free. The
`codegraph http {list, status, stop}` subcommand group inspects and terminates registered servers
(`stop` uses `process::terminate_pid` — SIGTERM on unix / `TerminateProcess` on Windows). None of this
touches extraction/golden equivalence.

## Agent installer (`codegraph install` / `uninstall`)

`codegraph install` writes the codegraph MCP-server entry into each supported agent's config
(Claude Code, Cursor, Codex CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro, Trae, Qoder, Zed);
`uninstall` reverses it. The written command launches the binary (`command: "codegraph"`,
`args: ["serve", "--mcp"]`). Cursor and Trae use `--path ${workspaceFolder}` in their global config so
one entry auto-follows each project window; Kiro and Qoder write a bare global entry (no `--path`) that
serves tools read-only off any existing index, with the agent passing the project path per call — run
`codegraph init --target=<ide>` inside each project to write a project-local config with an absolute
`--path` for live watch. Kiro's `mcp.json` also carries a `//`-commented HTTP alternative alongside the
active stdio entry (JSONC, idempotent, injected best-effort without corrupting existing files); it uses
`http://localhost:8111/mcp` because Kiro allows `http` only for localhost (remote servers must be `https`).
Zed's `settings.json` likewise carries `//`-commented remote-development alternatives after the active
`context_servers.codegraph` stdio entry (both `install` global and `init` project-local): an SSH-stdio
bridge and an HTTP server (`http://localhost:8111/mcp`, marked RECOMMENDED for remote); the shared
JSONC-safe injector is `inject_commented_alternative(path, parent_key, entry_key, sentinel, block)` in
`shared.rs`, used by both Kiro (`mcpServers`) and Zed (`context_servers`).
Non-interactive, flag-driven (`--target`, `--global`/`--local`/`--location`,
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
make coverage    # workspace coverage summary (informational; `make coverage-html` for the full report)
```

## Test coverage (tracked, informational)

- Unit-test coverage is a tracked metric via `cargo llvm-cov` + Codecov.
  Run `make coverage` for a summary; `make coverage-html` for the browsable
  report; `make coverage-lcov` writes the `lcov.info` CI uploads.
- **Target is 95%+** (aspirational). The CI gate is **informational /
  non-blocking** — the `coverage` job is kept out of the `CI Success` gate and
  the Codecov status is `informational: true` (`codecov.yml`), so a below-target
  % never turns CI red. This honors the iron rule "local green ⇒ CI green".
- **Baseline ~72% line coverage** — a known gap to close. Biggest gaps:
  `codegraph-resolve/src/import_resolver.rs`, `codegraph-resolve/src/name_matcher.rs`,
  and the 0%-covered `codegraph-watch/src/{git,worktree}.rs`.
- **Enabling Codecov:** enable the repo at codecov.io. This repo is public, so
  tokenless upload works (no `CODECOV_TOKEN` needed); a private repo would need
  `CODECOV_TOKEN` in GitHub repo Secrets.

## CI, hooks & release

- **Pre-push hook** (`.githooks/pre-push`): runs fmt + clippy + test + guardrail
  on `git push` (never on commit). Enable once per clone with `make hooks`
  (sets `core.hooksPath`). Local green ⇒ CI green.
- **CI** (`.github/workflows/ci.yml`): `Test` (fmt/clippy/test/guardrail) +
  `Security Audit` (cargo-audit) + `CI Success` gate, on push/PR to `main`.
- **Release** (`.github/workflows/release-please.yml`): release-please opens a
  release PR; merging it cuts a `v<version>` tag and triggers the pipeline —
  4-platform binaries (linux musl x86_64/aarch64 via cargo-zigbuild, macOS
  x86_64/aarch64), git-cliff release notes, and a GitHub Release with the
  binaries attached. The project is distributed via GitHub Releases +
  `cargo install --git`; it is NOT published to crates.io. Version bumps are
  owned by release-please via `.release-please-manifest.json` — never bump by
  hand.
- **Commits are English Conventional Commits.** `feat`→minor, `fix`→patch.
  The end-to-end release runbook lives in the `codegraph-release` skill.
- **Docs** are formatted with `oxfmt` (`make fmt`); `.oxfmtignore` excludes
  golden fixtures, embedded JSON, and auto-generated files.
