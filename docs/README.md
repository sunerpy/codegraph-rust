# CodeGraph documentation

The root [README](../README.md) is the landing page. This index points to the
canonical technical references; update the owning page rather than duplicating
volatile behavior elsewhere.

## Use CodeGraph

- [CLI reference](cli.md) — command/path contracts, installation targets,
  daemon/watch lifecycle, configuration, and diagnostics
- [MCP reference](mcp.md) — tool schemas, visible/known tools, project resolution,
  stdio and streamable HTTP, protocol behavior, and client configuration
- [Supported languages](languages.md) — user-facing language coverage, extensions,
  extraction tiers, and static-analysis boundaries
- [Godot static analysis](godot.md) — scenes, resources, scripts, autoloads,
  honesty signals, and known runtime boundaries
- [Troubleshooting](troubleshooting.md) — opt-in JSONL diagnostics and evidence to
  include in a report

## Understand the implementation

- [Architecture](architecture.md) — workspace dependency graph and the extraction,
  storage, resolution, query, MCP, daemon, and watcher flow
- [Data model](data-model.md) — SQLite/FTS5 tables, indexes, migrations, and
  connection policy
- [Equivalence oracle](equivalence.md) — stable IDs, canonicalization tiers,
  golden corpora, and regeneration procedures
- [Grammar ABI manifest](grammar-manifest.md) — grammar/custom extractor ownership
  and contributor checks
- [Embedded extraction](embedded-extraction.md) — Vue, Svelte, Astro, Razor,
  Liquid, MyBatis XML, and DFM/FMX delegation
- [Benchmark methodology](benchmark.md) and
  [current result status](benchmark-results.md) — how performance evidence is
  collected and what is currently safe to claim

## Project governance

- [Contributing](../CONTRIBUTING.md)
- [Security policy](../SECURITY.md)
- [Code of Conduct](../CODE_OF_CONDUCT.md)
- [Agent contributor contract](../AGENTS.md)
- [Upstream synchronization ledger](upstream-sync/UPSTREAM.md)
- [Dated upstream audits](upstream-sync/)

## Language policy

English is canonical for deep technical references. The
[Chinese README](readme/README.zh-CN.md) mirrors the landing page, commands,
security posture, and navigation; it does not duplicate every implementation
reference.
