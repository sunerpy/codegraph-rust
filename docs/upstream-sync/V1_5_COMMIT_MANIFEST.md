# Colby v1.5.0 commit manifest

This is the frozen scope inventory for `v1.4.1..v1.5.0`.

- Baseline: `ecc8b307ac2f8a7d06bff02ee513c4ea2380b2f8`
- Target: `ea72e1b190921232aa7bd02e96bef5bbe4fe0ab6`
- Commit count: `86`
- Source-list SHA-256:
  `60746f77a9ce721c10b83bc5bb8804c153f7da84217c4afb96f26645c6aa33f7`
- Reproduction: clone `https://github.com/colbymchenry/codegraph.git`, emit
  `git log --reverse --format='%H|%s'` over the full baseline and target SHAs,
  truncate each full SHA to exactly seven lowercase hex characters, and format
  each pipe-delimited row with backticked `<short>` followed by the subject. The
  pre-Red provenance gate requires exactly 86 lines and the hash above;
  unavailable network is a named environment failure, not a skip.
- Review provenance is external to this immutable file: both review prompts must
  name the plan path + SHA-256 and this manifest path + SHA-256. Final verdicts
  and those exact hashes are recorded in the implementation evidence ledger at
  `docs/upstream-sync/V1_5_PORTABLE_FIXES.md`; embedding this file's own hash here
  would make a reproducible hash impossible.

`PORT` means behavior lands in the Rust implementation. `ALREADY-HAVE` means the
current Rust architecture already supplies the behavior and keeps a regression
contract. `DEFER` is portable but intentionally outside this release. `N/A`
means implementation/runtime/tooling-specific or prose-only. All `N/A` and
behavior-neutral performance rows have no golden effect.

`Snapshot` is row-level provenance. Unchanged Revision 5 classifications retain
`R5`; rows whose classification or evidence wording changed in Revision 6 use
`R6`. The whole-file SHA, not a uniform row label, identifies the reviewed freeze.

