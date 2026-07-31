# AGENTS.md — codegraph-rs

A deterministic tree-sitter + SQLite/FTS5 **code knowledge graph**: it parses a codebase,
extracts symbols and their relationships, persists them to a per-project SQLite database
(with an FTS5 search index), and exposes the result through a CLI and an MCP (Model Context
Protocol) stdio server. No AI / vector / LLM anywhere in the binary — output is byte-stable.

## Hard invariants (never break)

- **Golden `.schema` byte-stability** — verified by `crates/codegraph-bench/tests/equivalence.rs`
  against the fixed golden artifacts under `reference/golden/`. Fixtures: the existing upstream
  corpus plus `reference/golden/godot/` (corpus `crates/codegraph-bench/fixtures/godot/`;
  guards F1 autoload-call edges + F2 signal-handler edges + UID-form autoloads
  — a sidecar-UID script autoload (`*.gd.uid`) and a header-UID scene autoload
  (`.tscn` `uid=`) — byte-for-byte) and
  `reference/golden/ruby/` (corpus `crates/codegraph-bench/fixtures/ruby/`; guards #1110
  Ruby `receiver.method` extraction — instance/class-method Calls, `Const.new` Instantiates,
  bare `include` Implements — byte-for-byte) and `reference/golden/cpp/`
  (corpus `crates/codegraph-bench/fixtures/cpp/`; guards #1043 C++ class/struct
  inheritance incl. templated-base stripping byte-for-byte, and retroactively
  the earlier C++ extraction work).
  Regen recipe: `docs/equivalence.md` "Godot fixture" / "Ruby fixture" / "C++ fixture" sections.
- **node-id formula**: `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}`; file nodes are the
  literal `file:{relpath}`; lines are 1-based; paths relative with `/`.
- **No AI / vector / LLM crates** — enforced by `scripts/guardrail.sh` (CI gate):
  no surrealdb / rig / qdrant / lancedb / candle / onnx / ort.
- **Deterministic** extraction + resolution; sync output must equal `index --force` byte-for-byte.

## Workspace layout (10 crates)

