# Colby v1.5 performance strategies: W9-00 dispositions (2026-11-26)

This dated report records the disposition of each deferred colby v1.5.0
performance strategy of the v0.56 follow-up plan's table 9.1 (item W9-00). It
is append-only evidence: a later round adds a new dated report rather than
editing this one. `UPSTREAM.md`'s Current alignment block and Sync log take
these dispositions over in a later change.

## Rules

- OD-9 decides each strategy automatically. `ADOPTED` needs (a) every golden
  re-extraction gate passing and the A/B corpora's canonical graphs unchanged,
  (b) the target metric's median improving by at least 5% and by more than
  twice the larger MAD in every required cell (32 and 4 threads, at least six
  runs, the first dropped), and (c) no other phase, the wall time, or the peak
  RSS regressing by more than 2% and twice the larger MAD.
  `REJECTED_BY_PROFILE` is any failed rule. The verdicts use release-profile
  builds (OD-B88); the rules are applied by
  `crates/codegraph-bench/scripts/od9-verdict.py`.
- `ALREADY-HAVE` and `N/A` (OD-B84) record a strategy the port already has or
  cannot have, with a source location at the code base, without an A/B run.
- A strategy whose profiled upper bound stays below 5% in every required cell
  is `REJECTED_BY_PROFILE` from the profile alone (OD-B85).
- A strategy that drops and recreates schema objects during bulk writes moves
  them in `sqlite_master`, whose rowid order the canonical schema dump follows
  (`crates/codegraph-bench/src/oracle/canonicalize.rs`), so it fails (a) and is
  `REJECTED_BY_PROFILE` unless a variant keeps the order (OD-B86).
- `DEFER` moves a strategy to the next round by an owner decision.

Code base: `2d56231`. Evidence paths are relative to the v0.56 evidence root;
`W9/w9c/rows.md` holds the file and line evidence of every row.

## Dispositions

