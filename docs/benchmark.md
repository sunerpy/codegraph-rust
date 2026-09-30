# Benchmark methodology

CodeGraph ships a reproducible comparison harness in `codegraph-bench`. It is
separate from correctness: a faster result never relaxes schema, golden, or graph
parity.

## What the pipeline measures

For each pinned corpus, the harness compares the release Rust CLI with a built
copy of the pinned upstream TypeScript CLI on byte-identical source trees. It
records:

- cold and warm full-index wall time;
- one-file incremental synchronization wall time;
- cold and warm query latency (median, MAD, p50, and p99);
- peak RSS samples when the host exposes them;
- resulting database size; and
- a Rust-only in-process parse metric, clearly separated because upstream has no
  equivalent entry point.

Before timing a corpus, the pipeline indexes both arms and requires normalized
SQLite `.schema` equality. A schema mismatch aborts the run rather than comparing
non-equivalent work.

## Pinned corpora

The source registry in `crates/codegraph-bench/src/corpus.rs` is authoritative.
Each row pins a repository URL, full commit SHA, benchmark subdirectory, expected
non-blank LOC, and source-file count. `--fetch-corpora` creates immutable
checkouts under ignored `bench/corpora/` and refuses an existing checkout at the
wrong commit.

Inspect the registry and local state:

```bash
cargo run --locked -p codegraph-bench --bin bench -- --list-corpora
```

Fetch the exact commits (networked, explicit operation):

```bash
cargo run --locked -p codegraph-bench --bin bench -- --fetch-corpora
```

## Prerequisites

1. Build the shipped Rust binary with the same locked release profile used by
   release workflows:

   ```bash
   cargo build --locked --release -p codegraph-rs
   ```

2. Materialize the pinned upstream checkout at `reference/colby` and build
   `reference/colby/dist/bin/codegraph.js` according to that checkout's
   `RUN.md`. Record its exact commit in the result artifact.
3. Fetch the corpus pins and verify `--list-corpora` reports the expected commit,
   file count, and LOC for every selected corpus.
4. Use an otherwise idle machine. Record CPU, memory, OS, kernel, Node, Rust,
   Rust implementation commit, and upstream implementation commit.

The full pipeline is Linux-oriented because peak-RSS collection and cold-cache
handling use Linux facilities when available. If the kernel does not allow page
cache drops, the harness records best-effort userspace eviction; do not label
that series equivalent to a privileged cold-cache run without the emitted caveat.

## Run

Use at least two runs because run one is discarded. A normal evidence run should
use enough samples for stable p99 interpretation; twelve is the CLI default.

```bash
cargo run --locked --release -p codegraph-bench --bin bench -- \
  --run \
  --runs 12 \
  --corpora all \
  --out target/benchmark-results.json \
  --report-md docs/benchmark-results.md
```

To isolate one corpus during harness development:

```bash
cargo run --locked --release -p codegraph-bench --bin bench -- \
  --run --runs 12 --corpora fd-small \
  --out target/benchmark-fd.json
```

To re-render Markdown without rerunning measurements:

```bash
cargo run --locked -p codegraph-bench --bin bench -- \
  --render-md target/benchmark-results.json docs/benchmark-results.md
```

## Evidence requirements

A publishable result must include:

- raw JSON and generated Markdown from the same run;
- clean Git status or an explicit diff description;
- both implementation commit SHAs;
- exact corpus pins and observed counts;
- environment fields emitted by the harness;
- run count, discarded-first policy, and cache mode;
- schema equality for every corpus;
- every raw sample, median, MAD, p50, and p99;
- failures, timeouts, unsupported RSS, or cache-eviction caveats.

Do not copy a latency number into README, AGENTS, or release notes without linking
to such an artifact. Results are snapshots, not timeless product guarantees.
