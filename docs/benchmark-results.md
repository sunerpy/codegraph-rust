# Benchmark result status

**No current benchmark snapshot is claimed by this revision.**

The repository contains the benchmark harness and pinned corpus registry, but the
isolated implementation worktree used for this documentation update did not
contain the ignored fetched corpora or a built pinned upstream CLI. Running only
the Rust arm, inventing an upstream command, or reusing a result from an unnamed
older commit would not satisfy the comparison contract.

Accordingly, the landing pages make no quantitative latency or throughput claim.
Generate this file from a complete run using the command and evidence checklist in
[`benchmark.md`](benchmark.md). Commit a generated result only when it identifies
both implementation commits, all corpus pins, environment, cache policy, run
count, schema controls, raw samples, and dispersion.