| #   | Upstream  | Strategy                                                | Disposition                                                 | Evidence                                        | Commit    |
| --- | --------- | ------------------------------------------------------- | ----------------------------------------------------------- | ----------------------------------------------- | --------- |
| 1   | `18f0745` | defer the WAL autocheckpoint for the whole sync         | REJECTED_BY_PROFILE (K5)                                    | `W9/w9c/rows.md`, `W9/0a4ca58/`                 | —         |
| 2   | `a2f3c31` | defer checkpoints during resolution; double buffer      | deferral ALREADY-HAVE; double buffer ADOPTED (K1)           | `W9/w9c/rows.md`, `W9/bc9cc95/`                 | `bc9cc95` |
| 3   | `6e52295` | writer backpressure under parallel resolution           | ALREADY-HAVE                                                | `W9/w9c/rows.md`                                | —         |
| 4   | `ca88d3b` | WAL file cap; cgroup cache budget and pool sizes        | WAL cap ALREADY-HAVE; the rest N/A                          | `W9/w9c/rows.md`                                | —         |
| 5   | `2adc7f6` | truncate the WAL only at a parked barrier               | ALREADY-HAVE                                                | `W9/w9c/rows.md`                                | —         |
| 6   | `971a5a0` | recycle worker read connections                         | N/A                                                         | `W9/w9c/rows.md`                                | —         |
| 7   | `c74e8b0` | quick-fire debounce for one or two saved files          | ADOPTED (K6)                                                | `W9/w9c/rows.md`, `W9/1be277d/`                 | `1be277d` |
| 8   | `5736e24` | faster fresh index (bulk FTS, multi-row INSERT, …)      | mostly ALREADY-HAVE or N/A; 8a, 8b REJECTED_BY_PROFILE (K2) | `W9/w9c/rows.md`, `W9/811e8fa/`, `W9/3506888/`  | —         |
| 9   | `cf38ef6` | run the synthesis passes in parallel                    | DEFER (next round, OD-B93)                                  | `W9/w9c/rows.md`, `W9/w9c/row9-wip-note.md`     | —         |
| 10  | `567b4ad` | drop the non-unique edge indexes during bulk resolution | REJECTED_BY_PROFILE (OD-B86)                                | `W9/w9c/schema/`                                | —         |
| 11  | `4efc6c7` | OOM-safe pass skip, watchdog-safe index rebuild         | N/A                                                         | `W9/w9c/rows.md`                                | —         |
| 12  | `28068fa` | decode straight into the store (kernel)                 | N/A                                                         | `W9/w9c/rows.md`                                | —         |
| 13  | `b8833fe` | memory-aware, cgroup-aware pool size                    | CPU term ALREADY-HAVE; memory term N/A                      | `W9/w9c/rows.md`                                | —         |
| 14  | `7cc2366` | de-quadratic batch loop; valve scaled by database size  | ①② ALREADY-HAVE; ③ REJECTED_BY_PROFILE; ④ N/A               | `W9/w9c/rows.md`, `W9/w9c/schema/`              | —         |
| 15  | `d510de4` | faster cFnPtrEdges                                      | ALREADY-HAVE                                                | `W9/w9c/rows.md`                                | —         |
| 16  | `c6850d7` | cFnPtr: fuse, then link; registration filter            | body ALREADY-HAVE; filter REJECTED_BY_PROFILE               | `W9/w9c/prescreen/summary-r2.md`                | —         |
| 17  | `69ea438` | cFnPtr native extraction sweep                          | N/A                                                         | `W9/w9c/rows.md`                                | —         |
| 18  | `f6d8e8f` | defer the parse lane's indexes                          | REJECTED_BY_PROFILE (OD-B86)                                | `W9/w9c/schema/`                                | —         |
| 19  | `ce0ae30` | resolution-time reference index window                  | REJECTED_BY_PROFILE (OD-B86)                                | `W9/w9c/schema/`                                | —         |
| 20  | `27c3c55` | macOS memory budget                                     | N/A                                                         | `W9/w9c/rows.md`                                | —         |
| 21  | `082ea65` | skip provably empty synthesis passes                    | ALREADY-HAVE                                                | `W9/w9c/rows.md`                                | —         |
| 22  | `1aa4de6` | start the resolution pool adaptively                    | ALREADY-HAVE                                                | `W9/w9c/rows.md`                                | —         |
| 23  | `abb0a91` | index Lua require targets by basename                   | REJECTED_BY_PROFILE (K4)                                    | `W9/w9c/prescreen/summary-r2.md`, `W9/be7999e/` | —         |
| 24  | `974e6c8` | memoize receiver-inference scans and compiled patterns  | ADOPTED (K4)                                                | `W9/w9c/prescreen/summary-r1.md`, `W9/330166d/` | `330166d` |
| 25  | `157c8e7` | generation-tagged supertype memo; method owner index    | ① and ② REJECTED_BY_PROFILE                                 | `W9/w9c/prescreen/summary-r1.md`                | —         |
| 26  | `8c1e821` | the WAL valve truncates at a parked barrier             | ALREADY-HAVE, with a regression test                        | `W9/w900/`                                      | `2d56231` |
| 27  | (OD-B89)  | reuse synthesis per pass at sync                        | TODO (another W9-00 item)                                   | `W9/row27/`                                     | —         |

## Profile bounds behind the profile-only rejections

Fast-profile builds of `2d56231`, `CODEGRAPH_PHASE_TIMINGS=1` and private
counters of a scratch branch that never lands, three runs per cell with the
first dropped, medians; the bound is the share of the target metric the
strategy could remove at most:

- Row 14 ③: the typescript-large index is 148,103,168 bytes after publishing,
  so the database-size clamp gives the fixed 256 MiB valve.
- Row 16: on redis-medium the files whose own unit registers nothing take
  6.41 ms of a 136.9 ms cFnPtrEdges pass with a free filter (4.7%, 32 and 4
  threads alike).