`codegraph-core` (types/config/logger) · `codegraph-store` (SQLite+FTS5) · `codegraph-extract`
(tree-sitter walker + embedded + custom extractors; incl. C++ `base_class_clause` → `Extends`
inheritance extraction with templated-base stripping, #1043) · `codegraph-graph` (traversal + FTS search) ·
`codegraph-resolve` (import + name matcher + FrameworkResolver; concrete `GodotResolver` impl — autoload-call + signal-handler resolution) · `codegraph-mcp`
(stdio JSON-RPC; `codegraph_explore` runs a change-surface rescue, #1064, that surfaces a
callable's buried parameter/return-type files into the explored subgraph) · `codegraph-cli` (single binary, owns logger; also hosts the `install`/`uninstall`
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
  `crates/codegraph-cli/src/main.rs`. `codegraph affected` additionally emits an
  `affectedFiles` key — the sorted+deduped union of every traversed dependent
  plus the test-file set (`affectedFiles ⊇ affectedTests`) — so it LISTS the
  complete affected set instead of only counting it via
  `totalDependentsTraversed`. Additive: `changedFiles`, `affectedTests`,
  `totalDependentsTraversed` are byte-for-byte unchanged.

Full Godot static-analysis scope, static-vs-runtime boundary, and honesty signals:
[`docs/godot.md`](docs/godot.md).

## MCP protocol surface (`codegraph-mcp`, rmcp 3.0.1)

`codegraph-mcp` builds on **rmcp 3.0.1** (`crates/codegraph-mcp/Cargo.toml`, dep and dev-dep;
features `server`, `client`, `transport-io`, `transport-streamable-http-server`). The 2.1 → 3.0.1
upgrade cost three lines in `rmcp_handler.rs`: `call_tool` returns the `#[non_exhaustive]`
`CallToolResponse` enum instead of `CallToolResult` (we only ever build `Complete`, via `.into()`),
`with_stateful_mode` became `with_legacy_session_mode`, and a stale `get_info()` comment was
corrected. The 3.x breaking changes that bit third-party code were all CLIENT-side
(`InitializeResult` → `ServerPeerInfo`, optional `server_info`, OAuth `resolve_metadata()`), none of
which touch a server `ServerHandler`. `Peer::list_roots` is still `#[deprecated]` (SEP-2577) with
still no replacement, so the `#[allow(deprecated)]` at `rmcp_handler.rs:334` stays.

**We serve three protocol revisions, and that is the SDK default rather than a choice.**
`ProtocolVersion` is `struct ProtocolVersion(Cow<'static, str>)` plus associated constants (NOT an
enum); rmcp's `negotiate_protocol_version` echoes back any client-requested version present in
`KNOWN_VERSIONS`. So `get_info()`'s `.with_protocol_version(V_2024_11_05)` is only the FALLBACK for
an unknown client version — it is not a ceiling, and we do not pin or force 2024-11-05:

| client requests | negotiated | `resultType` in results | HTTP session           |
| --------------- | ---------- | ----------------------- | ---------------------- |
| 2024-11-05      | 2024-11-05 | absent                  | no header (our config) |
| 2025-11-25      | 2025-11-25 | absent                  | no header (our config) |
| 2026-07-28      | 2026-07-28 | `"complete"`            | no header (per spec)   |

`resultType` (SEP-2322) is absent for pre-2026 peers because upstream
[#1038](https://github.com/modelcontextprotocol/rust-sdk/pull/1038) strips it for legacy peers, and
2026-07-28 is stateless per SEP-2567 — no `Mcp-Session-Id` — unconditionally, regardless of
`with_legacy_session_mode`, which now governs legacy versions only. We pass
`with_legacy_session_mode(false)` (`rmcp_handler.rs:737`), so the legacy versions issue no
`Mcp-Session-Id` either: THIS server never sends one at any revision. The two absences differ in
cause, though — legacy is our configuration and would come back if that flag flipped to `true`,
while 2026-07-28 is mandated and cannot be turned back on. SEP-2243 standard headers are
validated in both directions: matching `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` reach the
tool, while a missing protocol header or a mismatched `Mcp-Method` / `Mcp-Name` is rejected with
HTTP 400 + JSON-RPC code `-32020`. Coverage lives in `tests/rmcp_l3.rs` (version echo, `resultType`
presence/absence) and `tests/rmcp_http.rs` (no session header, SEP-2243 both branches).

**Not implemented, deliberately:** Tasks (SEP-2663), MRTR / elicitation-in-tool, subscriptions, and
discovery. These are capability-gated, so a 2026-07-28 server may legitimately omit them — we
declare tools only and always return `Complete`.

**MCP golden fixtures are STRUCTURAL, not byte-stable.** `tests/support/parity.rs:244-300` and
`tests/golden_mcp.rs` compare only NAMED fields — initialize: `protocolVersion`, `capabilities`,
`serverInfo.name`, `serverInfo.version`, `instructions`; tools/list: names + order + `inputSchema` +
annotations; tool result: `content[0].type`, `isError`, and the sorted set of non-empty text lines;
error: `code` + `message` — never whole-object equality. That is why an added top-level `resultType`
could not drift the 15 existing fixtures, and equally why it needed its own explicit test. (The
EXTRACTION goldens under `reference/golden/` are a different contract and genuinely ARE
byte-stable — see "Hard invariants" above.)

## HTTP MCP server: background mode + addr-keyed registry

`serve --mcp` (stdio) uses the PER-PROJECT daemon (`.codegraph-v2/daemon.pid` + socket; the whole
rendezvous is derived from `IndexPaths::current_root`). `serve --http`
(streamable-HTTP) is different: HTTP servers are keyed by BIND ADDR — a global server (no `--path`)
spans many projects — so they use a GLOBAL, addr-keyed registry, NOT the per-project root. The registry
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

Foreground stdio `serve --mcp` gets a THIRD, PID-keyed registry
(`codegraph-daemon/src/mcp_registry.rs`): one `<pid>.json`
(`McpServerInfo { pid, project: Option<String>, transport: "stdio", started_at, version }`, camelCase on
the wire) under the same GLOBAL state chain as the HTTP one but with an `mcp` leaf
(`$XDG_STATE_HOME/codegraph/mcp`, else `~/.local/state/codegraph/mcp`, `%LOCALAPPDATA%\codegraph\mcp` on
Windows; `CODEGRAPH_MCP_REGISTRY_DIR` overrides). PID keying is forced by the transport: a stdio process
has no addr and no per-project rendezvous, several may serve one project, and one may serve none.
Registration fires from all THREE foreground exits in `cmd_serve` (`Direct`, `SpawnOrProxy`, and the
too-broad-root home guard) and NEVER from `BeDaemon`, which already owns `.codegraph-v2/daemon.pid`.
Reads go through `RegistryRead::{Available, Unavailable}` so a MISSING directory ("nobody registered
yet", normal) is distinguishable from an unreadable one (an outage); `read_dir`'s `NotFound` only means
MISSING when `fs::symlink_metadata` also fails, so a dangling symlink at the registry path reads as an
outage instead of an empty registry. `list_entries` is the RAW on-disk view (stale entries included);
`live_entries` filters its RETURN VALUE by `is_process_alive` rather than trusting `prune_dead`'s
deletions to have landed, so a dead entry on an undeletable (read-only) registry is still never reported
as running — dead-PID and unparseable files are pruned on read as best-effort disk self-heal only.
`project` records the launch `--path` and ONLY that: it is `Some` only when the user actually passed
`--path` (a bare `serve --mcp` stores `None`, not its cwd), and it is purely INFORMATIONAL — never a
capability boundary, because `roots::resolve_project_arg` probes an absolute per-call `projectPath` on its
own merits and consults the launch default only when no path was passed, so any live server can be asked
to open any indexed project's database. `codegraph mcp list [--json]` renders it as `LAUNCH PROJECT`, and
BOTH holder diagnostics — `index`'s pre-warning and the `RemoveDatabase` FAILURE path — therefore report
ALL live entries with no narrowing: filtering by `project` would drop the holder in the very case they
exist for (a server launched elsewhere that a client has since pointed at this project), and the failure
path additionally has only a DB path, which with `CODEGRAPH_DIR` set is a `<name>-v2-<projectIdentity>`
sibling of the legacy root that can sit outside the project tree entirely. This
registry is PURE OBSERVABILITY — there is no `mcp stop` and no `terminate_pid` import, because a stale
entry's PID may have been reused and this workspace has no portable instance-identity primitive
(`try_acquire_daemon_lock` is a `create_new` placeholder plus a recorded PID, not an OS advisory lock);
`list` asks a human to confirm the PID with `ps -p <pid> -o command=` / `tasklist /FI "PID eq <pid>"`
before offering `kill <pid>` / `taskkill /PID <pid> /F`. CLI/daemon only; extraction and golden
equivalence untouched.

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
  6-platform binaries (linux musl x86_64/aarch64 via cargo-zigbuild, macOS
  x86_64/aarch64, and Windows MSVC `x86_64-pc-windows-msvc` on `windows-latest`
  plus `aarch64-pc-windows-msvc` on `windows-11-arm`), git-cliff release notes,
  and a GitHub Release with the binaries attached. The project is distributed via GitHub Releases +
  `cargo install --git`; it is NOT published to crates.io. Version bumps are
  owned by release-please via `.release-please-manifest.json` — never bump by
  hand.
- **Commits are English Conventional Commits.** `feat`→minor, `fix`→patch.
  The end-to-end release runbook lives in the `codegraph-release` skill.
- **Docs** are formatted with `oxfmt` (`make fmt`); `.oxfmtignore` excludes
  golden fixtures, embedded JSON, and auto-generated files.
