# AGENTS.md — codegraph-rs contributor contract

CodeGraph is a deterministic tree-sitter + SQLite/FTS5 code knowledge graph. The
single `codegraph` binary indexes source, resolves relationships, answers graph
queries through the CLI and MCP, and can keep an index current through a local
daemon. It contains no AI, vector database, embedding model, or LLM runtime.

This file is the canonical instruction source for coding agents. `CLAUDE.md`
imports it; do not create a second agent guide. Detailed, changeable behavior
belongs in [`docs/`](docs/README.md), not here.

## Navigate with CodeGraph first

Before researching source in an indexed checkout, verify the index:

```bash
codegraph status . --json
```

Lifecycle commands take an optional positional project path. Research commands
take one query/target plus `-p/--path`:

```bash
codegraph status . --json
codegraph sync .
codegraph explore "index and resolve flow" -p .
codegraph search "ReferenceResolver" -p .
codegraph node "ReferenceResolver" -p .
```

Use `explore` for an area or call flow, `search` for a known name, `node` for one
symbol/file plus its trail, and `impact` before a refactor. Do not append `.` as
a second positional argument to a research command. If the index is unavailable,
follow `status` recovery guidance; do not initialize or rebuild unless requested
or required by that guidance.

## Hard invariants

1. **Deterministic graph output.** Identical source and configuration must produce
   identical canonical nodes, edges, references, files, and query ordering.
   Incremental `sync` must converge with a clean full index.
2. **Golden compatibility.** Extraction goldens under `reference/golden/` are
   byte-stable canonical artifacts. Update them only for an intentional graph
   behavior change, with the matching corpus and regeneration evidence documented
   in [`docs/equivalence.md`](docs/equivalence.md).
3. **Stable node IDs.** Symbol IDs are
   `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[..32]}`; file nodes are
   `file:{relative/path}`. Paths use `/`; lines are 1-based. Never change this
   formula incidentally.
4. **No AI/vector runtime.** Do not add AI, LLM, embedding, vector-database, or
   inference dependencies. `scripts/guardrail.sh` enforces the dependency boundary.
5. **Project containment.** Managed state stays under the selected project index
   root. Filesystem fallbacks must prove lexical containment before probing or
   reading a path. Do not broaden project authority through ancestor discovery,
   symlink guessing, environment-global state, or another project's configuration.
6. **Fail closed.** Ambiguous resolution stays unresolved. Unsafe stale source is
   served whole or omitted, never sliced using stale line ranges. Lock, checksum,
   migration, and release validation failures must stop rather than silently skip.
7. **Protocol and stdout purity.** MCP/JSON-RPC output owns stdout. Logs and
   diagnostics go to stderr. Additive fields are preferred; existing JSON, text,
   installer, and protocol contracts require explicit compatibility tests.
8. **No manual releases or version edits.** Release Please owns versions and tags.
   Distribution is GitHub Releases plus `cargo install --git`; no crate is
   published to crates.io.

## Workspace ownership

The workspace members are declared in root `Cargo.toml`; that manifest is the
authority when the list changes.

| Crate                            | Owns                                                               |
| -------------------------------- | ------------------------------------------------------------------ |
| `codegraph-core`                 | shared types, config, IDs, file classification, logging            |
| `codegraph-extract`              | language detection, tree-sitter/custom extraction, scan policy     |
| `codegraph-store`                | SQLite schema/migrations, FTS5, persistence and queries            |
| `codegraph-resolve`              | import/name resolution and framework resolvers                     |
| `codegraph-graph`                | traversal, impact, search scoring and query parsing                |
| `codegraph-mcp`                  | MCP schemas, rmcp transports, project resolution, tool rendering   |
| `codegraph-watch`                | incremental synchronization and filesystem watching                |
| `codegraph-daemon`               | shared process lifecycle, IPC, registries and detach behavior      |
| `codegraph-cli` (`codegraph-rs`) | CLI, installer, orchestration and shipped binary                   |
| `codegraph-bench`                | equivalence oracle and reproducible benchmark harness; not shipped |

Keep dependency direction acyclic and lower layers independent of presentation.
Extraction must not depend on store/graph. Query rendering must not leak into core
semantics. See [`docs/architecture.md`](docs/architecture.md) for the current graph.

## Change-to-proof matrix

Run the narrowest relevant tests while iterating, then the complete gate before
handoff. Do not use a narrow test to claim a workspace-wide property.