- Row 25: on alamofire-small `get_supertypes` takes at most 0.55% and the
  method candidate filter at most 0.41% of reference resolution.

Row 9 passed this pre-screen (its Amdahl bound is 67–70% on the synthesis
fixture and typescript-large, 8.3% on its lowest cell, redis-medium) and row
23 did not qualify for it (its bound is 9.33% at 32 threads); both are listed
above with their dispositions.

## Campaigns

Builds: `cargo build --locked --release -p codegraph-rs -p codegraph-bench`
with the repository's release profile (OD-B88), one per branch head; each
campaign's `provenance.txt` under `W9/w9c/campaigns/out/<campaign>/` names the
binaries' SHA-256, commit and tree. Host: one shared machine with 32 vCPUs
(Intel Xeon 6975P-C), 62 GiB of memory and Linux 7.0 on AWS. Each campaign
times all its arms together, one session per operation (`bench --ab` unless
noted below), with the load guard `--guard-max-foreign-saturating 3.0
--guard-max-foreign-partial 6.0 --guard-start-max-load 5`, which runs a voided
run again. Every cell runs at 32 and 4 threads (`RAYON_NUM_THREADS`), six runs
per arm with the first dropped as the warm-up and the arms interleaved or
rotated per run; medians and MADs (unscaled, the larger of the two arms) are
over the five measured runs. `bench --ab` has no watcher operation and copies
only a corpus's source directory, so K6 and K5's whole-checkout cell run
through the evidence harness `W9/w9c/scripts/customab.py`, which keeps bench's
guard rules, rotation, warm-up and report shape and checks each arm's final
graph against a fresh `init` with `bench --graph-diff`.

Noise floor: 31 ms. K0 timed the base build against a byte-identical copy of
itself (`W9/w9c/campaigns/out/K0/base-vs-copy.json`); `od9-verdict.py`'s rule
(c) fails the identical binaries at noise floors up to 30.0 ms and passes them
from 30.3 ms, rounded up to 31 ms. Every verdict below therefore judges an
other phase, or the wall time, whose baseline median is under 31 ms by its
absolute increase alone (more than 31 ms regresses), a longer one by the 2%
and twice-the-MAD rule, and the peak RSS always by the ratio rule. Each
verdict directory also holds the verdict under the first floor definition,
3.8 ms, as information (`verdict-floor3.8.txt`).

| Campaign | Row    | Arms: commit, codegraph SHA-256 prefix                     | Required cells                                                   | Target                               | Verdict (31 ms floor)              | Evidence                     |
| -------- | ------ | ---------------------------------------------------------- | ---------------------------------------------------------------- | ------------------------------------ | ---------------------------------- | ---------------------------- |
| K0       | —      | `2a18fb7` `43ec097f`; base `2d56231` `08316f5b`; base copy | init of the five corpora; one-file sync of typescript-large      | — (calibration)                      | noise floor 31 ms                  | `W9/w9c/campaigns/out/K0/`   |
| K1       | 2      | base; `bc9cc95` `9b73c2d2`                                 | init of typescript-large                                         | reference_resolution and its parts   | ADOPTED                            | `W9/bc9cc95/`                |
| K2       | 8a, 8b | base; `811e8fa` `6526bb23`; `3506888` `20ca3bf6`           | init of fd-small, tokio-medium, typescript-large, express-medium | init wall                            | both REJECTED_BY_PROFILE           | `W9/811e8fa/`, `W9/3506888/` |
| K4       | 23, 24 | base; `be7999e` `5a45635a`; `330166d` `0e359ec1`           | row 23: kong-medium; row 24: kong-medium, typescript-large       | reference_resolution                 | 23 REJECTED_BY_PROFILE; 24 ADOPTED | `W9/be7999e/`, `W9/330166d/` |
| K5       | 1      | base; `0a4ca58` `e7cd3774`                                 | 200-file sync of typescript-large; one-file sync of its checkout | sync and its parts                   | REJECTED_BY_PROFILE                | `W9/0a4ca58/`                |
| K6       | 7      | base; `1be277d` `4f04fb4f`                                 | tokio-medium, save to graph                                      | wall, save to `auto-synced` log line | ADOPTED                            | `W9/1be277d/`                |

