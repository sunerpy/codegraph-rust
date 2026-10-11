# Benchmark methodology

CodeGraph ships a reproducible comparison harness in `codegraph-bench`. It is
separate from correctness: a faster result never relaxes schema, golden, or graph
parity.

## What the pipeline measures

For each pinned corpus, `--run` compares the release Rust CLI with a built copy
of the pinned upstream TypeScript CLI on byte-identical source trees. It
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

The source registry in
[`crates/codegraph-bench/src/corpus.rs`](../crates/codegraph-bench/src/corpus.rs)
is authoritative. Each row pins a repository URL, full commit SHA, the release tag
the commit was taken from (when there is one), benchmark subdirectory, expected
non-blank LOC, and source-file count. `--fetch-corpora` creates immutable
checkouts under ignored `bench/corpora/` and refuses an existing checkout at the
wrong commit.

| Corpus             | Repository                | Tag                  | Commit                                     | Benchmark directory | Used for                               |
| ------------------ | ------------------------- | -------------------- | ------------------------------------------ | ------------------- | -------------------------------------- |
| `fd-small`         | `sharkdp/fd`              | —                    | `25461e5ce13dc12ff2a75993285a87e99b33db2d` | `src`               | timing (Rust)                          |
| `tokio-medium`     | `tokio-rs/tokio`          | —                    | `ecb5125a6787b9d8eb818b1b00973bcd55ae77c0` | `tokio/src`         | timing (Rust)                          |
| `typescript-large` | `microsoft/TypeScript`    | —                    | `7964e22f2b85f16e520f0e902c7fd7b6f0c15416` | `src`               | timing (TypeScript)                    |
| `express-medium`   | `expressjs/express`       | —                    | `dae209ae6559c29cfca2a1f4414c51d89ea643d5` | `.`                 | timing (JavaScript)                    |
| `leveldb-small`    | `google/leveldb`          | `1.23`               | `99b3c03b3284f5886f9ef9a4ef703d57373e61be` | `.`                 | precision review (C++)                 |
| `gson-medium`      | `google/gson`             | `gson-parent-2.14.0` | `3ff35d6269894901ab8006258395aafc4b9765cd` | `gson`              | precision review (Java)                |
| `newtonsoft-large` | `JamesNK/Newtonsoft.Json` | `13.0.4`             | `4e13299d4b0ec96bd4df9954ef646bd2d1b5bf2a` | `Src`               | precision review (C#)                  |
| `alamofire-small`  | `Alamofire/Alamofire`     | `5.12.2`             | `bda9ed57d72988a3a2ada33d824583541f86eac6` | `Source`            | precision review (Swift)               |
| `redis-medium`     | `redis/redis`             | `8.10.2`             | `498ecd0d6d007db11ddb3aea9428552598a78622` | `src`               | timing (C with function-pointer calls) |
| `kong-medium`      | `Kong/kong`               | `3.9.3`              | `a643428bc4d5397152164a63bcc0f8bc65fce69d` | `kong`              | timing (Lua)                           |

A unit test in `codegraph-bench` checks that every registry row appears in this
table with its commit, tag, and benchmark directory.

Inspect the registry and local state:

```bash
cargo run --locked -p codegraph-bench --bin bench -- --list-corpora
```

Fetch the exact commits (networked, explicit operation):

```bash
cargo run --locked -p codegraph-bench --bin bench -- --fetch-corpora
```

Both take `--corpora-root <dir>` to use another directory of checkouts, such as
one shared between worktrees.

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

## A/B comparison

`--ab` times a cold `init` of two CodeGraph binaries on the same pinned corpora,
and checks that each binary builds the same graph on every run. It compares two
builds of this project, such as release builds of a base commit and of a change;
the upstream CLI is not involved.

```bash
cargo build --locked --release -p codegraph-rs -p codegraph-bench
target/release/bench --ab \
  --baseline /path/to/baseline/codegraph \
  --candidate target/release/codegraph \
  --corpora fd-small,tokio-medium,typescript-large,express-medium \
  --corpora-root bench/corpora \
  --threads 32,4 \
  --runs 6 \
  --out target/ab.json \
  --report-md target/ab.md
```

`--corpora` takes registry names, comma-separated or repeated, or `all`.
`--corpora-root` is a directory of registry checkouts and defaults to
`bench/corpora`. Before anything runs, a checkout that is missing, at another
commit than its pin, or modified (non-empty `git status --porcelain`) is
refused. `--runs` counts every run of a cell, the warm-up included, and must be
at least 2. Without `--out` the JSON report goes to stdout; progress goes to
stderr.

### What each run does

For each corpus, thread count, and run, the harness:

1. copies the corpus's benchmark directory into a fresh temporary project,
   leaving out `.git`, `.codegraph`, and `.codegraph-wsl` and skipping symlinks;
2. runs `<binary> init <project>` with `RAYON_NUM_THREADS=<threads>`,
   `CODEGRAPH_NO_DAEMON=1`, and `CODEGRAPH_NO_WATCH=1`, and with `CODEGRAPH_DIR`
   removed from the environment;
3. records the wall time from spawn to exit, polled every millisecond, and the
   peak RSS reported by `wait4` (`ru_maxrss`; not collected on Windows);
4. checks `.codegraph/codegraph.db` with `PRAGMA quick_check`, canonicalizes it
   through the [equivalence oracle](equivalence.md), and fingerprints it; and
5. deletes the project.

The CLI has no thread-count flag. Indexing parses and resolves on rayon's global
thread pool, which takes its size from `RAYON_NUM_THREADS`, so each thread count
is a separate cell with that variable set.

The binaries alternate: even runs time the baseline first and odd runs the
candidate first. The first run of every (corpus, binary, thread count) cell is a
warm-up. It is checked like every other run but left out of the statistics.
"Cold" means that no index exists when `init` starts; the page cache is warm,
because the project was just copied.

The fingerprint hashes each canonical surface with SHA-256: for `nodes`,
`edges`, `unresolved_refs`, and `files`, the compact JSON array of the
surface's canonical rows; for `schema`, the normalized schema text. The graph
hash is the SHA-256 of the five lines `<surface> <sha256>`, in that order.

### Failure policy

The harness stops with a non-zero exit status, and writes no report, when:

- a run exits non-zero, is killed by a signal, or runs longer than 30 minutes;
- a run exits 0 but leaves no database, or leaves one that fails
  `PRAGMA quick_check` or canonicalization; or
- two runs of the same binary on one corpus produce different graph hashes, at
  the same or at different thread counts.

A difference between the baseline and candidate graphs is not a failure. The
report records it per corpus, so a change that alters the graph can still be
timed against its row growth; `--graph-diff` shows the rows.

### Report

The JSON report has `kind` `codegraph-ab` and `schema_version` 1. Its fields:

- `argv`: the `bench` command line.
- `environment`: CPU model, logical CPUs, memory, OS, kernel, `rustc --version`,
  the harness commit and whether its worktree was dirty, and how RSS was
  collected.
- `settings`: the invocation, environment overrides, thread variable, thread
  counts, runs, discarded warm-ups, run order, run timeout, and budget limits.
- `baseline`, `candidate`: the path, SHA-256, size, and `--version` line of each
  binary.
- `corpora[]`: the name, pinned commit and tag, source directory, copied files,
  and skipped symlinks of each corpus.
- `corpora[].graph`: `identical`, `differing_surfaces`, and each binary's
  fingerprint: `hash`, per-surface hashes, and canonical row counts.
- `corpora[].cells[]`: one entry per thread count, holding every run of each
  binary (`wall_ms`, `peak_rss_kb`, `warmup`, `canonical_hash`), the median and
  MAD of the measured runs, canonical rows per second at the median, the
  candidate/baseline `ratio` of wall time, peak RSS, rows, and rows per second,
  and the `budget` verdict.

`--report-md` renders the same data as a Markdown table with one row per corpus
and thread count, below a table of the binaries, their hashes, and the
environment.

Publishing an A/B result follows the
[evidence requirements](#evidence-requirements): the report carries the binary
hashes, the environment, every run, and the graph fingerprints.

### Regression budget

Measured against a release build of the base commit, a change stays within the
project's performance budget when:

- the cold `init` median is at most 1.10× the baseline's. A slower `init` is
  still within budget when the candidate graph has more canonical rows and its
  canonical rows per second stay at or above 0.90× the baseline's;
- a single-file `sync` takes at most 1.15× the baseline's time;
- the `explore` p50 is at most 1.10× the baseline's; and
- the peak RSS median is at most 1.15× the baseline's.

`--ab` measures the `init` and peak RSS terms and reports them per cell as
`budget.init_wall_ok` and `budget.peak_rss_ok`. It never fails a run for being
over budget; `sync` and `explore` are measured separately. A change over budget
is fixed, or kept only with its A/B report attached for the owner to accept.

### Graph diff

`--graph-diff <LEFT> <RIGHT>` compares two canonical graphs. Each side is a
CodeGraph SQLite database or a golden directory (`nodes.json`, `edges.json`,
`refs.json`, `files.json`, `schema.sql`), so a fresh index can be compared with
a committed golden or with another index:

```bash
target/release/bench --graph-diff reference/golden/typescript \
  /tmp/cg-fixture-typescript/.codegraph/codegraph.db
```

A database is copied, together with its `-wal` when one exists, into a temporary
directory and read only there, so the input never gains SQLite sidecars. The
copy must pass `PRAGMA quick_check`.

Rows compare as multisets of canonical JSON. A row only on the left is removed,
a row only on the right is added, and a node or file whose attributes changed
appears once on each side. The report gives the removed and added counts of each
surface and whether the schema changed, then a table of the changed groups, then
every changed row under its group, all in a fixed order. A group is (surface,
language, kind, resolvedBy):

- a node groups by its language and kind;
- an edge by its source node's language, its kind, and `metadata.resolvedBy`; a
  synthesized edge without one shows `synthesizedBy:<metadata.synthesizedBy>`;
- an unresolved reference by its language and `reference_kind`; and
- a file by its language.

The exit status is 0 when the graphs are identical and 1 when they differ. It is
2 when an input cannot be read, loaded, or canonicalized, and nothing is printed
to stdout in that case. `--graph-diff` runs on its own; other options are
ignored.

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