| Change                     | Minimum focused proof                                                  | Required documentation                                                       |
| -------------------------- | ---------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| extraction/language rules  | extractor tests; affected golden corpus; incremental/full equivalence  | `languages.md`, `grammar-manifest.md`, and golden recipe when behavior moves |
| resolution/framework rules | resolver unit/integration tests; ambiguity negatives; affected golden  | `equivalence.md` or framework reference when public behavior moves           |
| schema/migrations/store    | schema parity; migration replay; state/lease tests; golden equivalence | `data-model.md`                                                              |
| graph/search               | graph/query tests; deterministic ordering and limit cases              | CLI/MCP docs for public output changes                                       |
| MCP/protocol               | engine tests; structural MCP goldens; rmcp stdio/HTTP/version tests    | `mcp.md`                                                                     |
| CLI/installer              | command tests; installer round trips; script contract fixtures         | `cli.md`, README only for landing-page behavior                              |
| daemon/watch/concurrency   | lifecycle, lock, recovery, watcher and platform-focused tests          | architecture/CLI/MCP lifecycle sections                                      |
| release/install/checksum   | shell/PowerShell fixtures, asset-name checks, archive smoke            | README install section and release workflow contract                         |
| docs/community files       | `python3 scripts/docs-check.py`; formatter; link/anchor checks         | update the canonical page, not a duplicate summary                           |

If a change alters nodes, edges, reference resolution, file classification, or
stored graph meaning, decide explicitly whether the extraction version must move.
Group related graph-semantic changes so users do not rebuild repeatedly. Schema
version and extraction version are independent.

## Documentation rules

- Public technical docs are canonical in English. The Chinese README is a
  maintained landing-page mirror, not a promise to translate every deep reference.
- Docs describe **AS-BUILT** behavior. Update the relevant page in the same change
  as code. Avoid fixed counts and point-in-time versions unless a source-contract
  test derives and checks them.
- Keep the English and Chinese README structure, commands, security claims, and
  links in sync. Move volatile CLI/IDE/protocol details into canonical docs.
- Historical entries in `docs/upstream-sync/UPSTREAM.md` and dated audit files are
  evidence: append a new entry; never rewrite an old observation to look current.
- Do not publish unmeasured performance claims. Benchmark method and results must
  identify the commit, corpus, environment, command, run count, and dispersion.
- `CLAUDE.md` must remain a regular file containing exactly `@AGENTS.md` plus a
  trailing newline.

Directory-specific rules live in [`docs/AGENTS.md`](docs/AGENTS.md).

## Validation

The authoritative local/CI entry point is:

```bash
make check
```

`make ci` is a compatibility alias for the same complete gate. `make pre-ci` adds a package/unpack/execute smoke over the built release bytes.
It validates workspace-version consistency before any Cargo subprocess, required
tool versions, Rust and repository-text formatting, workflow/shell linting,
Clippy with warnings denied, locked tests, a locked release build, guardrails,
and script fixtures. The version-controlled pre-push hook calls this same path.

Useful focused commands:

```bash
cargo test -p <crate> --locked <test-filter>
cargo test -p codegraph-bench --test equivalence --locked
cargo test -p codegraph-mcp --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/docs-check.py
actionlint .github/workflows/*.yml
bash scripts/guardrail.sh
```

Coverage is tracked against an aspirational 95% target but remains informational
until it is sustainably above that target. Do not weaken tests to raise coverage,
and do not quote a cached percentage as current without re-measuring it.

## Upstream synchronization

The TypeScript reference is `colbymchenry/codegraph`; the Rust product remains
independent. Read [`docs/upstream-sync/UPSTREAM.md`](docs/upstream-sync/UPSTREAM.md)
first. Port behavior and intent, not TypeScript mechanisms. For each upstream
family record one of `PORT`, `ALREADY-HAVE`, `N/A`, or `DEFER`, with source proof,
Rust target, graph/schema/API impact, and acceptance tests. Update the ledger only
after current source and runtime evidence agree.

## Git, PR, and release discipline

- Preserve dirty work and owner-uncertain worktrees. Use an isolated worktree for
  implementation; never reset, clean, overwrite, apply/drop stashes, or delete
  another worker's state.
- Commits and PR titles use English Conventional Commits. `feat` is a minor bump,
  `fix` is a patch bump, and `feat!`/`BREAKING CHANGE` is major. Never add AI or
  co-author trailers.
- Stage specific files. Do not commit, push, merge, or mutate GitHub settings
  unless explicitly requested.
- The required check is `CI Success`. Coverage is separately informational.
- Release runs are same-run, draft-until-verified: the exact tag SHA, six platform
  archives, archive smoke, checksums, attestations, asset inventory, and source CI
  gate must pass before publication. Never describe a draft or partial run as a
  release.
- A release claim binds implementation PR head → merge → tag SHA → workflow run →
  downloaded public bytes. “Latest” is not evidence.

Canonical details: [`CONTRIBUTING.md`](CONTRIBUTING.md),
[`docs/equivalence.md`](docs/equivalence.md),
[`docs/mcp.md`](docs/mcp.md), and [`docs/upstream-sync/`](docs/upstream-sync/).
