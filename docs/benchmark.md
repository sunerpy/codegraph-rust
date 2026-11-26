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

`--ab` times one operation of a baseline CodeGraph binary and one or more
candidates on the same pinned corpora, and checks that each binary builds the
same graph on every run. It compares builds of this project, such as release
builds of a base commit and of a change; the upstream CLI is not involved.

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
stderr. The other options:

- `--candidate <BIN>` may be repeated: every candidate is timed in the same
  session, in one rotation with the baseline, and compared with the baseline
  on its own.
- `--threads` takes `RAYON_NUM_THREADS` values, comma-separated, each its own
  cell. `default` is a cell with the variable removed, so rayon sizes its
  pool to the CPUs the process may use; it is the only cell when `--threads`
  is omitted.
- `--op` picks what each run times: `init` (the default), `sync-edit:<N>`, or
  `sync-edit:1:<path>` (see [What each run does](#what-each-run-does)).
- `--no-load-guard` times without the [load guard](#load-guard). On a host
  without `/proc/stat` the guard is off.
- `--guard-max-foreign-saturating <CPUS>`, `--guard-max-foreign-partial <CPUS>`,
  and `--guard-start-max-load <LOAD>` replace the guard's foreign-CPU limit for
  a cell whose threads cover every CPU, its limit for any other cell, and the
  load each quiet-start check must stay below (defaults 0.5, 2, and 3). Each
  takes a non-negative number and is refused with `--no-load-guard`.

### What each run does

Every child runs with `RAYON_NUM_THREADS=<threads>` (removed for the `default`
cell), `CODEGRAPH_NO_DAEMON=1`, `CODEGRAPH_NO_WATCH=1`, and
`CODEGRAPH_PHASE_TIMINGS=1`, with `CODEGRAPH_DIR` removed from the
environment. The CLI has no thread-count flag. Indexing parses and resolves on
rayon's global thread pool, which takes its size from `RAYON_NUM_THREADS`, so
each thread count is a separate cell with that variable set.

With `--op init`, for each corpus, thread count, and run, the harness:

1. copies the corpus's benchmark directory into a fresh temporary project,
   leaving out `.git`, `.codegraph`, and `.codegraph-wsl` and skipping symlinks;
2. runs `<binary> init <project>`;
3. records the wall time from spawn to exit, polled every millisecond; the
   peak RSS: on Linux the child's own `VmHWM`, read from `/proc/<pid>/status`
   at each poll, since `wait4`'s `ru_maxrss` also counts the address space the
   child was forked from (the harness's, which grows with every database it
   canonicalizes); `ru_maxrss` on other Unix hosts; not collected on Windows;
   the child's user and system CPU time from `wait4`; and the phase-timing
   lines of its stderr (the
   [CLI's Index diagnostics](cli.md#index-diagnostics)), in milliseconds by
   phase;
4. checks `.codegraph/codegraph.db` with `PRAGMA quick_check`, canonicalizes it
   through the [equivalence oracle](equivalence.md), and fingerprints it; and
5. deletes the project.

With `--op sync-edit:<N>` or `--op sync-edit:1:<path>`, each binary of a cell
gets its own copy of the corpus, indexed by an untimed `init`. The edit
appends one line, `<comment>codegraph-bench sync-edit <run>`, to each edited
file: the first `N` indexed files in path order whose language takes a line
comment (`//` for the C family, JavaScript, TypeScript, Rust, Go, Java, Kotlin,
Swift, Dart, Scala, and Solidity; `#` for Python, Ruby, R, Nix, YAML,
Terraform, properties, and GDScript; `--` for Lua; `%` for Erlang), or the one
indexed file `<path>`. One untimed warm-up sync comes first; then each run
appends another line and times `<binary> sync <project>`, recording what an
`init` run records except the graph. Every sync must print
`Synced: <n> reindexed` with `<n>` the number of edited files. At the end of
the cell each edited file is set to its original text plus one final comment
line, an untimed sync indexes that, and the synced graph must equal a fresh
`init` of the same source by the same binary. That graph is the cell's graph
for the comparisons below.

Run `r` of a cell starts with arm `r mod n` of its `n` binaries, the baseline
first and then the candidates in order: with one candidate, even runs time the
baseline first and odd runs the candidate first. The first run of every
(corpus, binary, thread count) cell is a warm-up. It is checked like every
other run but left out of the statistics. "Cold" means that no index exists
when `init` starts; the page cache is warm, because the project was just
copied.

The fingerprint hashes each canonical surface with SHA-256: for `nodes`,
`edges`, `unresolved_refs`, and `files`, the compact JSON array of the
surface's canonical rows; for `schema`, the normalized schema text. The graph
hash is the SHA-256 of the five lines `<surface> <sha256>`, in that order.

### Load guard

Timing starts on a quiet host, and a run that shares its CPUs with other work
is void and runs again:

- At the start of a session, three checks in a row, 20 s apart, must each find
  the 1-minute load below 3, no `cargo`, `cargo-*`, `rustc`, `rustdoc`, or
  `clippy-driver` process of any user, and at least 90% of the CPU time idle
  over a 1 s read of `/proc/stat`. After 80 minutes without them the harness
  exits and times nothing.
- Before each run, the 1-minute load must be below 8 and a 1 s sample must find
  the cell's idle CPUs: 7/8 of the CPUs, rounded down, when the cell's threads
  cover every CPU (`default` included), and 3/4 otherwise. On 32 CPUs that is
  28 idle CPUs for 32 threads and 24 for 4; on a 4-CPU runner, 3 for 4. A run
  waits for both, 10 minutes at most, or the harness stops.
- After each run, the CPU time all CPUs spent busy during it (`/proc/stat`),
  less the child's own (`wait4`) and the harness's, divided by the run's wall
  time, is its foreign load. Above 0.5 CPU (threads covering every CPU) or 2
  CPUs (fewer threads) the run is void: it stays in the report with the reason
  and runs again at once. A warm-up is never void. Each binary of a cell runs
  at most twice the requested runs.

The start check's load limit and the two foreign-CPU limits are
defaults; the `--guard-*` options above set them for a session, and the
report's `settings.load_guard` records the ones in force. Every run's foreign
load is in the report, a void run's included, so a result can cite the
contamination it was timed under. The counters tick at the kernel's clock
rate, so a run of a fraction of a second can read a few tenths of a CPU of
foreign load that was not there; on a host shared with other work the
background alone can exceed the defaults.

### Failure policy

The harness stops with a non-zero exit status, and writes no report, when:

- a run exits non-zero, is killed by a signal, or runs longer than 30 minutes;
- an `init` exits 0 but leaves no database, or leaves one that fails
  `PRAGMA quick_check` or canonicalization;
- two runs of the same binary on one corpus produce different graph hashes, at
  the same or at different thread counts;
- a sync re-indexes another number of files than the run edited, or its graph
  differs from a fresh `init` of the same source;
- a phase-timing line is malformed, comes from another command, or repeats a
  phase, or two runs of one binary in a cell report different phases; or
- the load guard finds no quiet start, waits out a run, or voids so many runs
  that a cell cannot collect its runs.

A difference between the baseline and a candidate graph is not a failure. The
report records it per corpus, so a change that alters the graph can still be
timed against its row growth; `--graph-diff` shows the rows.

### Report

The JSON report has `kind` `codegraph-ab` and `schema_version` 2. Its fields:

- `argv`: the `bench` command line.
- `environment`: CPU model, logical CPUs, memory, OS, kernel, `rustc --version`,
  the harness commit and whether its worktree was dirty, and how RSS was
  collected.
- `settings`: the invocation, the operation (`op`), environment overrides,
  thread variable, thread counts (`threads`, a `default` cell's being the
  CPUs) and `thread_settings` as given, runs, discarded warm-ups, run order,
  run timeout, budget limits, and the load guard's limits (`null` when off).
- `session_start`: the load guard's start checks, each with its load, idle
  fraction and builds, and how long they took.
- `baseline`, `candidate`, `candidates`: the path, SHA-256, size, and
  `--version` line of each binary; `candidate` is the first of `candidates`.
- `corpora[]`: the name, pinned commit and tag, source directory, copied files,
  skipped symlinks, and for a sync op the edited files of each corpus.
- `corpora[].graphs[]`, one per candidate: `identical`, `differing_surfaces`,
  and both binaries' fingerprints: `hash`, per-surface hashes, and canonical
  row counts. `graph` is the first candidate's.
- `corpora[].cells[]`: one entry per thread count, with `default_threads` for
  the `default` cell, the baseline's runs, and in `candidates[]` each
  candidate's runs, its candidate/baseline `ratio` of wall time, peak RSS,
  rows, and rows per second, its `budget` verdict, and its `phase_ratio` of
  each phase both report; `candidate`, `ratio`, and `budget` are the first
  candidate's. A binary's runs hold every run (`run`, `warmup`, `wall_ms`,
  `peak_rss_kb`, `cpu_ms`, `phase_ms`, an `init` run's `canonical_hash`, the
  guard's `load` with its `load1`, `idle_cpus`, `waited_ms`, and
  `foreign_cpus`, and `void` with its reason), and the median and MAD of the
  measured runs, those neither warm-up nor void: wall time, peak RSS, CPU
  time, foreign CPUs (`foreign_cpus`), each phase, and for `init` the
  canonical rows per second at the median.

`--report-md` renders the same data as Markdown: a table of the binaries,
their hashes, and the environment; one row per corpus, thread count, and
candidate; one row per phase both binaries of a cell report; and with the
guard on, one row per binary and cell listing the foreign CPUs of its
warm-up, its measured runs, and its void runs, in run order.

Publishing an A/B result follows the
[evidence requirements](#evidence-requirements): the report carries the binary
hashes, the environment, every run, and the graph fingerprints.

### Adoption verdict

`crates/codegraph-bench/scripts/od9-verdict.py` applies the adoption rule of a
performance strategy (OD-9) to one or more reports:

```bash
python3 -I crates/codegraph-bench/scripts/od9-verdict.py \
  --target reference_resolution --noise-floor-ms 1.5 \
  --profile release --goldens-ok target/ab.json
```

`--target` names the phases the strategy changes, the phase its cost moves
into included, or `wall`; a run's target is the sum of their times. A
strategy is `ADOPTED` only when, for the candidate (`--candidate`, default the
first):

- every corpus's graph is identical to the baseline's, and `--goldens-ok`
  attests that `scripts/regen-goldens.sh --check` and the `golden_reextract`
  test passed on the candidate build;
- in every required cell (`--threads`, default `32,4`, on every corpus or
  those `--corpora` names), with `B` and `C` the baseline and candidate
  medians of the target over at least five measured runs each and the MAD
  unscaled, `(B - C) / B >= 0.05` and `B - C > 2 * max(MAD_B, MAD_C)`; and
- in every required cell no other phase both binaries report, nor the wall
  time, nor the peak RSS regresses: `C / B > 1.02` together with
  `C - B > 2 * max(MAD_B, MAD_C)`. A phase whose baseline median is below
  `--noise-floor-ms` is judged by its increase alone, `C - B` above the floor.

Any failed rule makes it `REJECTED_BY_PROFILE`, exit status 1; a rule it
cannot evaluate (a missing cell, too few measured runs, no golden
attestation) gives `INCOMPLETE`, exit 3. The verdict needs release-profile
builds: `--profile fast` prints the evaluation as attribution only, exit 3.
Cells may come from several reports, such as 32-thread cells timed on one host
and 4-thread cells on another, each cell from one report only. `--json`
prints the result as JSON.

### Regression budget

Measured against a release build of the base commit, a change stays within the
project's performance budget when:

- the cold `init` median is at most 1.10× the baseline's. A slower `init` is
  still within budget when the candidate graph has more canonical rows and its
  canonical rows per second stay at or above 0.90× the baseline's;
- a single-file `sync` takes at most 1.15× the baseline's time;
- the `explore` p50 is at most 1.10× the baseline's; and
- the peak RSS median is at most 1.15× the baseline's.

`--ab` measures the `init`, single-file `sync`, and peak RSS terms and reports
them per cell as `budget.init_wall_ok` (`--op init`), `budget.sync_wall_ok`
(`--op sync-edit:1` and `sync-edit:1:<path>`), and `budget.peak_rss_ok`. It
never fails a run for being over budget; `explore` is measured separately. A
change over budget is fixed, or kept only with its A/B report attached for the
owner to accept.

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

### Go standard-library method table

`--gen-go-std-methods --go-src <DIR>` regenerates the rows of
`crates/codegraph-resolve/src/go_std_methods.rs`, the methods of the
standard-library types Go code embeds (upstream's `go-stdlib-methods.ts`), from
a Go source tree. For each type a row lists, it reads that package's non-test
`.go` files with this repository's Go grammar and lists the type's methods as
`name:params/results`: an interface's own and those of the interfaces it embeds,
a named type's and those its embedded fields promote, an alias's target's.
Exported names are kept, and an unexported one only where a listed interface
requires it. Add or drop a type by editing a row, then regenerate.

```bash
target/release/bench --gen-go-std-methods --go-src /path/to/go --check
```

Without a mode it prints the rows; `--check` exits 1 when they differ from the
committed ones and prints each differing row; `--write` rewrites them. It also
exits 1 when a listed type is missing or a kept method's signature does not
read, as upstream's generator does, and 2 when `--go-src` is not a Go source
tree. The
committed rows come from Go 1.27.3: `go1.27.3.src.tar.gz` from `go.dev/dl`,
checked against the sha256 that `go.dev/dl/?mode=json&include=all` publishes
(`6e7749035cf360f06b2575d162ea79df9fda8ef671889e5c375f8e50777f26bb`) and
unpacked, its `go` directory being `<DIR>`. CI does not download Go's source; run
`--check` locally after changing the generator or the table.

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