The five corpora are fd-small, tokio-medium, typescript-large, express-medium
and kong-medium; every graph of every campaign was identical across arms.

- K0: W9-00a (`2d56231`) against its parent `2a18fb7` stays inside the
  plan's instrumentation line: init wall ×0.980–1.015 and peak RSS
  ×0.987–1.078 on the five corpora, the one-file sync wall ×0.991 (32
  threads) and ×0.992 (4 threads).
- K1, row 2: reference_resolution with its parts on typescript-large went
  from 11589.8 to 8379.7 ms (−27.70%, Δ 3210 ms against 2·MAD 71 ms) at 32
  threads and from 27897.8 to 23619.8 ms (−15.33%, Δ 4278 ms against 496 ms)
  at 4; no other timing and not the peak RSS regressed.
- K2, row 8a (fill the search index once): init wall improved by 5.2–15.3% in
  five cells but only by 4.95% (fd-small, 32 threads), 2.94% (fd-small, 4) and
  3.97% (express-medium, 4) in three, and the peak RSS (fd-small, +2.1% and
  +3.3%), `publish` (typescript-large, +2.3% and +3.5%) and
  reference_resolution (fd-small at 4 threads, tokio-medium at 32) regressed.
- K2, row 8b (multi-row INSERT): init wall improved by 5.4–13.0% in six cells
  but only by 3.58% (fd-small, 4 threads) and 4.25% (express-medium, 4) in
  two, and timings regressed on tokio-medium and typescript-large, among them
  `reference_resolution.insert_edges` (+6.9–8.2%), `edge_write` and
  `reference_write` (+4.1–4.3%) and `synthesis` (+2.9–3.4%).
- K4, row 23: reference_resolution on kong-medium went from 413.7 to 419.0 ms
  (+1.28%) at 32 threads and from 1147.5 to 1126.0 ms (−1.87%, inside 2·MAD
  28.5 ms) at 4; the profile bound of 9.33% assumed every scan on the
  critical path.
- K4, row 24: reference_resolution went from 413.7 to 354.6 ms (−14.29%) and
  from 1147.5 to 813.5 ms (−29.11%) on kong-medium, from 5938.5 to 5581.1 ms
  (−6.02%) and from 14292.1 to 12194.5 ms (−14.68%) on typescript-large; no
  other timing and not the peak RSS regressed.
- K5, row 1: the sync phase with its parts went from 173103 to 106891 ms
  (−38.25%) at 32 threads and from 173820 to 104193 ms (−40.06%) at 4 on the
  200-file sync of typescript-large (sync wall ×0.618 and ×0.600), but from
  8456.0 to 8516.4 ms (+0.71%) and from 8456.2 to 8565.2 ms (+1.29%) on the
  one-file sync of the whole TypeScript checkout (39,330 files), where
  `synthesis.read` and four synthesis passes also regressed by 2.2–3.3%.
- K6, row 7: the time from writing `lib.rs` of tokio-medium to the server's
  `auto-synced` log line went from 2097.2 to 385.4 ms (−81.62%) at 32 threads
  and from 2094.5 to 384.7 ms (−81.63%) at 4, and the peak RSS did not
  regress; the base waits out the default 2000 ms debounce, row 7 its 300 ms
  quick window.

A verdict judges only the required cells. As information, rule (c) at the
31 ms floor on the campaigns' other corpora (fd-small, tokio-medium and
express-medium): row 24 regressed nothing; row 2 raised the peak RSS by
2.3–5.8% in five of six cells, `publish` by 4.0% (tokio-medium, 4 threads)
and `parse_write` by 6.4% (express-medium, 32 threads)
(`W9/w9c/campaigns/out/K1/guard-rails-information.txt`).