| # | Commit | Upstream change | Disposition | Rust landing/test and golden effect | Snapshot |
|---:|---|---|---|---|---|
| 1 | `6103f5e` | fix(cpp): resolve explicit operator calls (a.operator+(b)) to the operator method (#1268) | PORT | Batch B explicit-operator extraction/resolution; named C++ additions | R5 |
| 2 | `e871c49` | fix(resolution): clean up processed refs by row id so batch boundaries can't drop sibling call sites (#1269) (#1270) | PORT | Batch C3 exact persisted row IDs; golden-neutral | R5 |
| 3 | `2b0b4b5` | ci(release): publish npm packages with provenance and attest release bundles (#1296) | N/A | npm publishing is absent | R5 |
| 4 | `a66683d` | feat(installer): offer CodeGraph Pro beta signup after install and upgrade (#1297) | N/A | product/UI-only | R5 |
| 5 | `243ef1d` | ci(release): switch npm publishing to OIDC trusted publishing; document verified releases (#1298) | N/A | npm publishing is absent | R5 |
| 6 | `ad5300a` | fix(ui): show synthesis as a 'Linking dynamic dispatch' phase; mute node:sqlite warning spam (#1299) | N/A | upstream UI-only | R5 |
| 7 | `246aee8` | fix(ui): within-pass progress for the C fn-pointer linking pass (#1300) | N/A | upstream UI/progress-only | R5 |
| 8 | `5736e24` | perf(index): faster fresh indexing + parallel reference resolution, byte-identical graphs (#1305) | N/A | Rust index pipeline architecture differs; behavior-neutral | R5 |
| 9 | `d6efd43` | fix(cli): honor NO_COLOR/--no-color and go plain when stdout is piped (#1306) | DEFER | separate CLI presentation item; no graph effect | R5 |
| 10 | `3042195` | fix(ui): consistent frame glyphs on Windows — agree with clack, keep raw path ASCII (#1307) | N/A | upstream terminal UI-only | R5 |
| 11 | `e1f339f` | fix(go): require URL-shaped paths for route detection (#1308) | DEFER | separate route-extraction behavior; no v1.5 selected dependency | R5 |
| 12 | `4dd29ea` | fix(cpp): strip template args from out-of-line method receiver qualifiers (#1309) | PORT | Batch B; named C++ additions | R5 |
| 13 | `e437918` | fix(cpp): compose namespace prefix into out-of-line method qualified names (#1310) | PORT | Batch B; named C++ additions | R5 |
| 14 | `b6a05d1` | fix(c): blank leading attribute macros so functions index under real names (#1311) | PORT | Batch B; named C++ corpus additions | R5 |
| 15 | `18f0745` | perf(sync): defer WAL autocheckpoint for the whole incremental run (#1312) | DEFER | current WAL deferral is full-index-only; incremental performance work is outside this release | R5 |
| 16 | `8dcf92f` | fix(watch): schedule a sync when a directory is deleted (#1313) | PORT | Batch M directory-removal event schedules one full sync; golden-neutral | R5 |
| 17 | `ce983a0` | fix(cli): `node <symbol> -f <file>` includes the source body (#1314) | PORT | Batch A4 combined symbol/file disambiguation and source body; golden-neutral | R6 |
| 18 | `2ec877b` | fix(resolution): calls through an imported singleton resolve to the method (#1315) | PORT | Batch C1; golden-neutral | R5 |
| 19 | `41c2029` | fix(go): field-chain calls resolve via validated type inference, never bare-name guessing (#1316) | DEFER | separate multi-hop type analysis | R5 |
| 20 | `c472cfb` | fix(resolution): literal-receiver builtins and nested locals stop fabricating call edges (#1317) | PORT | Batch C2; golden-neutral | R5 |
| 21 | `a5a8942` | fix(scan): includeIgnored child patterns revive repos under a gitignored parent (#1318) | ALREADY-HAVE | behavioral reinclusion exists through `indexing.include`; upstream field name intentionally absent; regression evidence only | R6 |
| 22 | `1de7e8f` | fix(retrieval): multi-hump field-name queries reach their definers (#1319) | PORT | Batch A2; golden-neutral | R5 |
| 23 | `a2f3c31` | perf(resolution): defer checkpoints and double-buffer persist during resolution, byte-identical graphs (#1320) | N/A | upstream worker/DB architecture; byte-neutral perf | R5 |
| 24 | `cf38ef6` | perf(synthesis): fan dynamic-dispatch passes across the resolver pool, byte-identical graphs (#1321) | N/A | upstream worker-pool architecture; byte-neutral perf | R5 |
| 25 | `567b4ad` | perf(resolution): drop non-unique edge indexes during the bulk resolution window, byte-identical graphs (#1322) | N/A | upstream DB implementation detail; byte-neutral perf | R5 |
| 26 | `4efc6c7` | fix(scale): kernel-scale hardening — OOM-safe pass skipping + watchdog-safe index recreate (#1323) | N/A | upstream Node+napi migration architecture | R5 |
| 27 | `c5eebe6` | feat(kernel): R1 scaffold — napi-rs extraction kernel, buffer contract, routing + fallback, grammar-parity CI | N/A | product is already native Rust | R5 |
| 28 | `9ad5cd7` | feat(kernel): R2 — full TypeScript/JavaScript extraction port, byte-parity with the wasm path | N/A | product is already native Rust | R5 |
| 29 | `c8cca9a` | feat(kernel): R3 — TS/JS equivalence gate passed, kernel default-on | N/A | product is already native Rust | R5 |
| 30 | `03d54e4` | feat(kernel): R4 — Java port with Lombok synthesis, gate passed, default-on | N/A | product is already native Rust | R5 |
| 31 | `28068fa` | perf(kernel): direct-to-store decode — buffers flow to the store worker, main thread never materializes nodes | N/A | upstream napi/worker architecture | R5 |
| 32 | `c2503e2` | feat(kernel): R5 — Python and Go ports, gates passed, default-on | N/A | product is already native Rust | R5 |
| 33 | `f07fd54` | docs(kernel): record 2-CPU django/prometheus benchmarks in §4e | N/A | prose-only | R5 |
| 34 | `2a79432` | docs(kernel): R6 — kernel-scale re-validation record (§4f) + parity-harness symlink robustness | N/A | prose-only | R5 |
| 35 | `8060da2` | docs(kernel): make the migration plan a cold-start handoff — status checklist, §0a operational handoff, superseded-expectation annotations | N/A | prose-only | R5 |
| 36 | `c1dc78d` | Merge pull request #1326 from colbymchenry/rust-kernel | N/A | merge record; constituent commits classified | R5 |
| 37 | `9e18ac2` | docs(kernel): P1 first measurement round — premise correction (cpuset-blind pool), OOM + WAL-pinning findings, revised P1 order; O1 merged, O2 in progress (#1327) | N/A | prose-only | R5 |
| 38 | `5e329ad` | fix(kernel): CRLF docstring parity — JS multiline ^ anchors after \r, regex crate's (?m)^ is \n-only (#1329) | N/A | upstream kernel regex quirk; Rust extractor path differs | R5 |
| 39 | `04ab45c` | docs(kernel): O2 Windows VM validation closed — win32-arm64 native build + 33/33 suites, CRLF find credited (#1330) | N/A | prose-only | R5 |
| 40 | `6e52295` | fix(resolution): WAL containment for the pooled superphase — writer backpressure at pool-idle boundaries (#1332) | N/A | upstream pooled resolver architecture | R5 |
| 41 | `b8833fe` | feat(resolution): memory-aware, cgroup-honest worker-pool sizing + CODEGRAPH_RESOLVE_WORKERS (#1333) | N/A | upstream worker-pool architecture | R5 |
| 42 | `8c1e821` | fix(db): WAL valve — TRUNCATE at parked barriers, futility latch, CODEGRAPH_WAL_VALVE_DEBUG (#1334) | ALREADY-HAVE | Rust WAL valve/maintenance controls already present | R5 |
| 43 | `ca88d3b` | fix(db,resolution): WAL file cap + cgroup cache credit + pool/parse sizing corrections from the instrumented kernel-scale runs (#1335) | N/A | upstream cgroup/worker implementation | R5 |
| 44 | `2adc7f6` | fix(db): WAL truncate at parked barriers ONLY — the timer-path truncate loses the race it was assumed to lose (#1336) | N/A | upstream worker barrier implementation | R5 |
| 45 | `19cf1ec` | docs(kernel): P1 record runs — 2c 20.4min (-23%), 8c 18.3min no-OOM, WAL 14x contained; resolution measured core-invariant (#1338) | N/A | prose-only | R5 |
| 46 | `7cc2366` | perf(resolution): batch-loop de-quadratic — keyset reads, changes-based guard, DB-scaled valve caps + resolve profiler (#1339) | N/A | upstream DB/pool performance implementation | R5 |
| 47 | `9f10318` | docs(kernel): §7a.3 batch-loop profile round — countGuard quadratic eliminated, envelope 19.3min; two theories falsified by measurement (#1340) | N/A | prose-only | R5 |
| 48 | `d510de4` | perf(synthesis): cFnPtrEdges 2.07x at kernel scale — probe-profiled, edge set hash-identical (#1341) | DEFER | behavior-neutral performance follow-up | R5 |
| 49 | `636c74a` | docs(kernel): §7a.4 cFnPtr round record — 2.07x standalone, 17.6min envelope, LRU-cyclic-thrash lesson (#1342) | N/A | prose-only | R5 |
| 50 | `705e501` | docs(kernel): sync §0 P1 checklist with the completed §7a.3/§7a.4 rounds (#1343) | N/A | prose-only | R5 |
| 51 | `34ad080` | docs(kernel): R7a survey complete — the C/C++ bug-for-bug port checklist (#1344) | N/A | prose-only | R5 |
| 52 | `44561b6` | feat(extraction): vendor current C/C++ grammars (R7a prep) — c v0.24.2 + cpp v0.23.4, sha-matched (#1345) | N/A | Rust uses crate-pinned native grammars | R5 |
| 53 | `2d72891` | feat(kernel): R7a C/C++ walker — dual-lang ccpp module, preParse hoist, 7 new blanks, c/cpp default-routed (#1346) | N/A | product already has native C/C++ walker; selected fixes port separately | R5 |
| 54 | `b9d0f57` | feat(extraction): C deferral round 2 — 8 new preParse passes, linux kernel/+mm/ deferral 58.6%→33.9% (#1353) | DEFER | performance/coverage program outside selected fixes | R5 |
| 55 | `5955d04` | docs(kernel): §7a.6 per-ref measurement round — pool works, two cache theories killed, writes-under-readers named (#1354) | N/A | prose-only | R5 |
| 56 | `971a5a0` | perf(resolution): worker connection recycling — WAL-depth writes-under-readers fix, superphase −11.4% at 8c (#1362) | N/A | upstream worker pool architecture | R5 |
| 57 | `b877db6` | docs(kernel): §7a.8 cFnPtr calibration — strip rewrite killed by measurement, fuse-then-link is step 1 (#1363) | N/A | prose-only | R5 |
| 58 | `c6850d7` | perf(resolution): cFnPtr fuse-then-link — one extraction sweep + filtered verbatim linking, pass −22% at kernel scale (#1364) | DEFER | behavior-neutral performance follow-up | R5 |
| 59 | `69ea438` | perf(kernel): cFnPtr native extraction sweep — step 2, pass 230→151s across the arc (§7a.10) (#1365) | N/A | upstream napi kernel architecture | R5 |
| 60 | `9647771` | docs(kernel): §7a.11 continuous-shallow WAL probe — killed by measurement, fold I/O is a fixed budget (#1366) | N/A | prose-only | R5 |
| 61 | `f6d8e8f` | perf(store): parse-lane index deferral — dubbo fresh init −19%, kernel-scale envelope best-ever 14.2min (§4d round 1) (#1368) | N/A | upstream store performance implementation | R5 |
| 62 | `ce0ae30` | perf(store): resolution ref-index window — kernel-scale resolution 423→276s, 8c envelope ≈11min (§4d round 2) (#1369) | N/A | upstream store performance implementation | R5 |
| 63 | `f1ca991` | feat(kernel): R7b Rust walker — rustlang module, tree-sitter-rust 0.24.2 bump, rust default-routed (#1371) | N/A | product already has native Rust walker | R5 |
| 64 | `286e9cc` | feat(kernel): R7b C# walker — csharp module, tree-sitter-c-sharp 0.23.5 pin, csharp default-routed (#1378) | N/A | product already has native C# walker | R5 |
| 65 | `1909931` | feat(kernel): R7b Ruby walker — ruby module, tree-sitter-ruby 0.23.1 bump, ref-flag wire slot, ruby default-routed (#1379) | N/A | product already has native Ruby walker | R5 |
| 66 | `a6c62d7` | feat(kernel): R7b PHP walker — php module, tree-sitter-php 0.24.2 bump, php default-routed (#1380) | N/A | product already has native PHP walker | R5 |
| 67 | `09e301b` | feat(kernel): R7b Swift walker — swift module, tree-sitter-swift 0.7.3 bump, swift default-routed (#1381) | N/A | product already has native Swift walker | R5 |
| 68 | `45a53eb` | feat(kernel): R7b Kotlin walker — kotlin module, vendored-grammar-C build, kotlin default-routed (#1382) | N/A | product already has native Kotlin walker | R5 |
| 69 | `b2f9ab1` | feat(kernel): R7b R walker — rlang module, tree-sitter-r 1.2.0 crate pin, r default-routed (#1383) | N/A | product already has native R walker | R5 |
| 70 | `e321351` | feat(kernel): R7b Lua+Luau walker — one lua module, vendored-grammar-C lua v0.4.1, tree-sitter-luau 1.2.0 pin, both default-routed (#1384) | N/A | product already has native Lua/Luau walker | R5 |
| 71 | `bdd687b` | feat(kernel): R7b Scala walker — scala module, vendored-grammar-C master@0aca5d0a6f, scala default-routed (#1385) | N/A | product already has native Scala walker | R5 |
| 72 | `d1b75a1` | feat(kernel): R7b Dart walker — dart module, vendored-grammar-C d4d8f3e + wasm byte-copy vendor, dart default-routed (#1386) | N/A | product already has native Dart walker | R5 |
| 73 | `3c1f30a` | docs(kernel): mark R7b complete — 20 languages default-routed, Linux leg validated (#1387) | N/A | prose-only | R5 |
| 74 | `27c3c55` | perf(resolution): darwin-honest memory budget — vm_stat-based availability unstrangles the resolver pool on macOS (#1388) | N/A | upstream worker-pool implementation | R5 |
| 75 | `082ea65` | perf(synthesis): provably-empty pass gates + prefilters — render/expo/rn/mybatis stop scanning repos they can't match; iface memo (#1389) | DEFER | behavior-neutral performance follow-up | R5 |
| 76 | `1aa4de6` | perf(resolution): adaptive pool engagement — projected-settle bar replaces the fixed 150k-ref gate for mid-run boot; tokio −23% (#1390) | N/A | upstream worker-pool implementation | R5 |
| 77 | `abb0a91` | perf(resolution): per-context basename index for Lua/Luau require resolution — kong fresh index −16% (#1391) | DEFER | behavior-neutral resolver performance follow-up | R5 |
| 78 | `974e6c8` | perf(resolution): incremental receiver-inference scan memo + compiled-pattern memo — kong −8% more (−23% cumulative), byte-identical (#1392) | DEFER | behavior-neutral resolver performance follow-up | R5 |
| 79 | `157c8e7` | perf(resolution): generation-tagged supertype memo + method owner index — Swift compiler 185→98s, byte-identical (#1395) | DEFER | behavior-neutral resolver performance follow-up | R5 |
| 80 | `c74e8b0` | perf(sync): adaptive quick-fire debounce + scoped watcher sync — save-to-graph well under a second at any scale (#1397) | DEFER | separate watcher latency program | R5 |
| 81 | `f8e6f00` | chore: gitignore target-linux/ cross-build cache (two cache files slipped into #1397) (#1398) | N/A | upstream build-cache cleanup | R5 |
| 82 | `0f1096e` | docs: Opus 4.8 benchmark re-validation + release-notes headline (#1399) | N/A | prose-only | R5 |
| 83 | `7d4a3d0` | docs(release): README polish + v1.5.0 (#1400) | N/A | prose/release metadata | R5 |
| 84 | `a6682c6` | ci(release): kernel builds required + full walker-parity gate (#1401) | N/A | upstream kernel-specific CI | R5 |
| 85 | `9b1fd6d` | release: sync package-lock.json to 1.5.0 | N/A | npm lockfile release metadata | R5 |
| 86 | `ea72e1b` | docs(changelog): promote [Unreleased] into [1.5.0] | N/A | release prose | R5 |

## Issue-only selected behaviors

These reviewed upstream issues are part of the selected Rust behavior release but
do not appear as separate commits in the frozen `v1.4.1..v1.5.0` list.

| Issue | Disposition | Rust landing/test | Golden effect |
|---|---|---|---|
| #1372 | PORT | Batch A1 Unicode search scoring + real CLI/MCP contracts | none |
| #1359 | PORT | Batch A3 enqueue-once unit instrumentation | none |
| #1348 | PORT | Batch D1 bounded React opening-tag scan | none |
| #1350 | PORT | Batch D2 DFM nesting/end lines | none |
| #1182/#1209 | PORT | Batch D3 MyBatis/iBatis forms and qualified refids | none |
| #1349 | N/A | forbidden node-ID byte-offset/column discriminator | none |
| #1243 | N/A | periodic MCP network update notice rejected by policy | none |
| #1351 / `572d22b` | ALREADY-HAVE | TOML array-of-tables regression already present | none |

## Review closeout

This manifest is immutable once its SHA-256 is submitted for review. Record both
review verdicts and the exact reviewed plan/manifest hashes plus the source-list
hash in the implementation evidence ledger, not by modifying this file. Any
manifest change requires a new plan/manifest freeze and both reviews again.
