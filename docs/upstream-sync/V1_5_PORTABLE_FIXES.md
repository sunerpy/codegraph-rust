# Colby v1.5 portable-fixes evidence ledger

This ledger records the reviewed provenance bootstrap and will accumulate the
Red/Green evidence for the selected colby v1.5 portable fixes. The frozen plan
remains ignored and local; this file records its reviewed identity without
tracking or modifying it.

## Bootstrap provenance

- Reviewed base and branch pre-bootstrap HEAD:
  `aba40799ecacb94515f7e1690914d2accc4c8973`
- Fetched `origin/main` at bootstrap:
  `aba40799ecacb94515f7e1690914d2accc4c8973`
- Frozen Revision 14 plan SHA-256:
  `5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450`
- Frozen source-list SHA-256 (exact 86-row projection):
  `60746f77a9ce721c10b83bc5bb8804c153f7da84217c4afb96f26645c6aa33f7`
- No Cargo command has run yet. The first Cargo invocation remains reserved for
  `scripts/check-workspace-versions.sh` after that script and its fixture are
  introduced without invoking Cargo.

### Frozen support-file hashes

| File                                         | SHA-256                                                            |
| -------------------------------------------- | ------------------------------------------------------------------ |
| `.gitignore`                                 | `beb58777556b1e37354e5adb3ec15834683edaaba95f15bfed13d92dda42c13a` |
| `.oxfmtignore`                               | `7fb05bd0552e6e383434d7b807f836eb8e1474e08e84adfaab09c8cd8553f103` |
| `docs/upstream-sync/V1_5_COMMIT_MANIFEST.md` | `1bf8a9022b7702368a66f0159ce84dc6ba846e5aa9c8db32f3e2ecf83affbc96` |
| `docs/upstream-sync/UPSTREAM.md`             | `e99826c37572235266299f49d206f7c441f822297cc2e5d66745a275d6bf129f` |
| `docs/upstream-sync/KNOWN_DIFFS.md`          | `2c973b6f1407b409131c20849de44f15dc9989c1b47bb278e836daf3c682c07d` |

## Complete Momus verdict

```text
**[OKAY]**
Summary: The frozen plan, manifest, and source-list hashes match exactly. The plan is executable: all 15 PORT rows map to concrete batches, behavioral Red/Green evidence, commands, and expected outcomes, with no blocking contradiction.
Verification evidence:
- Plan: 5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450
- Manifest: 1bf8a9022b7702368a66f0159ce84dc6ba846e5aa9c8db32f3e2ecf83affbc96
- Source list: 60746f77a9ce721c10b83bc5bb8804c153f7da84217c4afb96f26645c6aa33f7
PORT mapping and TDD: lines 166–184, 780–817, and 819–972; executable commands and invalid-Red exclusions are at lines 1263–1298.
Revision 14’s interrupted-uninit sequence is complete at lines 386–431 and 590–612, with crash/recovery QA at lines 721–760. Dual-slot publication, Future precedence, and Corrupt fail-closed behavior are specified at lines 452–522.
Namespace isolation, deterministic golden policy, Cargo-first-invocation gate, and terminal release DAG are covered at lines 246–287, 201–217, 1002–1081, and 1083–1257. Extraction/graph golden additions are limited to the four named C/C++ fixtures; the only separate exception is the explicitly scoped v2 MCP wording goldens.
Finding classification: Blockers: 0 · Majors: 0 · Follow-ups: 0
VERDICT: APPROVE
Open Blockers: 0
```

## Complete Codex verdict

```text
Reviewed hashes:
- Plan: 5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450
- Manifest: 1bf8a9022b7702368a66f0159ce84dc6ba846e5aa9c8db32f3e2ecf83affbc96
- Source list: 60746f77a9ce721c10b83bc5bb8804c153f7da84217c4afb96f26645c6aa33f7

Summary: No implementation-blocking defect found. The base, manifest projection, migration design, dependency order, version gate, and release topology are feasible. Four Majors must be resolved within their named batches.

Findings:
1. [Major] Future storage-protocol classification is contradictory
   Evidence: Plan lines 462-464 classify every unsupported storage protocol as `Corrupt`, while lines 468-490 classify newer extraction/storage protocols as `Future`. Tests at lines 516-520 and 740-745 require Future-protocol behavior.
   Required action: Batch M must define directional precedence, preferably protocol greater than current as `Future` and malformed/lower unsupported protocol as `Corrupt`, with exact classification and byte-snapshot tests.

2. [Major] Identical equal-sequence slots have no publication rule
   Evidence: Plan lines 465-468 reject equal-sequence slots only when payloads differ, making identical duplicates valid. Lines 502-514 require replacing an older or missing inactive slot, but neither slot qualifies in this state.
   Required action: Batch M must either classify every equal-sequence duplicate as `Corrupt` or select a fixed slot deterministically, then test the next publication and crash points.

3. [Major] Red-commit sequencing is internally ambiguous
   Evidence: Plan lines 805-809 permit API-dependent unit tests after minimal APIs compile, while lines 995-998 say each batch's tests precede its first Green edit. Repository `AGENTS.md` lines 124-134 also require running the normal verification gates before every commit.
   Required action: The evidence ledger must distinguish initial behavioral Red tests from later API-level refinements and explicitly record the expected failing gate for test-first Red commits. Compile/setup failures remain invalid evidence.

4. [Major] Already-published verification lacks an authoritative expected checksum source
   Evidence: Plan lines 1137-1145 and 1212-1237 skip builds for `already_published` while requiring exact checksum verification. Lines 1361-1371 generate expected hashes only in the original publication path. A later run has no specified trusted prior digest.
   Required action: E3 must persist an owner-bound checksum authority, such as artifact attestations keyed by release ID and tag SHA, and verify all six downloads against it without adding a seventh release asset.

5. [Follow-up] `KNOWN_DIFFS.md` overstates oracle integration
   Evidence: `KNOWN_DIFFS.md` line 3 says the file is parsed by the oracle, but all production equivalence calls pass `None`; `KnownDiffs::load` has no caller.
   Required action: E1 should correct that statement or deliberately wire the file into the oracle. No active rules currently exist, so golden safety is unaffected.

PORT coverage: complete. All 86 source rows match exactly; dispositions are 10 PORT, 2 ALREADY-HAVE, 12 DEFER, and 62 N/A. The five issue-only PORT groups produce 15 total behavior groups mapped to M/A/B/C/D with named Red and acceptance evidence.

Revision 14 uninit assessment: safe. The plan requires exclusive lease, under-lease reclassification, durable owner-bound `uninitialized` publication, tombstone, owner-bound daemon drain, and cleanup preserving the lock and both slots. Invalid, Future, owner-mismatched, and no-valid-slot-plus-DB states remain nonmutating.

TDD/verification assessment: adequate, subject to Finding 3. Reds are behaviorally defined, invalid setup/compile/network failures are excluded, and graph/golden determinism is explicitly gated.

Release assessment: adequate, subject to Finding 4. The ten-package updater is feasible, Cargo ordering is gated, permissions and pins are constrained, historical workflow retirement is explicit, and the six-target terminal DAG includes draft and public smoke.

VERDICT: APPROVE
Open Blockers: 0
```

## Codex finding assignments

| Finding                              | Assignment                               | Required closure                                                                                                                                                                                   |
| ------------------------------------ | ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Future storage-protocol direction    | Batch M                                  | A protocol greater than the current protocol is `Future`; malformed or unsupported lower protocol is `Corrupt`. Snapshot every namespace byte and prove both classifications are byte-nonmutating. |
| Identical equal-sequence slots       | Batch M                                  | Conservatively classify every equal-sequence duplicate as `Corrupt`, whether payloads are identical or different. Snapshot every byte and test publication/crash attempts remain byte-nonmutating. |
| Red-commit evidence distinction      | Cross-batch ledger rule beginning with M | Record initial black-box behavioral Red evidence separately from later API-level refinements, including the expected failing behavioral assertion or gate.                                         |
| Already-published checksum authority | E3                                       | Persist an owner-bound checksum authority keyed by immutable release ID and tag SHA; verify all six downloaded assets against it without creating a seventh release asset.                         |
| `KNOWN_DIFFS.md` oracle wording      | E1                                       | Correct the overstatement or deliberately wire the file into the oracle, and record the exact closeout diff.                                                                                       |

## Red-evidence rule

Initial black-box behavioral Red commits may fail the expected behavior tests.
Later API-level refinements follow minimal compiling scaffolding and retain the
original black-box Red as the behavioral evidence. Every Red entry must identify
the expected failing assertion, root ID, or behavior gate. Compile failures,
setup failures, fixture/network failures, and unrelated failures are never valid
Red evidence.

## E2 workspace-version gate (first Cargo invocation)

This section records the E2 prerequisite: `scripts/check-workspace-versions.sh`
plus its fixture harness `scripts/tests/check-workspace-versions.test.sh`, both
authored WITHOUT invoking Cargo. Executing the gate against the repository is the
first Cargo invocation of the whole v1.5 implementation, and that gate's first
Cargo subprocess is `cargo metadata --locked --no-deps --format-version 1`. No
Batch M / behavioral Red or other Cargo command may run before this gate
succeeds. This is not a behavioral Red; it is the release-invariant gate whose
Green output authorizes all later Cargo use.

### Cargo-ordering proof

- Test-first commit landed with ZERO prior Cargo: the implementation worktree had
  no `target/` directory and no Cargo command had run when
  `test(release): add workspace version gate`
  (`06d315eb24f51345d56752ea2eb6c3502d74e27b`) was committed on top of the
  bootstrap `a877aa30269e2fcfe7d3a484540ab05fe60da27a`.
- First-ever Cargo invocation of the task = the gate run below. The gate runs
  Cargo with the workspace root as its current working directory (`cd
"$WORKSPACE_ROOT"`), so the argv carries NO `--manifest-path`. A logging Cargo
  shim placed first on `PATH` captured exactly one Cargo call during a gate run,
  and the captured argv was exactly:

  ```text
  metadata --locked --no-deps --format-version 1
  ```

  That is the ONLY Cargo subprocess the gate spawns, it is non-mutating, and the
  argv is byte-exact — no `--manifest-path` or other extra argument. The gate
  works from any caller cwd: it derives the workspace root (positional arg, else
  the script's parent dir) and changes into it only to run this one command. The
  fixture harness scenario `F_exact_cargo_argv` invokes the gate from a caller
  cwd OTHER than the repo root and asserts this exact argv (one call, no
  `--manifest-path`) mechanically.

### First real gate run — repository Green

- Command: `scripts/check-workspace-versions.sh` (no arg → repository root).
- Exit status: `0`.
- `Cargo.lock` SHA-256 pre-run:
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`
- `Cargo.lock` SHA-256 post-run:
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`
  (byte-for-byte unchanged; the EXIT trap re-hashes and enforces this on every
  exit path.)
- Four version surfaces, all equal to `0.40.4`:
  - root `Cargo.toml` `[workspace.package] version` = `0.40.4`
  - `version.txt` = `0.40.4`
  - `.release-please-manifest.json` `"."` = `0.40.4`
  - every source-less `Cargo.lock` package version = `0.40.4`
- Ten source-less workspace packages identified (identical between
  `cargo metadata --no-deps` and the source-less `Cargo.lock` entries):
  `codegraph-bench`, `codegraph-core`, `codegraph-daemon`, `codegraph-extract`,
  `codegraph-graph`, `codegraph-mcp`, `codegraph-resolve`, `codegraph-rs`,
  `codegraph-store`, `codegraph-watch`.

### Fixture harness — eight scenarios, all as expected

`scripts/tests/check-workspace-versions.test.sh` builds dependency-free temporary
workspaces so their locks are trivially consistent (`cargo metadata --locked`
accepts them), then drives the gate. Result: `8 passed, 0 failed`. Every failure
scenario exits nonzero on a precise business assertion (never a
compile/setup/environment failure), and every scenario leaves its
`Cargo.lock` byte-for-byte unchanged (the trap-regression scenarios G/H mutate or
delete a TEMP fixture lock on purpose, and the harness verifies the REAL
repository lock is never touched).

| Scenario                   | Description                                                       | Expected  | Observed exit | Diagnostic / assertion                                                                                                       | Lock unchanged |
| -------------------------- | ----------------------------------------------------------------- | --------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------- | -------------- |
| A `manifest_lock_drift`    | manifest `0.40.4` / lock members `0.40.3`                         | nonzero   | `1`           | `Cargo.lock package 'fixture-pa' = '0.40.3' != [workspace.package] version = '0.40.4'` (and `fixture-pb`)                    | yes            |
| B `package_set_mismatch`   | extra source-less lock entry (`vendored` excluded from workspace) | nonzero   | `1`           | `workspace package set differs between cargo metadata and Cargo.lock (source-less)` … only in Cargo.lock: `fixture-vendored` | yes            |
| C `stale_version_txt`      | `version.txt` = `0.40.3`, all else `0.40.4`                       | nonzero   | `1`           | `version.txt = '0.40.3' != [workspace.package] version = '0.40.4'`                                                           | yes            |
| D `stale_release_manifest` | manifest `"."` = `0.40.3`, all else `0.40.4`                      | nonzero   | `1`           | `.release-please-manifest.json "." = '0.40.3' != [workspace.package] version = '0.40.4'`                                     | yes            |
| E `repository_green`       | the real repository lock                                          | zero      | `0`           | `check-workspace-versions: OK`                                                                                               | yes            |
| F `exact_cargo_argv`       | argv shim + caller cwd ≠ repo root                                | argv-only | `0`           | captured Cargo argv is exactly `metadata --locked --no-deps --format-version 1` — one call, no `--manifest-path`             | yes (repo)     |
| G `trap_mutation`          | shim appends bytes to a TEMP fixture lock mid-gate                | `90`      | `90`          | `CRITICAL: Cargo.lock mutated during gate`                                                                                   | repo untouched |
| H `trap_deletion`          | shim deletes a TEMP fixture lock mid-gate (under `set -e`)        | `90`      | `90`          | `CRITICAL: Cargo.lock mutated during gate` + `MISSING-OR-UNREADABLE` (trap not short-circuited)                              | repo untouched |

### Scope note

This task adds only the gate and its fixture harness. It does NOT begin Batch M,
does NOT touch the release-please `GenericToml`/`codegraph*` selector, the pinned
Action commit, any GitHub/AWS workflow, the `Makefile`, hooks, or CI wiring — all
of which remain E2 follow-up work. No version value in `Cargo.toml`, `Cargo.lock`,
`version.txt`, or `.release-please-manifest.json` was modified. No third-party
dependency was added; the gate uses only `bash`, `awk`, `sed`, `jq`, and
`sha256sum` already present in the repository's toolchain.

## Batch M initial black-box Red — isolated v2 namespace

This is the INITIAL black-box behavioral Red for Batch M, exactly as scoped by the
frozen plan (`upstream-v1.5-portable-fixes.md`) at line 805: "Batch M starts with
behaviorally failing black-box CLI/MCP/process tests that use only existing public
surfaces and filesystem artifacts." It is NOT a later API-level refinement; it
imports no proposed Green type (`IndexPaths`, `IndexLease`, `open_for_*`), which
plan lines 806-807 explicitly mark as Green design, not compile-time Red
prerequisites. No production/Green code was written.

### Pre-Red gates (all passed, in order)

- Final pre-Red base gate (plan lines 87-91): `git fetch origin main`; peeled
  remote tip `origin/main`, branch merge-base `HEAD..origin/main`, and reviewed
  base all equal `aba40799ecacb94515f7e1690914d2accc4c8973`. Local `HEAD` before
  Red = `10352ba0943622e665b0ff96c5b5f57589448c9c` (branch tip; its base is
  `aba4079`). No divergence → Red authorized.
- Network provenance gate (plan lines 40-47): fresh
  `git clone https://github.com/colbymchenry/codegraph.git`; boundary commits
  `ecc8b307ac2f8a7d06bff02ee513c4ea2380b2f8..ea72e1b190921232aa7bd02e96bef5bbe4fe0ab6`
  present; `git log --reverse --format='%H|%s'` over the two boundary SHAs yields
  exactly **86** rows; formatting each row as a numbered-free markdown table row
  `| \`<short7>\` | <subject> |\n`(backticked seven-lowercase-hex short SHA +
subject) reproduces source-list SHA-256`60746f77a9ce721c10b83bc5bb8804c153f7da84217c4afb96f26645c6aa33f7`. Match.
- Frozen identities still match: plan SHA
  `5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450`; immutable
  manifest SHA `1bf8a9022b7702368a66f0159ce84dc6ba846e5aa9c8db32f3e2ecf83affbc96`;
  `.gitignore` `beb58777556b1e37354e5adb3ec15834683edaaba95f15bfed13d92dda42c13a`;
  `.oxfmtignore` `7fb05bd0552e6e383434d7b807f836eb8e1474e08e84adfaab09c8cd8553f103`;
  `UPSTREAM.md` `e99826c37572235266299f49d206f7c441f822297cc2e5d66745a275d6bf129f`;
  `KNOWN_DIFFS.md` `2c973b6f1407b409131c20849de44f15dc9989c1b47bb278e836daf3c682c07d`.
- Workspace-version gate `bash scripts/check-workspace-versions.sh` → exit `0`
  ("check-workspace-versions: OK", workspace version `0.40.4`) run before Cargo
  test discovery; repository `Cargo.lock` SHA-256 unchanged before/after every
  Cargo invocation:
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

Legacy v0.40.4 asset fixture (plan lines 685-693) was NOT needed for this initial
boundary: the selected Red proves the namespace-placement gap using only the
current in-tree binary (`codegraph init`) and the committed `mini` corpus, with no
downloaded legacy executable. The downloaded-asset manifest is reserved for the
later legacy-isolation Red (plan tests 3/4) that actually runs the v0.40.4 binary.

### Test added

`crates/codegraph-cli/tests/batch_m_v2_namespace.rs` ::
`init_writes_isolated_v2_namespace_not_legacy_codegraph`. Black-box: drives the
shipped `codegraph init` against a private temp copy of the `mini` fixture, with
isolated `CODEGRAPH_HTTP_REGISTRY_DIR` and `CODEGRAPH_NO_DAEMON=1`, then inspects
filesystem artifacts only. The setup step (`init` succeeds) and a non-empty
built-DB byte snapshot both reach the assertion, so the failure is behavioral, not
a compile/setup/panic/network failure. The snapshot preserves the built DB bytes
that later Green asserts remain a byte-usable legacy graph.

### Discovery + execution

- Owner discovery: `cargo test -p codegraph-rs --locked --test batch_m_v2_namespace -- --list`
  → lists `init_writes_isolated_v2_namespace_not_legacy_codegraph: test`
  (`1 test, 0 benchmarks`), confirming the test binary compiles and the target
  owns the test (package `codegraph-rs`, test target `batch_m_v2_namespace`, per
  plan line 1273 "M CLI").
- Red run:
  `cargo test -p codegraph-rs --locked --test batch_m_v2_namespace init_writes_isolated_v2_namespace_not_legacy_codegraph -- --exact`
  → process exit `101` (`test result: FAILED. 0 passed; 1 failed`).
- Exact failing assertion (behavioral, plan line 262 + lines 805-817):
  "Batch M: `init` must create the isolated v2 namespace at
  `<tmp>/mini/.codegraph-v2/codegraph.db` (a sibling of the legacy root, per plan
  line 262), but no v2 DB exists; current v0.40.4 behavior wrote the legacy
  namespace instead (.codegraph/codegraph.db present=true)". This is precisely the
  Red-evidence requirement of plan lines 811-812 ("Red evidence must demonstrate
  the current fixed legacy path …").

### Classification

Initial black-box behavioral Red (plan lines 805-809), NOT a later API-level
refinement. Remaining Batch M Red is NOT yet landed and is explicitly deferred to
subsequent commits: stale-row serving / hash-skip migration (tests 2, 9),
extension-filtered folder delete (test 18), stamp/finalizer gap (test 8),
absent typed/lease/path gates (tests 6, 7), legacy-binary storage isolation
(tests 3, 4) which require the downloaded v0.40.4 asset fixture, and every
lease/lifecycle/uninit-state test (tests 5, 11, 12, 13, 15, 16, 20, 21) whose
deterministic form needs the minimal compiling Green scaffolding. This commit
lands only the initial black-box namespace boundary.

## Batch M — IndexPaths path authority + init writes isolated v2 namespace (2026-07-25)

### Scope

First minimal vertical Green of Batch M's path layer: establish
`codegraph-core::IndexPaths` as the single current/legacy path authority, and
move the default index storage root that public `codegraph init` writes from the
fixed legacy `<project>/.codegraph` to the isolated sibling
`<project>/.codegraph-v2`. This turns the already-committed initial black-box Red
`init_writes_isolated_v2_namespace_not_legacy_codegraph` green. This slice is the
PATH LAYER + init data-plane wiring ONLY; the state-slot / `IndexLease` / Store
`open_for_*` protocol, tombstone/lock consumers, daemon/watch/MCP lifecycle,
project-scoped `Config` refactor, extension/Godot DSL project-scoping, and
`uninit` remain later Batch M tasks.

### TDD evidence

- Preserved Red (unchanged): `crates/codegraph-cli/tests/batch_m_v2_namespace.rs`
  `init_writes_isolated_v2_namespace_not_legacy_codegraph`. Pre-Green failing
  command:
  `cargo test -p codegraph-rs --locked --test batch_m_v2_namespace -- init_writes_isolated_v2_namespace_not_legacy_codegraph --exact`
  → exit 101, `test result: FAILED. 0 passed; 1 failed`; failing assertion at
  `batch_m_v2_namespace.rs:157` — v2 DB `<tmp>/mini/.codegraph-v2/codegraph.db`
  absent while legacy `.codegraph/codegraph.db` present (current v0.40.4 wrote
  legacy). Re-verified once more on this task's HEAD before the Green wiring.
- API-refinement tests (plan lines 805-809 distinction): committed FIRST, before
  the data-plane wiring, as the `codegraph-core::index_paths::tests` module in
  commit `b5a66f2`. They exercise the new exported API surface directly
  (defaults, relative/absolute `CODEGRAPH_DIR`, two-project collision resistance
  on shared/escaping roots, root/dot/parent rejection, symlink alias rejection,
  legacy overlap, and every derived artifact path). Because that commit added the
  compiling API surface AND its behavioral tests together, the API tests compiled
  and passed at introduction — the compile-time Red prerequisite the plan forbids
  is satisfied by the separately preserved black-box Red above, which is the
  behavioral failure evidence; the API tests are the refinement layer.
- Post-Green: the exact black-box command now exits 0
  (`test result: ok. 1 passed`); the 18 `codegraph-core` `index_paths` unit tests
  pass.

### Commits

- `b5a66f2` `feat(core): add IndexPaths v2 namespace path authority` —
  `crates/codegraph-core/src/index_paths.rs` (new) + `src/lib.rs` export. Path
  layer + unit tests only; no data-plane consumer touched.
- (this task's Green) `feat(index): route init/default index root to isolated
.codegraph-v2 via IndexPaths` — data-plane wiring + directly-affected test
  updates (see below).

### `IndexPaths` contract landed

`IndexPaths::resolve(project, CODEGRAPH_DIR)` returns: canonical `project`;
`project_identity` (full lowercase SHA-256 of a versioned binary payload —
`st_dev`/`st_ino` on Unix, volume serial + 128-bit `GetFileInformationByHandleEx(FileIdInfo)`
on Windows via a compiled `cfg(windows)` raw-kernel32 block, NO lexical fallback,
unsupported filesystems fail closed); the normalized `legacy_roots` set (fixed
`<project>/.codegraph` plus the configured old CLI root when `CODEGRAPH_DIR` is
set); the isolated `current_root` (`.codegraph-v2` by default; a
`<name>-v2-<projectIdentity>` sibling of the configured legacy root); and the
derived `current_db`, `permanent_lock`, two `state_slots`, `tombstone`,
`config_toml`, `extension_config`, `daemon_pid`/`daemon_log`/`daemon_socket`.
This slice supplies the path VALUES only; no protocol consumer of the state-slot,
tombstone, or lock paths is implemented yet. Fail-closed: empty/root/dot/parent
aliases, symlink/reparse components below the canonical nearest-existing
ancestor, legacy equality, and ancestor/descendant overlap. Two distinct physical
projects given the same configured root receive distinct identity-suffixed
current roots. Infallible transitional helpers `current_root_lenient` /
`current_db_lenient` (used by the data plane in this slice; a not-yet-created
project must stay infallible) centralize the `.codegraph-v2` default so no
consumer reconstructs the literal; `is_reserved_index_dir` centralizes the
scanner/watcher `.codegraph`/`.codegraph-*` skip.

### Data-plane consumers routed through IndexPaths in THIS slice

- `crates/codegraph-cli/src/main.rs` `codegraph_dir` / `db_path` → default
  `.codegraph-v2`. This is the central helper for init, index, sync, status,
  unlock, and every `open_store`/`is_initialized`/`remove_db_files`/`SpillWriter`
  call site, so the whole CLI DB flow moved together.
- `crates/codegraph-mcp/src/engine.rs` `CodeGraphEngine::open` and
  `crates/codegraph-mcp/src/roots.rs` `db_path_for` → `.codegraph-v2`, so the
  stdio/HTTP/daemon MCP request path reads the SAME namespace init writes (this
  is why the serve/query integration tests stay green).
- `crates/codegraph-watch/src/sync.rs` `default_db_path` → `.codegraph-v2`, so
  watcher/catch-up sync targets the v2 DB.
- `crates/codegraph-extract/src/engine.rs` `scan_dir` now skips the whole
  reserved `.codegraph`/`.codegraph-*` family via `IndexPaths::is_reserved_index_dir`,
  so a project never scans its own `.codegraph-v2` back into the graph.

### Directly-affected tests updated (moved DB namespace only)

`crates/codegraph-cli/tests/{sync_incremental,parallel_index,status_debug_fields,godot_idfields_cwd,godot_idfields_determinism,cli_commands}.rs`,
`crates/codegraph-daemon/tests/{rmcp_tools_call,async_model}.rs`,
`crates/codegraph-mcp/tests/{golden_mcp,support/parity,godot_honesty_mcp,reopen}.rs`,
plus the `CodeGraphEngine` test helper and the `serve_services_gate` unit test in
`main.rs`. Each switched the DB (and CLI-owned lock/uninit-root) path from
`.codegraph` to `.codegraph-v2` to match the moved data plane. Daemon rendezvous
paths (`daemon.pid`/`daemon.sock`/`daemon.log`) and the MCP "must NOT self-index"
assertions in `http_serve.rs`/`zed_bare_serve.rs` were deliberately KEPT on
`.codegraph`.

### Remaining Batch M path consumers for the next integration task

These still hardcode `.codegraph` and are NOT part of this init-only slice
(listed for the follow-up integration task, per the "do not claim repo-wide
migration" rule):

- `crates/codegraph-daemon/src/paths.rs` `codegraph_dir` (and thus
  `daemon_pid_path`/`daemon_log_path`/`daemon_socket_path`/candidates) — daemon
  rendezvous still lives under `.codegraph`; moving it to the current root (with
  the v2 socket/pipe discriminator) is a later lifecycle task.
- `crates/codegraph-core/src/config.rs` `Config::discover` — still reads
  `<project>/.codegraph/config.toml` and the CWD fallback; the project-scoped
  `<current-root>/config.toml` move + migration gate is a later task (kept
  unchanged here to avoid deepening global-config bleed the plan later removes).
- `crates/codegraph-extract/src/ext_config.rs` and
  `crates/codegraph-resolve/src/frameworks/godot_dsl_config.rs` — still walk
  ancestor `.codegraph/codegraph.json`; project-scoped `extension_config` wiring
  is a later task. The two Godot idfields CLI tests therefore still write the DSL
  config under `.codegraph/` while reading the DB from `.codegraph-v2/`.
- `crates/codegraph-bench/src/pipeline.rs` `db_path` (benchmark oracle vs the
  legacy upstream CLI) — intentionally legacy; not a v2 consumer.
- The full `IndexPaths::resolve` fail-closed authority (identity-suffixed
  configured-root sibling, symlink/overlap rejection) is implemented and unit
  tested but NOT yet wired into the infallible data-plane helpers; the data plane
  uses `current_root_lenient` (simple join for a configured `CODEGRAPH_DIR`) in
  this slice. Wiring `resolve` into the state/lease protocol is the next task.

### Verification

`bash scripts/check-workspace-versions.sh` (OK, lock `750ee84b…` unchanged),
`cargo check --workspace --all-targets --locked` (clean),
`cargo clippy` across all touched crates `-D warnings` (clean),
`cargo fmt --all --check` (clean), `bash scripts/guardrail.sh` (exit 0). Targeted
green: the exact black-box Red; `codegraph-core`/`extract`/`watch` suites;
`codegraph-mcp` golden/godot/reopen/rmcp_roots/rmcp_http/rmcp_l3/rmcp_parity +
lib; `codegraph-rs` bin (353) + cli_commands/sync_incremental/parallel_index/
status_debug_fields, both `godot_idfields` CLI tests, http_serve, zed_bare_serve,
proxy_e2e, live_update_direct, stdout_purity, rmcp_serve_direct, both `explore`
CLI tests, prompt_hook, catch_up;
`codegraph-daemon` rmcp_tools_call/async_model; `codegraph-bench` (golden oracle,
byte-stability) all pass. No plan-level architecture change was required.

## Batch M — path-authority correction: close the transitional bypass (2026-07-25)

### Why this follow-up exists

Manual verification of the prior two commits (`b5a66f2` + `8aeefc4`) REJECTED
the transitional `current_root_lenient`/`current_db_lenient` bypass: for a
configured `CODEGRAPH_DIR` those helpers did a raw `project.join(value)`
simple-join, bypassing `IndexPaths::resolve`'s physical identity, alias
validation, legacy disjointness, and the identity-suffixed sibling. That let
`CODEGRAPH_DIR=.` select the project root and write `<project>/codegraph.db`,
directly contradicting the frozen configured-root contract (plan lines 246-287,
330-338). The earlier ledger's implication that the configured-root override was
merely "deferred" was wrong; this commit CLOSES the bypass. Four correctness
defects are fixed together.

### Fixes

1. **Production data plane now consumes fail-closed `IndexPaths::resolve`.**
   Deleted `current_root_lenient`/`current_db_lenient`. CLI `codegraph_dir`/
   `db_path` (main.rs) are now `Result`-returning wrappers over a new
   `index_paths()` = `IndexPaths::resolve(project, CODEGRAPH_DIR)`; every
   mutating/opening call site threads `?`. `codegraph-mcp`
   `CodeGraphEngine::open` resolves fail-closed before `Store::open`.
   `codegraph-watch` `default_db_path` returns `Result` and resolves fail-closed.
   Consequently `CODEGRAPH_DIR=.`/`..`/root/overlap/symlink alias errors BEFORE
   any root/DB creation and never mutates the legacy namespace; a configured
   relative or absolute root gets the identity-suffixed sibling
   `<name>-v2-<projectIdentity>` through the REAL CLI and MCP paths, and two
   projects sharing one configured root get distinct roots. New black-box CLI
   regressions in `batch_m_v2_namespace.rs` prove all three via `init` +
   `status --json` on-disk placement.
   - Deliberate non-fail-closed exceptions, both existence-probes that must stay
     infallible and cannot open a wrong DB: CLI `is_initialized` (a `resolve`
     failure ⇒ "not initialized", so discovery keeps walking; the mutating paths
     re-resolve fail-closed before touching disk) and `status` (reports the safe
     `.codegraph-v2` default when a configured root is unresolvable, so status
     still answers). `codegraph-mcp` `roots::db_path_for` is likewise an
     `is_file()` probe: on a `resolve` failure it degrades to the default whose
     file simply won't exist, NEVER a reconstructed configured-root path that
     could shadow another project — the authoritative rejection is in
     `CodeGraphEngine::open`.

2. **Scanner excludes only the EXACT resolved physical root set.** Deleted the
   over-broad `is_reserved_index_dir(name.starts_with(".codegraph-"))`. New
   `IndexPaths::reserved_child_dir_names(project, CODEGRAPH_DIR)` returns the
   exact child-directory names of the resolved fixed/configured legacy roots and
   current root (always including `.codegraph`); `scan_dir` excludes those names
   ONLY as direct children of the project root. A user `.codegraph-sources/`
   directory is now indexed; `.codegraph` and the resolved `.codegraph-v2` are
   excluded. Proven by `scan_keeps_user_codegraph_prefixed_dir_but_excludes_reserved_roots`.

3. **Windows reparse-point detection.** `is_symlink` now also checks the raw
   `FILE_ATTRIBUTE_REPARSE_POINT` attribute bit under `cfg(windows)` (directory
   junctions are not caught by `FileType::is_symlink`). Verified compilable via
   `cargo check -p codegraph-core --target x86_64-pc-windows-msvc --all-targets
--locked` (both windows targets are installed on this host; this is a
   cross-compile check, NOT native Windows runtime validation).

4. **FFI last-error ordering.** In the Windows `GetFileInformationByHandleEx`
   path the query's `std::io::Error::last_os_error()` is now captured BEFORE
   `CloseHandle`, which itself sets the thread-local last-error and would
   otherwise mask the real failure diagnostic.

5. **Removed the `state_slot(usize)` modulo alias.** It documented "clamp" but
   implemented `is_multiple_of(2)` parity, silently aliasing `state_slot(2)` to
   slot 0. It is not needed by this slice; `state_slots() -> [PathBuf; 2]` is the
   exact contract. Dropped the API and its test.

Also fixed a consequential parent-dir bug surfaced by moving init off
`.codegraph`: the detached daemon's `log_target` (parent-side, pre-spawn) now
creates the `.codegraph` rendezvous parent before opening `daemon.log`, since
the current index namespace is the sibling `.codegraph-v2` and `.codegraph` may
not exist yet (the child's lock layer creates it, but too late for the parent's
stdout/stderr redirect). Daemon rendezvous stays under `.codegraph` by design.

### Correction to the prior ledger entry

The previous entry's "Remaining Batch M path consumers" bullet claiming the data
plane "uses `current_root_lenient` (simple join for a configured `CODEGRAPH_DIR`)
in this slice" is SUPERSEDED: that bypass is deleted. The data plane now honors
the configured-root fail-closed contract in full. Still legitimately unchanged
(NOT the DB data plane): daemon rendezvous paths, `Config::discover`,
`ext_config`/`godot_dsl_config` ancestor `codegraph.json` walk, and the
`codegraph-bench` legacy oracle — these remain later Batch M tasks.

### Verification

`bash scripts/check-workspace-versions.sh` (OK, lock `750ee84b…` unchanged);
`cargo check --workspace --all-targets --locked` clean;
`cargo check -p codegraph-core --target x86_64-pc-windows-msvc --all-targets --locked` clean;
`cargo clippy` across all touched crates `-D warnings` clean;
`cargo fmt --all --check` clean; `bash scripts/guardrail.sh` exit 0;
`cargo test --workspace --locked` — 0 failures (incl. the preserved initial
black-box Red, the new configured-root + `.` alias + scanner regressions, and
`codegraph-bench` golden byte-stability). No plan-level architecture change was
required.

## Batch M — path-authority correction #2: close verification defects (2026-07-25)

### Why this follow-up exists

Verification REJECTED the prior correction (commit `f6ff9e2`) on five concrete
defects. This commit closes each; the earlier claims are SUPERSEDED here, never
erased. Commits `b5a66f2`, `8aeefc4`, `f6ff9e2` stay intact; this is a NEW
correction commit (no amend). Frozen plan/manifest/support hashes, `UPSTREAM.md`,
`KNOWN_DIFFS.md`, `.gitignore`, `.oxfmtignore`, and `Cargo.lock` are all
untouched (`Cargo.lock` still `750ee84b…`).

### Rejected defects and their fixes

1. **External normalization checked only the nearest existing node, not every
   existing component.** `physical_normalize`'s out-of-project arm rejected a
   symlink only at `existing` (the nearest-existing ancestor), so an
   intermediate-link-then-existing-child path such as
   `<cache>/link/child/cg` (where `link` is a symlink to a real dir and `child`
   is an ordinary directory reached THROUGH it) would follow the alias silently.
   Fixed: the out-of-project arm now walks EVERY prefix component from the
   filesystem root down to `existing` and rejects the first symlink / Windows
   reparse-point component. The in-project arm already probed the whole tail
   below the (already-canonical) project. Both spellings are now locked by tests
   `rejects_external_configured_root_reached_through_intermediate_symlink` and
   the new `rejects_in_project_root_reached_through_intermediate_symlink`.

2. **Scanner exclusions were lossy / direct-child basename based.** The prior
   `reserved_child_dir_names` reduced roots to basenames and `scan_dir` only
   pruned them when `dir == root`, so a nested configured root
   (`CODEGRAPH_DIR=cache/index` → `<project>/cache/index`) was never excluded at
   its true depth, and a same-basename user dir could be mis-excluded. Fixed:
   `IndexPaths::reserved_index_roots` now returns the EXACT resolved physical
   root PATHS (full `PathBuf`s, re-anchored onto the caller's `project` spelling
   for in-project roots, absolute for external ones); `scan_dir` prunes a
   directory iff its FULL path equals one of them (at any depth), keeping the
   `.git`-at-root rule. New tests: `reserved_roots_include_nested_configured_roots_at_true_depth`,
   `scan_excludes_only_top_level_root_paths_not_same_named_nested_dirs`; the
   default/relative/degrade tests were rewritten to assert full paths.

3. **CLI/MCP fallbacks masked invalid configuration.** `cmd_status` degraded an
   unresolvable/aliased `CODEGRAPH_DIR` to a default `.codegraph-v2` layout and
   reported "not initialized"; `roots::db_path_for` returned a reconstructed
   default path. Fixed: `cmd_status` now propagates the `IndexPaths::resolve`
   error (fail closed) instead of masking it; `roots::db_path_for` returns
   `Option<PathBuf>` (`None` on a resolve failure) with a shared
   `db_exists_for` probe, and `server.rs engine_for` maps `None` to a stable
   error rather than opening a reconstructed path. `is_initialized` stays an
   infallible discovery probe by design (a resolve failure ⇒ "not initialized",
   discovery keeps walking; the mutating/opening paths re-resolve fail-closed).
   New CLI regression `status_fails_closed_on_invalid_configured_root_without_mutation_via_cli`
   proves both text and `--json` status fail closed and mutate zero bytes.

4. **Real MCP configured-root / cross-project tests were missing.** Added a
   dedicated test binary `crates/codegraph-mcp/tests/mcp_configured_root.rs`
   (separate process; serializes the process-global `CODEGRAPH_DIR` via an
   `ENV_LOCK`): `configured_relative_root_mcp_opens_identity_sibling_db` proves a
   real `McpServer` `tools/call` opens the identity-suffixed sibling DB (not the
   simple-join), and `two_projects_sharing_absolute_root_cannot_cross_read_via_mcp`
   proves two projects on ONE absolute configured root get distinct roots and
   cannot cross-read. Added a CLI-surface counterpart
   `two_projects_sharing_escaping_relative_root_get_distinct_roots_via_cli`.

5. **A stale rustdoc link.** `DEFAULT_CURRENT_DIR`'s doc referenced the removed
   `IndexPaths::current_root_lenient`; retargeted to `IndexPaths::resolve`.

Also replaced the previous `db_path_for_returns_none_on_invalid_configured_root`
unit test — which mutated the process-global `CODEGRAPH_DIR` and could race the
crate's env-sensitive readers — with the race-free
`db_path_for_returns_none_on_resolve_failure` (a nonexistent project triggers the
same `resolve` failure without touching the env); the invalid-`CODEGRAPH_DIR`
path is covered end-to-end by the real CLI/MCP black-box regressions. Removed the
now-unused `ENV_LOCK` from `roots.rs`.

### Verification

`bash scripts/check-workspace-versions.sh` (OK, lock `750ee84b…` unchanged);
`cargo fmt --all --check` clean; `cargo check --workspace --all-targets --locked`
clean; `cargo check -p codegraph-core --target x86_64-pc-windows-msvc
--all-targets --locked` clean (cross-compile only, NOT native Windows runtime);
`cargo clippy --workspace --all-targets --locked -- -D warnings` clean;
`cargo test --workspace --locked` — 0 failures across all 106 suites (incl. the
preserved initial black-box Red, the new intermediate-symlink / nested-root /
status-fail-closed / MCP-configured-root regressions, and `codegraph-bench`
golden byte-stability); `bash scripts/guardrail.sh` exit 0. No plan-level
architecture change was required; state-slot / lease / Store `open_for_*` /
uninit lifecycle and Batches A–E remain out of scope.

## Batch M — path-authority correction #3: byte-proof snapshots + reachable invalid-config state (2026-07-25)

### Why this follow-up exists

Verification of correction #2 (commit `7652267`) rejected TWO of its claims as
overclaims. This entry SUPERSEDES those two claims; nothing above is erased, and
commits `b5a66f2`, `8aeefc4`, `f6ff9e2`, `7652267` all stay intact — this is a
NEW correction commit (no amend). Frozen plan/manifest/support hashes,
`UPSTREAM.md`, `KNOWN_DIFFS.md`, goldens, schema, and node-id logic are
untouched; `Cargo.lock` is still `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

### Superseded overclaims

1. **"mutate zero bytes" was not byte proof.** Correction #2's
   `status_fails_closed_on_invalid_configured_root_without_mutation_via_cli`
   compared a `tree_snapshot` of `(relative path, byte LENGTH)` pairs. An
   equal-length in-place write leaves every length identical, so the assertion
   could not distinguish "unchanged" from "same-size content swap": size equality
   is NOT evidence of byte identity. The prior wording claiming the command
   "mutates zero bytes" therefore claimed more than the oracle proved.

2. **The invalid-`CODEGRAPH_DIR` state was still masked on the MCP surface.**
   Correction #2 made `roots::db_path_for` return `None` on a resolve failure and
   routed both MCP front-ends through the `db_exists_for` predicate, which
   collapsed "unsafe configured root" and "valid root, absent DB" into one
   `false`. Both `McpServer::handle_tools_call` and the rmcp
   `CodeGraphHandler::call_tool` consumed that boolean BEFORE
   `CodeGraphEngine::open` could run, so an invalid root emitted the generic
   `No indexed project …, or run codegraph init there.` message and the
   actionable `IndexPaths` diagnostic was unreachable through either front-end.
   Correction #2's claim that the fail-closed error "is surfaced by
   `CodeGraphEngine::open` when a tool actually runs" was therefore false for the
   invalid-config case: the call never reached `open`.

### Fixes

1. **Byte-complete nonmutation oracle.** `tree_snapshot` (CLI
   `batch_m_v2_namespace.rs`) now captures `(relative path, COMPLETE file bytes)`
   using `symlink_metadata` (a symlink is recorded as an entry, never followed).
   The comparison moved into `assert_tree_bytes_unchanged`, which diffs the byte
   maps and reports only the offending paths (created / removed / bytes changed)
   instead of dumping every file's contents into the failure message. New harness
   self-test `tree_snapshot_detects_equal_length_byte_mutation` flips ONE byte
   without changing any length and asserts (via `catch_unwind`) that the
   nonmutation assertion FAILS — proving the oracle now detects exactly the
   mutation class the size-only snapshot missed.

2. **Typed root state, reachable through both front-ends.** `roots.rs` gained
   `RootStatus { Indexed, Absent, Invalid(String) }` and `probe_root`, built on a
   pure `classify_resolve` over the `IndexPaths::resolve` RESULT — the state is
   discriminated by the error VARIANT, never by parsing a rendered string. An
   `IndexPathsError::ProjectInaccessible` stays `Absent` (a bogus `projectPath` is
   a missing project, not a bad configuration); every other variant is
   `Invalid`, carrying the stable diagnostic verbatim. `db_exists_for` is now
   `matches!(probe_root(..), Indexed)`, so adoption and the `tools/list` schema
   selector keep their unchanged boolean semantics.

   `roots::resolve_project_arg` is the single shared resolver both front-ends
   call, returning `ProjectArg { Resolved, InvalidConfig(String), NotIndexed }`.
   Candidate ORDER is preserved exactly (absolute raw → cwd-join → bare raw →
   default-by-basename; `None` raw → default): an INDEXED candidate still wins
   immediately, so a valid configured root resolves normally even when an earlier
   candidate is misconfigured; only when NO candidate is indexed is the FIRST
   invalid diagnostic surfaced (fail closed), and an all-absent candidate set
   stays the genuine `NotIndexed` "run `codegraph init`" case.
   `McpServer::handle_tools_call` and the rmcp `call_tool` both map
   `InvalidConfig` to the shared `roots::invalid_config_message`, which embeds the
   verbatim `IndexPaths` reason plus the remedy. `server.rs` / `rmcp_handler.rs`
   deleted their duplicated candidate loops in favor of the shared resolver, so
   the two front-ends cannot drift.

3. **Real public-surface trap-DB regressions.** `mcp_configured_root.rs` gained
   a panic-safe RAII `EnvGuard` that holds the `ENV_LOCK` and restores the prior
   `CODEGRAPH_DIR` in `Drop` (the previous manual restore lines were skipped on an
   assertion panic, leaking a bad value into every later test in the binary); both
   pre-existing tests were converted to it. Two new tests stage a TRAP copy of the
   golden mini DB at the DEFAULT namespace `<project>/.codegraph-v2/codegraph.db`
   — precisely the path a silent fallback would open — set the refused
   `CODEGRAPH_DIR=.`, and drive a real `codegraph_search` tool call:
   `invalid_configured_root_mcp_fails_closed_and_never_serves_trap_default_db`
   (hand-rolled `McpServer` over stdio) and
   `invalid_configured_root_rmcp_fails_closed_and_never_serves_trap_default_db`
   (the SHIPPED rmcp handler over a duplex transport with a real rmcp client).
   Each asserts `isError == true`; that the text carries the stable reason
   (`CODEGRAPH_DIR` + `project root itself`); that it is NOT the generic
   `No indexed project` message; that NONE of the trap DB's symbols
   (`Counter`, `increment`, `math.ts`) appear in the response; and that the whole
   project tree — trap DB included — is byte-for-byte unchanged.

   Both trap tests were confirmed to be REAL regressions: reverting
   `classify_resolve`'s non-`ProjectInaccessible` arm to `Absent` makes both fail
   with the exact pre-fix `No indexed project found for projectPath …` text, and
   restoring the arm makes them pass.

4. **Race-free unit coverage for the typed states.** `roots.rs` added a
   `resolve_project_arg_with` seam taking the per-candidate classifier as an
   argument, so candidate order and the three states are asserted with a stub
   probe — no filesystem, no `CODEGRAPH_DIR` mutation. New tests:
   `classify_resolve_{valid_present_is_indexed, valid_absent_is_absent,
invalid_config_is_invalid_with_diagnostic, missing_project_is_absent_not_invalid}`,
   `resolve_project_arg_none_invalid_default_is_invalid_config`,
   `resolve_project_arg_indexed_candidate_wins_over_earlier_invalid`,
   `resolve_project_arg_reports_first_invalid_in_candidate_order`,
   `resolve_project_arg_all_absent_is_not_indexed`, and
   `invalid_config_message_carries_detail_and_remedy`.

No existing test was weakened or deleted; the pre-existing resolution unit tests
were adapted to the typed return via `ProjectArg::resolved()`, keeping their
original assertions. No dependency was added. State slots, `IndexLease`, Store
`open_for_*`, migration, uninit/daemon lifecycle, project-scoped `Config`,
extension/Godot config relocation, and Batches A–E all remain out of scope.

### Verification

All commands run in the implementation worktree on
`feat/upstream-v1.5-portable-fixes`, each Cargo batch preceded by
`bash scripts/check-workspace-versions.sh` (OK at 0.40.4 every time):

- `cargo fmt --all --check` — clean.
- `cargo test -p codegraph-rs --test batch_m_v2_namespace --locked` — 7 passed,
  0 failed (incl. the new byte-oracle self-test).
- `cargo test -p codegraph-mcp --test mcp_configured_root --locked` — 4 passed,
  0 failed (incl. both new trap-DB regressions).
- `cargo test -p codegraph-mcp --lib --locked` — 256 passed, 0 failed.
- `cargo check --workspace --all-targets --locked` — clean.
- `cargo check -p codegraph-core --target x86_64-pc-windows-msvc --all-targets
--locked` — clean. CROSS-COMPILATION ONLY; no native Windows runtime coverage
  is claimed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean.
- `cargo test --workspace --locked` — exit 0; 105 suites reporting
  `test result: ok`, 2671 tests passed, 0 failures (incl. the preserved initial
  black-box Red and `codegraph-bench` golden byte-stability).
- `bash scripts/guardrail.sh` — exit 0.
- `sha256sum Cargo.lock` — `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`
  (byte-identical).

Red-proof for the invalid-config fix is recorded in Fix 3 above (temporary revert
⇒ both trap tests fail with the pre-fix generic message; revert restored ⇒ pass).

## Batch M — path-authority correction #4: fail-closed nonmutation oracle + stale-doc cleanup (2026-07-25)

### Why this follow-up exists

Manual review of commit `7d95634` (correction #3) found the byte-nonmutation
oracle it introduced was still not fail-closed, and that correction #3 left two
superseded `Engine::open` claims in the source docs. This is an APPEND-ONLY
correction; `7d95634` and every earlier commit are preserved unamended.

### Superseded overclaims (from correction #3)

1. **"a symlink is recorded as an entry, never followed" — FALSE.** The helper
   stat'd with `symlink_metadata` but then read the payload with
   `fs::read(&path)`, which FOLLOWS the link. A symlink's recorded bytes were the
   pointed-to file's bytes, so an out-of-tree write showed up as a mutation of
   the tree, while a retarget between two same-content files did not.

2. **"whole-tree byte proof" — TOO STRONG.** Four holes could each produce a
   FALSE "unchanged":
   - `let Ok(entries) = read_dir(dir) else { return }` silently dropped an entire
     subtree from BOTH snapshots.
   - `entries.filter_map(Result::ok)` silently dropped individual entries.
   - `fs::read(..).unwrap_or_default()` recorded an unreadable file as EMPTY
     bytes on both sides.
   - Only FILES were recorded, so creating or removing an EMPTY directory was
     invisible.
     Special entry kinds (fifo, socket, device) also fell into the "else" file arm
     and were read as bytes or silently defaulted.

3. **`roots::db_path_for` rustdoc and `McpServer::engine_for` comment** still
   said the authoritative fail-closed error "is surfaced by
   `CodeGraphEngine::open` when a tool actually runs". Correction #3 itself moved
   that rejection EARLIER, into the shared typed resolver, so the sentences
   described behavior that no longer exists.

4. **`mcp_configured_root.rs` module doc** said "Each test drives the REAL
   `McpServer` over an in-memory stdio pipe", but correction #3 added an rmcp
   handler test over a duplex transport.

### Fixes

1. **Typed, exact, fail-closed snapshot entries.** Both oracles (CLI
   `batch_m_v2_namespace.rs` and MCP `mcp_configured_root.rs`) now build
   `Vec<TreeEntry>` where `TreeEntry { rel: PathBuf, kind: EntryKind }` and
   `EntryKind ∈ { Directory, RegularFile(Vec<u8>), Symlink(PathBuf) }`:
   - `rel` is an OS-native `PathBuf`, so the equality key is never a lossy
     `to_string_lossy()` rendering.
   - `Directory` records presence itself, so an EMPTY directory's creation or
     removal is detectable.
   - `RegularFile` carries the COMPLETE bytes (never a length).
   - `Symlink` carries the target from `read_link`; the link is NEVER read
     through, so an out-of-tree write to the target is correctly NOT a mutation
     of this tree, and a retarget IS.

2. **Fail loudly, never skip.** Every I/O step (`read_dir`, each entry,
   `symlink_metadata`, `read_link`, `read`, `strip_prefix`) now panics with a
   path-naming message instead of `return` / `filter_map(Result::ok)` /
   `unwrap_or_default()`. An entry kind with no deterministic exact
   representation panics rather than being omitted.

3. **Bounded failure messages.** `assert_tree_bytes_unchanged` (CLI) and the new
   `assert_tree_unchanged` (MCP) diff the typed maps and report only
   `created/removed/changed: <path> (<kind label>)`, where the label is
   `directory` / `file[N bytes]` / `symlink -> <target>` — never file contents.
   The MCP trap tests' inline diff loop and the rmcp test's bare
   `assert_eq!(before, after)` (which would have dumped the whole golden DB into
   a failure message) both now call that helper.

4. **New oracle self-tests** (CLI), each proving a property mechanically:
   - `tree_snapshot_detects_empty_directory_mutation` — creating an empty
     directory changes the snapshot and the assertion FAILS; removing it restores
     the exact snapshot.
   - `tree_snapshot_detects_symlink_target_mutation_without_following`
     (`#[cfg(unix)]`) — the link is recorded as `Symlink(target)`; a retarget
     FAILS the assertion; a write to the pointed-to file OUTSIDE the tree leaves
     the snapshot identical (proving no follow).
   - `tree_snapshot_fails_loudly_on_unsupported_entry_kind` (`#[cfg(unix)]`) — a
     unix-socket file makes `tree_snapshot` PANIC with "unsupported entry kind".
   - The pre-existing `tree_snapshot_detects_equal_length_byte_mutation` is
     preserved, now asserting the byte length through `EntryKind::RegularFile`.
     The `#[cfg(unix)]` gating keeps portable compilation intact; no Windows
     runtime behavior is claimed.

5. **Stale docs corrected.** `roots::db_path_for`'s rustdoc now says it is for
   path DISPLAY only and that the authoritative diagnostic comes from the typed
   `probe_root` / `resolve_project_arg` states, which reject an invalid configured
   root BEFORE any engine is opened. `McpServer::engine_for`'s comment now says
   the invalid root is already rejected upstream by `roots::resolve_project_arg`
   and that this arm is the defensive backstop for an unresolved DB path.
   `mcp_configured_root.rs`'s module doc now describes both front-ends.

No production behavior changed: fixes 1–4 are test-only and fix 5 is comments
plus rustdoc. No dependency was added, no assertion was weakened, and no
extraction / schema / node-id / golden surface was touched.

### Red proof for the new oracle properties

Each property was proven to be a REAL regression by temporarily reverting the
production arm and observing the failure, then restoring it:

- Removing the `EntryKind::Directory` push (pre-fix file-only walk) ⇒
  `tree_snapshot_detects_empty_directory_mutation` FAILS with
  `assertion left != right failed: creating an EMPTY directory must change the
snapshot (no file changed)`, both sides `[TreeEntry { rel: "a.txt", … }]`.
- Restoring the pre-fix follow-the-link read (`fs::read` in the symlink arm,
  `unwrap_or_default`) ⇒
  `tree_snapshot_detects_symlink_target_mutation_without_following` FAILS with
  `left: [TreeEntry { rel: "link", kind: RegularFile([65, 65, 65, 65]) }]` vs
  `right: [… kind: Symlink("…/outside/a.bin") }]` — the verbatim evidence that
  the old helper read THROUGH the link.
- Both reverts were undone; the full suite is green below.

### Verification

All commands run in the implementation worktree on
`feat/upstream-v1.5-portable-fixes`, each Cargo batch preceded by
`bash scripts/check-workspace-versions.sh` (OK at 0.40.4 every time):

- `cargo fmt --all --check` — clean.
- `cargo test -p codegraph-rs --test batch_m_v2_namespace --locked` — 10 passed,
  0 failed (7 pre-existing + 3 new oracle self-tests).
- `cargo test -p codegraph-mcp --test mcp_configured_root --locked` — 4 passed,
  0 failed.
- `cargo test -p codegraph-mcp --lib --locked` — 256 passed, 0 failed.
- `cargo check --workspace --all-targets --locked` — clean.
- `cargo check -p codegraph-core --target x86_64-pc-windows-msvc --all-targets
--locked` — clean. CROSS-COMPILATION ONLY; no native Windows runtime coverage is
  claimed. (A wider Windows `--all-targets` check across `codegraph-rs` /
  `codegraph-mcp` is not runnable in this environment: their C dependencies need
  the MSVC `lib.exe`, absent on this Linux host.)
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean.
- `cargo test --workspace --locked` — exit 0; 105 suites reporting
  `test result: ok`, 2674 tests passed, 0 failures (2671 before + the 3 new
  oracle self-tests), incl. the preserved initial black-box Red and
  `codegraph-bench` golden byte-stability.
- `bash scripts/guardrail.sh` — exit 0.
- `sha256sum Cargo.lock` — `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`
  (byte-identical before and after).

## Batch M — read-only dual-slot state classifier (2026-07-25)

This slice adds the store-owned READ-ONLY state classifier. Store now owns
storage protocol `2`, extraction version `2`, and metadata key
`indexed_with_extraction_version`. The classifier reads exactly the two
`IndexPaths::state_slots()` regular files, never opens SQLite, and never creates,
truncates, renames, deletes, or rewrites any namespace byte. The same commit
series also routes the CLI's PRE-EXISTING `project_metadata` extraction stamp
through the store-owned key/version and therefore changes that DB stamp from `1`
to `2`; that existing CLI mutation is outside the classifier and means the whole
slice must not be described as mutation-free.

The six protocol-stable JSON fields are required at their exact types; unknown
fields are tolerated. Owner and checksum must each be exactly 64 lowercase hex
characters. The SHA-256 payload uses fixed ASCII labels and LF separators and
hashes the raw phase text. These stable checks, checksum value, and owner equality
are enforced before a future storage protocol is trusted; an unknown future phase
is then accepted, while current protocol accepts only `building`, `current`, and
`uninitialized`. Lower/zero protocols and every present malformed, unreadable,
non-regular, checksum-invalid, or owner-mismatched slot are typed `Corrupt` and
dominate a valid companion.

Aggregation first rejects any invalid slot, then rejects equal sequence across
all validated pairs (current/current, future/future, or mixed) independently of
JSON byte formatting. A validated future-protocol record then dominates a current
record regardless of sequence; otherwise the highest current record wins. The
selected `u64::MAX` sequence is corruption before protocol/extraction status
mapping. Public tests cover the complete matrix plus exact fail-closed filesystem
snapshots (directories, complete file bytes, and unfollowed symlink targets keyed
by `PathBuf`), including equal-length mutation and Unix symlink self-tests.

Behavioral Red was established by temporarily weakening mixed equal-sequence
rejection: `index_state_equal_future_and_mixed_sequences_are_corrupt_before_future_dominance`
failed with `expected Corrupt, got Future { built: 9 }`; the production condition
was restored before verification.

**Strict scope boundary:** atomic publication, `IndexLease`, Store `open_for_*`
APIs, DB stamping/corroboration, migration/finalization, uninit and daemon/watcher
lifecycle remain unimplemented. This slice exposes no publication or lease API.

## Batch M — read-only classifier correction: byte proof + replacement detection (2026-07-25)

Verification of commit `4d5a39c` found three focused defects; that commit remains
intact and this correction is additive:

1. The fail-closed tree snapshot wrapped only one ordinary `Current` call, so it
   did not mechanically prove nonmutation on malformed-plus-valid,
   checksum/owner corruption, either future/current sequence direction, all four
   equal-sequence classes, current/future maximum sequence, or current
   `uninitialized`. The integration tests now use one
   generic `classify_unchanged` helper that snapshots immediately before and
   after every named `classify`/`classify_slots` call. The single oracle still
   records directories, complete regular
   file bytes, and unfollowed symlink targets under native `PathBuf` keys; every
   I/O/unsupported kind panics, and changed-path output is bounded to 16 paths.
   Its equal-length mutation and Unix non-following self-tests remain green.
2. `read_slot` previously paired `symlink_metadata(path)` with `fs::read(path)`.
   A fixed slot replaced after the no-follow stat could therefore be consumed
   through a symlink or as a different file. It now opens one `File`, verifies
   the opened handle is regular, reads only from that handle, and corroborates
   pre-open/handle/post-read identity. A static symlink remains
   `NotARegularFile`; disappearance, type change, or identity mismatch is the new
   typed `SlotChangedDuringRead` corruption and is never accepted. Unix compares
   `MetadataExt::{dev, ino}`; Windows uses std's volume serial plus file index.
   Other targets conservatively compare available metadata attributes and do NOT
   claim stable-identity exclusion. This is pre/open/post corroboration, not a
   claim that portable std provides an atomic no-follow open on every OS.
3. The prior ledger said the slice "adds only the classifier". That was too broad:
   the classifier itself is read-only, but `main.rs` also moved the existing CLI
   metadata key/version source into `codegraph-store` and now stamps extraction
   version `2`. There is still no state-slot publication, `IndexLease`, Store
   open/stamp API, migration/finalizer, or uninit/daemon/watch lifecycle here.

Deterministic Red evidence for defect 2: the private read-checkpoint test replaces
the regular slot at `InitialMetadataValidated`, with no sleeps. Temporarily
making Unix identity comparison always return true caused
`regular_slot_replaced_after_validation_is_typed_corruption` to fail its expected
`SlotChangedDuringRead` assertion; restoring `dev`/`ino` comparison made it pass.
The targeted command
`cargo test -p codegraph-store --locked index_state -- --nocapture` (after the
workspace-version gate) reports **1 internal + 20 integration tests passed, 0
failed**. Full final gate outputs are recorded below after completion.

Final validation evidence:

- `cargo fmt --all --check`, workspace/all-target `cargo check --locked`, the
  required `codegraph-core` MSVC cross-check, and workspace/all-target Clippy with
  `-D warnings` all passed. The MSVC result is compilation only; no native Windows
  runtime coverage is claimed.
- Standalone `cargo test --workspace --locked` passed **2695 tests across 106
  reporting suites, 0 failed**. `bash scripts/guardrail.sh` passed. Every Cargo
  batch was preceded by `bash scripts/check-workspace-versions.sh`.
- `make ci CARGO='cargo --locked'` was attempted twice. Both runs passed format
  and Clippy, then hit the same pre-existing timing-sensitive
  `daemon_single_watcher_fires_once` failure (`watcher sync #1: 0 file(s)
reindexed` instead of naming `extra.ts`). The standalone full workspace run in
  the same validation batch passed that test and all 2695 tests. Per the two-check
  ceiling, it was not retried a third time; no unrelated watcher code was changed.
- LSP diagnostics refused the external `/tmp` worktree because it is outside the
  tool request cwd. Workspace `cargo check` and Clippy are the recorded fallback.
- `Cargo.lock` remained byte-identical at
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

**Correction to the validation evidence above (post-`224e762` verification).**
Independent verification of commit `224e762` reran the targeted classifier command
and reproduced **1 internal + 20 integration tests passed, 0 failed**. It then ran
the full gate chain, and the `make ci` invocation failed _before_ tests at the
`oxfmt --check` doc-format step, because the validation evidence in this ledger had
been appended _after_ the last formatting pass and was therefore not
formatter-clean. The two `daemon_single_watcher_fires_once` runs recorded in the
bullet above are preserved as accurate _historical_ attempts, but they were not the
last gate: at commit `224e762` the gate stopped earlier, at doc formatting. The
formatting was corrected in this commit and a fresh `make ci` was then run; that
fresh post-format run is the authoritative final gate and its result is recorded
immediately below.

Authoritative final gate (fresh run, after doc formatting):

- `bash scripts/check-workspace-versions.sh` passed before every Cargo-invoking
  command, and `oxfmt --check` is clean on this ledger.
- `make ci` (fmt-check + Clippy `-D warnings` + `cargo test --workspace` +
  `bash scripts/guardrail.sh`) **passed, exit 0** — every reporting suite ended
  `0 failed` and the guardrail exited 0. Because no test in this workspace is
  ignored by the gate, that exit-0 necessarily includes
  `daemon_single_watcher_fires_once`, which therefore passed in this run rather
  than flaking as it did in the two historical attempts above.
- `Cargo.lock` still byte-identical at
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`; this
  correction changes documentation only.

## Batch M — reusable `IndexLease` capability (2026-07-25)

### Scope

This slice adds only `codegraph-store`'s reusable v2 lease capability and its
behavioral tests. It does not wire leases into `Store`, SQLite, CLI, MCP, watch,
daemon, state publication, migration/finalization, or uninit. The capability is
derived only from resolved `IndexPaths::permanent_lock()` and the physical
`current_db()` parent.

### Behavioral Red

The API-level refinement tests were added against minimal compiling scaffolding
whose acquisition methods opened the lock file but deliberately did not call the
kernel lock APIs. This preserved the earlier Batch M black-box Red distinction
while producing a real lease-behavior Red rather than a compile/setup failure.

- Gate: `bash scripts/check-workspace-versions.sh` → exit 0.
- Command: `cargo test -p codegraph-store --test index_lease --locked` → exit
  101; 4 passed, 4 failed.
- Exact missing behavior: all incompatible child-process probes returned
  `ACQUIRED` instead of `TIMED_OUT`; an in-process exclusive holder likewise did
  not force timeout. The failing tests were
  `shared_processes_coexist_but_shared_blocks_exclusive`,
  `exclusive_process_blocks_shared_and_exclusive`,
  `a_clone_keeps_the_single_lock_alive_until_the_final_drop`, and
  `timeout_and_post_contention_cancellation_preserve_lock_bytes`.
- The child holder printed `READY` only after acquisition and the parent waited
  through a channel before launching a contender. Release used a stdin control
  byte. Thus contention order did not depend on a sleep.

### Green behavior

- `IndexLease` keeps private `LeaseMode::{Shared, Exclusive}` plus the normalized
  v2 DB-parent `PathBuf`; callers can observe mode/match and validate an exact
  exclusive capability but cannot construct or mutate either identity.
- Existing-open APIs use `OpenOptions` without `create` and therefore cannot
  create an absent root/lock. The separate `create_exclusive` entry point is only
  for a genuinely absent root, opens the permanent lock with
  `create(true).truncate(false)`, and rejects an existing namespace rather than
  repairing it.
- Acquisition uses Rust 1.96 `File::try_lock_shared` / `File::try_lock` in a
  monotonic-deadline loop, checking cancellation before open, before every
  attempt, and immediately after contention. It never calls blocking `lock()`.
- One locked `File` lives in `Arc<LeaseInner>`. Clone drops cannot unlock; only
  final `LeaseInner::drop` calls `unlock`, then the `File` closes exactly once.
- Typed validation rejects `SharedLease` and `WrongDbParent`. Typed acquisition
  errors distinguish missing lock, non-absent initial namespace, open/create I/O,
  timeout, cancellation, and other kernel-lock errors.
- The lock file is permanent protocol state only: no PID is written/read, no
  stale-file deletion occurs, and tests preserve sentinel bytes through
  successful opens, contention timeout, cancellation, and validation.

Targeted Green: after implementing kernel acquisition,
`cargo test -p codegraph-store --test index_lease --locked` passed all tests. The
final required check/MSVC/clippy/CI results are recorded below after the last
documentation formatting pass. LSP diagnostics could not attach to this external
`/tmp` worktree (`file path must be inside request cwd`); Cargo diagnostics are
the accepted fallback. Native Windows runtime is unavailable, so only an MSVC
cross-compilation result is claimed.

The first store-wide MSVC check exposed a defect in the previously accepted
state reader: Rust 1.96 still marks
`MetadataExt::{volume_serial_number,file_index}` unstable. This task replaces
those calls with stable metadata corroboration plus a stronger Windows-only final
proof: a second live handle for the fixed slot path is compared with the opened
read handle through raw kernel32
`GetFileInformationByHandleEx(FileIdInfo)`. The byte read still uses exactly one
opened handle; the second handle only corroborates that the fixed path still
names that object. This adds no dependency and does not alter classifier ordering
or state semantics.

### Verification before the terminal CI gate

Every Cargo batch below was preceded by
`bash scripts/check-workspace-versions.sh` (exit 0, workspace version `0.40.4`):

- `cargo test -p codegraph-store --test index_lease --locked` — 9 passed, 0
  failed, including process-level shared/exclusive contention, timeout,
  post-contention cancellation, clone/final-drop ownership, nontruncation, and
  capability validation.
- `cargo test -p codegraph-store --test index_state --locked` — 20 passed, 0
  failed after the Windows identity compatibility correction.
- `cargo check -p codegraph-store --all-targets --locked` — clean.
- `LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/tmp/opencode cargo check -p
codegraph-store --target x86_64-pc-windows-msvc --all-targets --locked` —
  clean. The environment variables make `libsqlite3-sys` emit external link
  metadata instead of requiring unavailable Linux-host `lib.exe`; `cargo check`
  does not link. This proves the store's Rust MSVC target compiles, not that the
  lock behavior ran natively on Windows.
- `cargo clippy -p codegraph-store --all-targets --locked -- -D warnings` —
  clean.
- `make fmt-check` — Rust and the repository-scoped oxfmt set are clean.
- `Cargo.lock` remains byte-identical at
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

The terminal `make ci` is run only after these final ledger bytes are formatted;
its exact result is reported with the resulting commit so the evidence does not
create the stale-post-validation formatting trap.

## Batch M — `IndexLease` lock-identity correction (2026-07-25)

### Rejected boundary and behavioral Red

Manual review rejected commit `b772e0f519ce4ebc731e141f846ea321e5078789`:
`acquire_existing` opened `<current-root>/index.lock` directly, so Unix followed
a static symlink and the lease never proved that the fixed path still named the
opened and locked file before returning authority. Initial creation also used
`create(true)`, allowing a competing entry installed after root creation to be
reopened and accepted.

The correction began with compiling Linux behavioral tests against that code.
`cargo test -p codegraph-store --test index_lease --locked` exited 101: the
static-symlink test demonstrated that the old lease kernel-locked the external
target, while the directory test did not receive the required typed
`NonRegularLock` rejection. This was behavioral failure after compilation, not a
setup or API failure. The deterministic private-checkpoint tests were added with
the minimum test seam: they replace `index.lock` immediately after initial
validation and immediately after kernel acquisition, without sleeps.

### Correction

- A private `file_identity` module now supplies the lease and state reader with
  one cross-platform identity implementation. Unix identity is exact
  `(st_dev, st_ino)`. Windows rejects `FILE_ATTRIBUTE_REPARSE_POINT` and obtains
  exact `(volume serial, 128-bit file ID)` values from live handles through
  stable raw `GetFileInformationByHandleEx(FileIdInfo)` FFI; it does not use the
  unstable Rust metadata file-index accessors. Other targets retain the existing
  conservative metadata fallback without claiming the Unix/Windows guarantee.
- Existing acquisition performs no-follow metadata first, rejects a symlink /
  reparse point as typed `AliasedLock` and a directory/socket/other entry as
  typed `NonRegularLock`, opens the lock once for authority, compares the initial
  identity with that opened handle, acquires only through `try_lock*`, then
  re-reads no-follow metadata and corroborates the final fixed path against the
  same locked handle. Any disappearance, type change, alias, or exact identity
  drift returns typed `LockChangedDuringAcquisition`; no `IndexLease` is built.
- A post-lock rejection drops the local handle. The deterministic replacement
  test then locks the displaced original and acquires the final fixed lock with a
  fresh contender, proving that neither the failed capability nor its kernel
  lock leaked.
- Explicit initial creation still requires a genuinely absent root, but now
  creates `index.lock` with `create_new(true)`. A regular or symlink entry that
  wins after root creation returns typed `LockCreationConflict`; it is never
  followed, reopened, repaired, or locked. The external symlink target's complete
  bytes remain identical and an independent handle can lock it.
- Existing lease semantics remain unchanged: private mode and exact DB-parent
  identity, `Arc<LeaseInner>` final-owner unlock, monotonic bounded deadlines,
  cancellation checks, nonblocking `try_lock*`, nontruncating existing opens,
  and no PID, stale-file deletion, or lock repair protocol.

The identity helper also replaces the state reader's duplicated platform code;
classifier order and public semantics are unchanged. No dependency, schema,
golden, node ID, version, SQLite/Store integration, publication, migration,
uninit, daemon/watch/MCP/CLI lifecycle, `UPSTREAM.md`, or `KNOWN_DIFFS.md` changed.

### Verification before the terminal CI gate

Every Cargo batch was preceded by `bash scripts/check-workspace-versions.sh`
(exit 0, workspace version `0.40.4`):

- Corrected targeted tests: 12 `index_lease` integration tests, four private
  deterministic lease tests, and 20 `index_state` tests passed with zero failures.
- `cargo check -p codegraph-store --all-targets --locked` passed.
- `cargo clippy -p codegraph-store --all-targets --locked -- -D warnings` passed.
- `LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/tmp/opencode cargo check -p
codegraph-store --target x86_64-pc-windows-msvc --all-targets --locked` passed.
  This is Linux-host cross-compilation evidence only; no native Windows runtime
  behavior is claimed.
- LSP diagnostics could not attach to the external `/tmp` worktree (`file path
must be inside request cwd`), so the clean Cargo check/Clippy/test diagnostics
  are the documented fallback.
- `Cargo.lock` remains required to match
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

Per the documented format trap, repository formatting and the terminal
`make ci` run occur only after this final ledger byte. The resulting commit is
created only if that post-format gate exits zero.

The first post-format `make ci` attempt reached the workspace tests after clean
format and Clippy gates, then failed only at the repository's previously recorded
`daemon_single_watcher_fires_once` timing assertion: watcher sync #1 reported
`0 file(s) reindexed` instead of naming the changed file. No lease/store test
failed. This historical attempt is retained honestly; after formatting this
append, one fresh terminal retry is the authoritative final gate.

### Correction (2026-07-25): authoritative final gate for the identity correction

The retry promised above did happen and it passed, so this section closes the
terminal-gate evidence for commit `8c19bce70cf8907f356ba0bb45e1188df00b00c9`.
The failed first attempt recorded directly above is kept verbatim as history: it
was a timing flake in an unrelated watcher test, not a lease or store
regression, and deleting it would destroy that distinction.

The parent then reran the whole gate independently on that exact HEAD, and it
passed. The reproduced focused evidence was:

- `index_lease` integration: 12 passed, 0 failed.
- Private deterministic lease unit tests: 4 passed, 0 failed.
- `index_state`: 20 passed, 0 failed.
- `cargo check -p codegraph-store --all-targets --locked`: clean.
- `cargo clippy -p codegraph-store --all-targets --locked -- -D warnings`: clean.
- `x86_64-pc-windows-msvc` all-target check: clean. This is Linux-host
  cross-compilation evidence only; no native Windows runtime behavior is
  claimed anywhere in this Batch M work.

The final parent command chain was
`bash scripts/check-workspace-versions.sh && make ci && sha256sum Cargo.lock &&
git status --short && git diff --check`, and it exited 0. That `make ci` run
covered `fmt-check`, `clippy -D warnings`, the full `cargo test --workspace`
suite, and `bash scripts/guardrail.sh`. `daemon_single_watcher_fires_once`
carries no `#[ignore]`, so an exit-0 workspace run necessarily includes it: it
passed in this authoritative run. `Cargo.lock` still hashes to
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, and the
worktree was clean with no whitespace errors.

## Batch M — lease-gated atomic state publisher (2026-07-25)

### Scope and behavioral Red

This slice adds only the reusable publisher for the two fixed state slots. It
does not integrate publication into `Store`, SQLite opens/stamps, CLI, MCP,
watch, daemon, rebuild/finalizer, migration, or uninit flows.

The compiling behavioral Red used a minimal public publisher scaffold plus the
first-publication test. After the workspace-version gate passed, the command
`cargo test -p codegraph-store --test index_state_publisher --locked --
--nocapture` compiled and exited 101. Its sole test reached the behavioral call
and failed with `initial state publication must succeed: Refused { status:
Missing }`; result: 0 passed, 1 failed. This was missing publication behavior,
not a compile, fixture, setup, or network failure.

### Accepted publisher behavior

- `publish_index_state` requires the exact `IndexLease` capability and calls
  `validate_exclusive(paths)` before classification or mutation. Shared and
  wrong-parent leases return typed validation errors; full-tree fail-closed
  snapshots prove every directory, regular-file byte, and symlink target stays
  unchanged.
- The publisher immediately consumes the accepted `classify(paths)` result under
  that lease. `Future` and every `Corrupt` case (malformed fixed slot, owner
  mismatch, equal sequence, and `u64::MAX` exhaustion included) return typed
  `Refused` before orphan cleanup or temp creation. It does not reimplement or
  weaken classifier precedence.
- A genuinely slot-absent namespace writes sequence 0 to fixed slot 0. Later
  publications use checked `sequence + 1` and only the opposite, older or
  missing inactive slot. The authoritative slot's complete bytes are preserved.
  A typed `WireState` serializes the exact field order
  `sequence,storageProtocol,extractionVersion,phase,projectIdentity,checksum`;
  the first-publication test asserts the complete canonical JSON bytes and
  checksum.
- Temps are same-directory, publisher-owned names and are opened with bounded
  `create_new(true)` retries. The publisher writes all bytes, flushes, calls
  `sync_all`, removes only an older regular inactive slot when present, renames
  the valid temp into that slot, and attempts parent-directory `sync_all`.
  Explicit OS error sets report unsupported directory sync as
  `ParentSyncStatus::Unsupported`; other failures remain typed I/O errors.
- Orphan cleanup starts only after exact exclusive-lease validation and an
  acceptable under-lease classification. It recognizes only the strict v2
  publisher name grammar, validates regular-file identity before removal, and
  leaves static aliases, non-regular entries, and unrelated names untouched.
  Classifier scanning still reads only `IndexPaths::state_slots()`.

### Deterministic fault and protocol matrix

The private fault seam is inaccessible to normal callers. It injects after
create, full write, flush, file `sync_all`, inactive delete/prepare, rename, and
parent-sync attempt. The matrix covers all 3×3 prior/new phase pairs and all
seven checkpoints (63 successor interruptions), plus all three initial phases at
all seven checkpoints (21 initial interruptions). Before rename, classification
is exactly the old authority (or `Missing` initially); after rename it is exactly
the fully checksummed successor. The old authoritative slot's complete bytes
remain unchanged at every successor checkpoint. This includes the explicit
`building -> uninitialized` interrupted-uninit path. A separate deterministic
checkpoint covers orphan-temp removal.

Public tests additionally cover monotonic
`building -> current -> uninitialized`, both-slot older replacement, missing
inactive-slot recreation, malformed inactive refusal, equal-sequence refusal,
owner mismatch, sequence exhaustion, strict temp names, regular orphan cleanup,
and Unix temp-symlink non-following. A v3 future-protocol slot at a higher
sequence than its v2 current companion still dominates and is byte-nonmutating;
the v2 publisher cannot author arbitrary future records.

### Green and platform evidence before the terminal gate

Every Cargo batch was preceded by `bash scripts/check-workspace-versions.sh`:

- Publisher integration tests: 9 passed, 0 failed.
- Private deterministic publisher tests: 4 passed, 0 failed (including the 84
  phase/checkpoint interruptions described above).
- Accepted classifier tests: 20 passed, 0 failed; accepted lease integration
  tests: 12 passed, 0 failed.
- `cargo check -p codegraph-store --all-targets --locked` passed.
- `cargo clippy -p codegraph-store --all-targets --locked -- -D warnings`
  passed after correcting two lint-only findings.
- `LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/tmp/opencode cargo check -p
codegraph-store --target x86_64-pc-windows-msvc --all-targets --locked`
  passed. This is Linux-host MSVC compilation only; native Windows runtime/crash
  behavior is not claimed.
- LSP diagnostics again refused this external `/tmp` worktree as outside the
  request cwd. The targeted tests, package check, Clippy, and MSVC compilation
  above are the diagnostics fallback.
- `Cargo.lock` remained required at
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

These are the final documentation bytes for this publisher slice. Per the
stale-documentation trap, they are formatted first; the authoritative terminal
gate is then exactly `bash scripts/check-workspace-versions.sh && make ci &&
sha256sum Cargo.lock && git diff --check`, with `CARGO='cargo --locked'` supplied
to `make ci`. The scoped commit is created only if that post-documentation
command chain exits 0, so no later evidence edit can invalidate it.

### First terminal-gate attempt

The first post-format terminal chain reached the full workspace tests after
clean workspace-version, formatting, and Clippy gates, then failed only in the
known timing-sensitive `daemon_single_watcher_fires_once` test: its first sync
reported `0 file(s) reindexed` instead of naming the changed file. No publisher,
classifier, or lease test failed. This failed attempt remains recorded; exactly
one post-append, post-format retry remains under the two-check ceiling.

## Correction (2026-07-25): rejected publisher protocol defects

The publisher slice above was rejected by manual review despite its focused
tests passing. This append supersedes, but does not erase, the earlier 3×3
transition and orphan-cleanup claims. The correction remains publisher/lease
only: it does not begin `Store`, SQLite, CLI, MCP, watch, daemon,
rebuild/finalizer, migration, uninit, or Batches A–E integration.

### Corrected protocol boundaries

- One pure transition validator now admits exactly `Missing -> Building`,
  `Outdated -> Building`, `Uninitialized -> Building`,
  `Current -> Building|Uninitialized`, and
  `Building -> Building|Current|Uninitialized`. Every other current-protocol
  status/phase pair returns typed `StatePublishError::InvalidTransition` with a
  cloned `ExtractionStatus` and requested `StatePhase`, before temp creation or
  fixed-slot mutation. A unit matrix covers all three requested phases for
  `Missing`, `Outdated`, `Uninitialized`, `Current`, `Building`, `Future`, and
  `Corrupt`. `Future` and `Corrupt` still take the earlier typed `Refused` path,
  preserving accepted classifier precedence. Initial slot 0 / sequence 0
  publication is therefore possible only for `Missing -> Building`.
- The successor fault matrix is no longer the unsafe 3×3 cross-product. It
  covers every allowed successor transition at the seven retained checkpoints:
  temp create, full write, flush, file sync, inactive-slot preparation, rename,
  and parent-sync attempt. `Building -> Uninitialized` remains explicit. The
  initial matrix covers only `Missing -> Building`. Successor slot and sequence
  continue to derive solely from the accepted classifier authority, and every
  pre-rename fault preserves the old authoritative bytes.
- `IndexLease` now privately retains the fixed permanent-lock path. Writer
  validation order is mode, exact DB parent, then
  `file_identity::path_still_names_file`: the fixed path must still name the
  exact held handle immediately before publisher classification. Replacement,
  disappearance, aliasing, non-regular replacement, or identity-check I/O
  failure maps to typed `PermanentLockChanged { path }`, without exposing
  platform identities. The public replacement regression snapshots the full
  tree around rejection and then proves a fresh lease can acquire the new fixed
  lock; missing, directory, and Unix alias regressions pin the same pre-mutation
  failure. Shared and wrong-parent precedence remains unchanged.
- Automatic orphan-temp cleanup is removed completely, including its name
  recognizer, directory scan, path unlink, and removal fault checkpoint. The
  prior verify-handle-then-`remove_file(path)` design had an unavoidable
  verify-to-unlink race: portable `std` supplies no atomic conditional unlink
  that removes a path only if it still identifies the verified handle. Keeping
  that cleanup could delete a replacement object, so the safe portable behavior
  is to leave every preexisting temp-like entry untouched. Generated temp names
  remain internal, bounded, collision-safe through `create_new(true)`, and
  outside fixed-slot scanning. Successful-publication tests preserve a regular
  orphan-like file, a directory and marker, an unrelated temp-like file, and on
  Unix a symlink plus its external target bytes.

### Focused verification before the final terminal gate

Every Cargo batch below was preceded by
`bash scripts/check-workspace-versions.sh`, and every Cargo command used
`--locked`:

- Publisher integration: 14 passed, 0 failed; store unit tests: 66 passed, 0
  failed, including the exhaustive transition validator and both fault
  matrices.
- Lease integration: 12 passed, 0 failed; classifier integration: 20 passed, 0
  failed.
- `cargo check -p codegraph-store --all-targets --locked`: clean.
- `cargo clippy -p codegraph-store --all-targets --locked -- -D warnings`:
  clean.
- `LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/tmp/opencode cargo check -p
codegraph-store --target x86_64-pc-windows-msvc --all-targets --locked`:
  clean. This is Linux-host cross-compilation only, not native Windows runtime
  or crash evidence.
- LSP again rejected the external `/tmp` worktree as outside its request cwd;
  the clean targeted Cargo diagnostics are the accepted fallback.

Two earlier terminal failures remain recorded honestly. The first publisher
gate failed in the unrelated timing-sensitive
`daemon_single_watcher_fires_once` test (`0 file(s) reindexed`). A later parent
retry failed in the unrelated MCP tools-list expectation: expected 4 tools but
observed 2. Neither failure is represented as Green or weakened. After these
final ledger/notepad bytes, the mandatory closing order is `make fmt`, then one
fresh `bash scripts/check-workspace-versions.sh && make ci`, then the exact
`Cargo.lock` SHA-256 check and scoped status/diff inspection. No final Green is
claimed in this entry before that post-documentation gate actually passes.

## Batch M — lease-retaining state-gated `Store` opens (2026-07-25)

### Scope and behavioral Red

This isolated `codegraph-store` slice adds
`Store::{extraction_status,open_for_read,open_for_status,open_for_write,stamp_extraction_version}`
and their typed public result/error surfaces. It preserves legacy `Store::open`
unchanged and does not migrate production CLI, MCP, watch, daemon, index,
rebuild/finalizer, or uninit call sites.

The compiling behavioral Red called the proposed `open_for_read` through a
minimal fallback to legacy `Store::open`. The targeted test reached the public
call, exited 101, and failed the exact assertion `Missing state must reject a
safe read open`: legacy fallback returned success and created SQLite in a
Missing namespace. This was missing state-gated behavior, not a compile, setup,
fixture, or network failure.

### Accepted behavior

- `extraction_status` delegates only to the accepted dual-slot classifier and
  never opens SQLite or mutates namespace bytes.
- `open_for_read` acquires and retains a bounded shared `IndexLease`, accepts
  only `Current`, rejects a tombstone, and corroborates the store-owned
  extraction stamp without creating SQLite or `-wal`/`-shm` artifacts. It opens
  the real DB with exactly `SQLITE_OPEN_READ_ONLY`, retains that source
  connection, copies the checkpointed main-file bytes into a separate read-only
  deserialized SQLite image, and queries only the private image. WAL header
  bytes 18/19 are normalized only in that private allocation; disk bytes remain
  exact. `SQLITE_DESERIALIZE_FREEONCLOSE` ownership follows SQLite's documented
  rule: SQLite frees the allocation on both successful close and failed
  `sqlite3_deserialize`, so the Rust failure path does not double-free it.
- `open_for_status` treats shared-lease timeout as typed status data
  (`rebuilding=true`, no SQLite open), reports stable non-Current states without
  SQLite, and uses the same retained read-only corroboration for `Current`.
- `open_for_write` validates the injected exact exclusive lease before
  classification. `Future` and `Corrupt` fail closed; `Missing` plus any DB,
  WAL, or SHM artifact is typed corruption at the Store boundary. Only
  `CurrentMutation + Current` opens SQLite read-write after read-only stamp
  corroboration. `FullRebuild` and valid `UninitContinuation` return opaque
  authorizations that retain the exclusive lease without opening SQLite;
  unauthorized purpose/state pairs are typed rejections.
- `stamp_extraction_version` is available only on a state-gated write Store and
  revalidates the retained fixed lock immediately before metadata mutation.
  Legacy/read/status Stores cannot stamp. `Store` declares SQLite connections
  before its lease field so both connections drop before the final lease owner
  unlocks.
- Stamp failures distinguish missing, noncanonical/malformed decimal, and exact
  version mismatch. Public APIs export only the purpose/status observations
  required by later lifecycle slices; callers cannot forge a lease-backed
  authorization.

### Focused verification before the terminal gate

Every Cargo command used `--locked` and the batch was preceded by
`bash scripts/check-workspace-versions.sh`:

- `store_state_gates`: 17 passed, 0 failed, covering read/status/write state
  matrices, sidecar-free byte snapshots, stamp failures, timeout status,
  cross-process lease retention/drop, wrong/shared/replaced leases, tombstones,
  Missing-plus-artifact refusal, and write-purpose authorization.
- Accepted publisher integration: 14 passed; lease integration: 12 passed;
  classifier integration: 20 passed; store unit tests: 66 passed; all with zero
  failures.
- `cargo check -p codegraph-store --all-targets --locked` and
  `cargo clippy -p codegraph-store --all-targets --locked -- -D warnings` passed.
- `LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/tmp/opencode cargo check -p
codegraph-store --all-targets --target x86_64-pc-windows-msvc --locked`
  passed. This is Linux-host MSVC compilation only; no native Windows runtime
  behavior is claimed.
- LSP diagnostics cannot attach to this external `/tmp` worktree, so the clean
  targeted Cargo diagnostics are the recorded fallback.

## Correction (2026-07-25): close reviewed Store state/artifact gates

Independent review rejected the Store slice above on contradictions with frozen
Revision 14. This append supersedes only those claims; it does not rewrite the
earlier evidence and still does not migrate CLI, MCP, watch, daemon, index,
rebuild/finalizer, or uninit production callers.

The corrected Store boundary now applies one closed state/artifact contract.
`Missing` plus any DB, WAL, or SHM artifact is a typed, full-tree-byte-nonmutating
failure for read, status, and write, whether the permanent lock exists or not.
A persisted Current, Building, Outdated, or Uninitialized state without the
permanent lock reports `StateWithoutPermanentLock`; Future/slot-Corrupt dominance
remains unchanged. `extraction_status` remains the same slot-only nonmutating
classifier.

`FullRebuild + Current` now receives the same DB/tombstone/extraction-stamp
corroboration as Current reads before an opaque authorization is returned. The
ordinary Current writer revalidates its exact fixed lock path and held handle a
second time at the final deterministic checkpoint immediately before the first
read-write SQLite open. A private test checkpoint replaces the lock at that exact
point; no sleeps infer the race.

Executable WAL evidence used a state-gated Current writer in a child process,
disabled autocheckpoint, committed a unique metadata row, then exited without
running Rust/SQLite destructors. SQLite's real read-only pager observed the row
from the retained WAL, while a rollback-header copy of only the main DB did not.
Therefore a public Current namespace could previously be protocol-classified
while the private deserialized main image silently ignored committed data. The
corrected read/status/Current-corroboration path fails closed on any WAL or SHM
sidecar and preserves the complete namespace tree byte-for-byte; a finalized
Current namespace is pinned as sidecar-free.

Finally, both `u64` and `i64` deserialize length conversions now occur before
`sqlite3_malloc64`. Once the ownership-bearing allocation succeeds, no fallible
Rust conversion can return before `sqlite3_deserialize` accepts
`SQLITE_DESERIALIZE_FREEONCLOSE`; the existing no-double-free failure rule is
unchanged.

Behavioral Red was captured from the landed implementation: Current full rebuild
accepted a missing DB; status returned clean Missing for DB residue; lockless
persisted states surfaced the old lock error; Current reads accepted committed
WAL and returned a private image that omitted its unique row; and deterministic
late lock replacement still opened write-capable SQLite. After the minimal
implementation, `store_state_gates` passes 21/21. The remaining required package,
MSVC cross-compile, and final locked repository gates are recorded only after they
run below; LSP still rejects this external `/tmp` worktree, so Cargo diagnostics
remain the fallback.

Focused implementation gates then passed in the prescribed order, with
`bash scripts/check-workspace-versions.sh` before every Cargo batch and `--locked`
on every dependency-resolving command: Store gates 21/21, accepted publisher
14/14, lease 12/12, classifier 20/20, store lib 68/68, package all-target check,
package Clippy with `-D warnings`, and the Linux-host
`x86_64-pc-windows-msvc` all-target cross-check. The MSVC result is compilation
only, not native Windows runtime/crash evidence. These are the final evidence
bytes before `make fmt`; the authoritative final sequence is format, workspace
version gate, locked `make ci`, lock hash, `git diff --check`, and scoped status.

No dependency, schema, node-id formula, extraction output, or golden artifact
changed. `Cargo.lock` remains required at
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.
These evidence bytes are written before formatting and the single final CI run;
no terminal Green is claimed until that post-documentation gate passes.

## Batch M — `full_sync_finalizer_publishes_current_last` (2026-07-25)

Frozen plan lines 548-556 and test 8 (line 746) are now implemented: a destructive
v2 rebuild becomes a readable `Current` namespace only after every SQLite
finalization step succeeded under the ONE retained exclusive `IndexLease`.

New `codegraph-store::rebuild` owns the whole rebuild lifecycle.
`begin_full_rebuild(paths, kind, deadline, cancelled)` acquires the single outer
exclusive lease (`create_exclusive` for a genuinely absent root,
`acquire_exclusive_existing` otherwise — an existing lockless root is never
repaired), refuses an interrupted-uninit namespace for anything but an explicit
`init`, classifies through `Store::open_for_write(FullRebuild)` while retaining
that authorization for the rebuild's whole lifetime, publishes `phase=building`
BEFORE any destructive work, removes only v2 `codegraph.db`/`-wal`/`-shm`, and
returns a `FullRebuild` handle. `FullRebuild::open_store` opens the fresh
write-capable target through the new `Store::open_rebuild_target`, which
revalidates `IndexLease::validate_exclusive` immediately before the first
write-capable SQLite open.

`FullRebuild::finish(store)` is the explicit fallible completion path, in exact
order: restore default pragmas → final checkpoint + compaction → stamp extraction
version 2 through the state-gated `Store::stamp_extraction_version` → checkpoint
that stamp into the main database file (new `Store::checkpoint_wal_truncate`, so
`Current` corroboration can read it from main-file bytes) → close the final SQLite
connection (new `Store::close`, which propagates a close failure as typed
`StoreError::Close` and drops the lease clone only after the connection is gone) →
publish `phase=current` → remove the tombstone ONLY for a successful explicit
`init`. Every step propagates. `finish` consumes the handle, so a caller cannot
finalize twice; the handle's `Drop` performs NO publication at all, which is what
keeps an abandoned rebuild at `phase=building`, unreadable and fail-closed.

CLI wiring is the minimum coherent rebuild path. `BulkIndexPragmaGuard` no longer
reopens the DB through legacy `Store::open` in `Drop`; it now owns the
`FullRebuild` and exposes `begin`/`finish`. `index_project_inner` takes a
`RebuildKind` instead of `clear_first`, no longer pre-creates the index root (the
rebuild layer creates root + permanent lock under the lease), reads `counts()`
before the connection closes, and finalizes through `pragma_guard.finish(store)`
under a "Publishing index" phase line. `cmd_init` passes `ExplicitInit`;
`cmd_index` passes `Reindex` and no longer removes the DB up front, because the
destructive removal now happens after `phase=building` is durable. The unused
`remove_db_files` helper and the redundant inline extraction-version stamp were
deleted (the finalizer's state-gated stamp replaces the latter).

Behavioral Red (compiling, public-surface, no sleeps): new
`crates/codegraph-cli/tests/batch_m_finalizer.rs`
`full_rebuild_publishes_readable_current_last_via_cli` drove the shipped
`codegraph init` and then asked the accepted state gate. Command
`cargo test -p codegraph-rs --locked --test batch_m_finalizer
full_rebuild_publishes_readable_current_last_via_cli -- --exact` exited 101 with
`0 passed; 1 failed` and the assertion
`explicit init: the finished rebuild must publish phase=current as its LAST step
(plan lines 548-556); observed Missing — left: Missing, right: Current`. Setup
(`init` exit 0) and the non-empty v2 DB check both passed first, so this was
behavioral, not a compile/setup/panic failure. The same command now exits 0.

Deterministic fault matrix (private seams, unit-level, zero sleeps) in
`rebuild.rs`: `every_rebuild_fault_leaves_building_or_missing_and_unreadable`
injects a fault after each of Building publication, database removal, pragma
restore, compaction, stamp, stamp checkpoint, connection close, and immediately
before Current publication — every injection leaves `Building` or `Missing`, is
unreadable through `Store::open_for_read`, and never classifies `Current`.
`failed_current_publication_after_finalization_never_becomes_readable_current`
replaces the fixed `index.lock` immediately before the publication, so the
database is fully finalized and closed yet the publication fails with
`PermanentLockChanged` and the namespace stays `phase=building` and unreadable —
the case where publication itself fails after DB finalization.
`faults_at_or_after_current_publication_have_already_published_current` pins the
complementary direction. `dropping_an_unfinished_rebuild_never_publishes_current`
proves `Drop` is emergency-only. `only_successful_explicit_init_removes_the_tombstone`
stages a genuine interrupted-uninit namespace (authoritative `uninitialized` slot
published first, then the tombstone) and proves a `Reindex` is refused outright,
while a successful `ExplicitInit` removes the tombstone;
`an_earlier_fault_never_removes_the_tombstone` proves an interrupted finalization
never does. `a_successful_reindex_preserves_an_unrelated_tombstone_free_namespace`
proves a reindex never creates one. `rebuild_retains_one_exclusive_lease_and_never_reacquires`
proves a competing writer is blocked for the rebuild's whole life and succeeds only
after the finished handle is dropped. `existing_root_without_a_permanent_lock_fails_closed`
proves a lockless existing namespace is refused byte-nonmutating.

Verification, with `bash scripts/check-workspace-versions.sh` (exit 0, workspace
`0.40.4`) before every Cargo batch and `--locked` on every dependency-resolving
command: `codegraph-store --lib rebuild` 10/10; full `cargo test -p
codegraph-store` 155/0 across its seven suites; `cargo test -p codegraph-rs
--locked --test batch_m_finalizer` 1/1; `cargo test --workspace --locked` exit 0
with 2762 passed / 0 failed across 110 reporting suites (up from the 2695 recorded
for the classifier correction, no test weakened, deleted, or filtered);
`cargo check -p codegraph-store -p codegraph-rs --all-targets` clean;
`cargo clippy -p codegraph-store -p codegraph-rs --all-targets -- -D warnings`
clean; Linux-host `x86_64-pc-windows-msvc` all-target check clean for
`codegraph-store` (the CLI package cannot be cross-checked here: its `--all-targets`
build needs MSVC `lib.exe` for a C dependency, which is unavailable — the store
result is compilation evidence only and no native Windows runtime, crash, or
handle behavior is claimed).

LSP diagnostics again rejected both changed Rust files because
`/tmp/opencode/codegraph-rust-v15-impl` is outside the request cwd; Cargo
check/Clippy/test remain the accepted fallback.

Scope held: no daemon, watcher, MCP, uninit-continuation, project-scoped config, or
extension-relocation work started; `sync`'s own migration-mode escalation remains a
later slice. No dependency added, `Cargo.lock` untouched at
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, and no node-id
formula, schema, extraction output, or golden artifact changed (the workspace run
includes `codegraph-bench`'s golden byte-stability suites). These evidence bytes are
written before `make fmt` and the single authoritative final locked `make ci`; no
terminal Green is claimed until that post-documentation gate passes.

## Batch M — rebuild finalizer correction (2026-07-26)

Independent rereading of frozen Revision 14 (`5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450`)
found six defects in the retained candidate above. This append-only correction
supersedes only those claims; the original Red/Green history remains intact.

1. CLI recovery no longer equates raw `codegraph.db` existence with a healthy
   initialized namespace. `cmd_init` returns “Already initialized” only after a
   typed `Current` classification is corroborated through `Store::open_for_read`.
   `Current+tombstone` is explicitly retryable by `init`, while every other
   Current inconsistency stays typed/fail-closed. `index` has a narrowly separate
   discovery predicate that recognizes a durable non-Missing state even when an
   interrupted Building rebuild already deleted the DB; actual authorization is
   still decided later under the exclusive lease. Black-box regressions cover
   `Building` with DB present and absent through both explicit `init` and
   `index --force`, plus explicit-init recovery of `Current+tombstone` residue.
2. `begin_full_rebuild` deleted the racy pre-authorization
   `Store::extraction_status` decision. `Reindex` refusal now uses exactly
   `StoreWriteAuthorization::status()`, the classification accepted by
   `open_for_write` under the held lease. A deterministic checkpoint changes the
   slot to `Uninitialized` before authorization and proves the accepted status,
   not a stale precheck, controls refusal before DB mutation.
3. The same retained lease is now revalidated immediately before every DB
   artifact unlink, write-capable SQLite open, rebuild setup/final pragma,
   checkpoint, compact, stamp, close, state publication (inside the accepted
   publisher), and tombstone unlink boundary. The redundant `create_dir_all`
   between validation and SQLite open is gone. Fresh lock-replacement tests cover
   the write open and every destructive/finalization boundary without sleeps.
4. `FullRebuild::open_store(&self) -> Store` was replaced by consuming typestate:
   `FullRebuild::open_store(self) -> ActiveFullRebuild`. The active handle owns the
   exact `IndexPaths`, retained capability/authorization, and sole final SQLite
   writer. Its `finish(self)` accepts no caller-supplied Store, so a rebuild can
   open at most one final writer and cannot publish after finalizing another
   connection.
5. Tombstone fault injection now fails at the actual `remove_file` operation with
   `PermissionDenied`; the old after-success checkpoint is named only
   `AfterTombstoneRemoval` and is not represented as a removal failure. The
   operation failure leaves `Current+tombstone`, which remains unreadable and is
   recoverable only through repeated explicit `init`; the retry rebuilds and then
   removes the tombstone successfully.
6. `ActiveFullRebuild::Drop` now implements the frozen emergency best-effort
   cleanup: while retained authority still validates, it attempts default-pragma
   restoration/checkpoint and compaction, then closes the owned writer. Its code
   contains no state-publication or tombstone-removal path, so it structurally
   cannot publish `Current`. The regression checks sidecar cleanup while the
   authoritative state remains `Building` and unreadable.

Valid correction Red: before the typestate change,
`rebuild::tests::one_rebuild_handle_can_open_at_most_one_final_writer` compiled and
failed because a second live SQLite writer opened successfully. The corrected
focused batch is sleep-free and green: `cargo test -p codegraph-store --locked
rebuild::tests -- --nocapture` reports 15 passed / 0 failed, and `cargo test -p
codegraph-rs --locked --test batch_m_finalizer -- --nocapture` reports 4 passed /
0 failed. `bash scripts/check-workspace-versions.sh` ran first and passed before
each Cargo batch.

Scope remains exactly the finalizer slice: no sync/watch migration escalation,
uninit command/continuation implementation, daemon/MCP lifecycle relocation,
project-scoped config, extension/Godot relocation, unlock, dependencies, schema,
node-id, goldens, or Batches A-E. Final affected gates, MSVC store compile-only
cross-check, formatter, and the one authoritative `make ci CARGO='cargo --locked'`
are intentionally run only after these final evidence bytes land. Native Windows
runtime/crash coverage is not claimed.

## Batch M — emergency close-boundary correction (2026-07-26)

Final review found that the emergency `ActiveFullRebuild::Drop` path described in
correction item 6 above still relied on implicit field destruction for the final
SQLite close. That was weaker than the frozen requirement: closing the last WAL
connection is itself a mutation-capable SQLite boundary, so emergency cleanup
must retain and revalidate the exact lease through the explicit close attempt.
This append supersedes only the earlier implicit-close and sidecar-cleanup claims.

`Store::close` is now the shared explicit state-gated close boundary. Its field
order remains connection before lease, and it validates the retained capability
immediately before `Connection::close`. The portable contract is deliberately
narrow: validation and close are separate APIs, and neither Rust nor SQLite
provides an atomic check-and-close primitive. `ActiveFullRebuild::Drop` performs
best-effort pragma restoration/checkpoint and compaction, then calls that explicit
close path and reports any error to stderr without panicking. It still contains
no `publish_index_state` or tombstone-removal call, so an emergency drop cannot
publish `Current`.

The deterministic, sleep-free regression
`emergency_cleanup_close_is_explicitly_state_gated` replaces `index.lock`
immediately before `Store::close` and observes
`IndexLeaseValidationError::PermanentLockChanged`. The focused rebuild batch now
reports 16 passed / 0 failed; the CLI finalizer suite reports 4 passed / 0 failed;
the complete `codegraph-store` package suite passes; affected-package all-target
`cargo check` is clean; and affected-package all-target Clippy with `-D warnings`
is clean. Every Cargo batch was preceded by
`bash scripts/check-workspace-versions.sh` and used `--locked`.

The attempted Linux-host `x86_64-pc-windows-msvc` all-target check did NOT
produce compilation evidence: `libsqlite3-sys` rejected the available GNU
compiler for that target and `cc-rs` failed to find `lib.exe` (`No such file or
directory`). This is recorded as a host-toolchain limitation, not as a source
failure, and no native Windows runtime/crash/handle claim is made. LSP likewise
cannot attach to this external `/tmp` worktree (`LSP file path must be inside
request cwd`); the clean targeted Cargo diagnostics are the fallback.

These are pre-format evidence bytes. The terminal Green is recorded only after
`make fmt`, one fresh authoritative `make ci CARGO='cargo --locked'`, the required
`Cargo.lock` SHA-256 check, `git diff --check`, and scoped status inspection pass.

## Batch M — finalizer authoritative gate handoff (2026-07-26)

`full_sync_finalizer_publishes_current_last` is implemented and its verification is
now closed except for one explicitly named gate. This append records the evidence
actually observed; it supersedes no earlier claim and rewrites no historical
failure.

Delegated verification result (completed): focused rebuild batch 16 passed / 0
failed; CLI finalizer suite 4 passed / 0 failed; the complete `codegraph-store`
package suite green; affected-package all-target `cargo check` and Clippy with
`-D warnings` green; and a delegated `make ci CARGO='cargo --locked'` green.

Parent independent rerun (completed, not inherited): focused rebuild batch 16
passed / 0 failed; CLI finalizer suite 4 passed / 0 failed; the full
`codegraph-store` package green at 84 unit tests plus integration suites of 6, 12,
20, 14, 4, and 21 tests; affected-package `cargo check` and Clippy green. No
pre-existing test was weakened, deleted, filtered, or ignored.

Parent hands-on CLI QA (completed): `codegraph --version` printed `0.40.4`; an
unknown command exited nonzero; and on a fresh mini fixture, `init`, `status`,
`index --force`, and a second `status` all succeeded, with each `status` reporting
3 files, 13 nodes, 21 edges, the `.codegraph-v2/codegraph.db` path, and “Index is
up to date”.

Environment limitations, unchanged and still explicitly claimed as limitations:
LSP diagnostics rejected every changed Rust file with `LSP file path must be
inside request cwd`, so targeted Cargo diagnostics are the accepted fallback; and
the attempted MSVC cross-check produced no evidence because `libsqlite3-sys` /
`cc-rs` could not find `lib.exe`, so no native Windows runtime, crash, or handle
behavior is claimed.

These bytes are written BEFORE the parent's final `make fmt` and single
authoritative `make ci CARGO='cargo --locked'`. That final run must cover these
exact bytes, it is the sole remaining completion authority, and no later
documentation edit is permitted. Apart from that one named gate, no validation for
this item remains pending.

## Batch M — `incremental_sync_on_outdated_v2_forces_all_files` (2026-07-26)

Frozen plan lines 557-565 and test 9 (lines 750-751). An incremental sync now
CLASSIFIES BEFORE MUTATING A ROW and escalates a namespace it cannot repair
file-by-file to a deterministic full from-source migration under the SAME
retained exclusive lease.

Behavior landed:

- `StoreWritePurpose::IncrementalSync` classifies once under the already-held
  exclusive lease. A corroborated `Current` yields the incremental writer;
  `Missing`, `Outdated`, and a recoverable `Building` yield the lease-retaining
  `FullRebuildRequired` authorization; `Future` / `Corrupt` are refused before
  any byte moves; `Uninitialized` falls through to the typed
  `WritePurposeRejected` refusal, because only an explicit `init` may rebuild it.
- `codegraph_store::resume_full_rebuild(paths, authorization)` performs the
  destructive prologue (publish `phase=building`, then remove only the v2
  database files) using the lease clone the authorization already carries. It
  acquires NO lease, so no nested acquisition of a lock this process holds is
  possible, and it refuses any authorization whose retained status it did not
  accept for escalation. `begin_full_rebuild` and `resume_full_rebuild` now share
  one `begin_from_authorization` prologue, and the existing-vs-initial lease
  decision moved into `IndexLease::acquire_or_create_exclusive` so every
  lifecycle owner takes the same one outer capability.
- `codegraph-watch`'s new `migrate` module runs the migration: sorted
  `scan_project` candidates, no mtime/content-hash gate at all (so zero
  `files_skipped_unchanged`), absent tracked files simply never enter the fresh
  database, then framework extraction, batched resolution, cross-file
  finalization, and the rebuild finalizer's `phase=current` publication last. The
  persist order reproduces the CLI full index exactly (file row then nodes per
  file, ALL nodes before ANY edge, then edges, then refs, with the same WAL-valve
  folds and the same 10k/20k/20k/5k batch constants).
- Both sync entry points (`sync_project_once_with_progress` and
  `sync_changed_paths_with_patterns`) route through the gate, finish a `Current`
  incremental mutation with an explicit checkpoint + state-gated close, and
  refuse a `db_path` that is not this project's resolved v2 database.
- `Current` corroboration gained an `allow_live_sidecar` option used ONLY by the
  incremental writer. The sidecar-free rule remains the publication and read
  contract; a live in-process MCP reader legitimately recreates `-wal`/`-shm` on
  an untouched `Current` namespace, so requiring sidecar-freedom from an
  incremental WRITER made every watcher sync inside `serve` fail. The stamp is
  still validated from the already-checkpointed main-file bytes, so the version
  gate is unchanged.
- `codegraph_core::config::try_get_config` is the non-panicking accessor a
  library sync path uses when no binary initialized the global config.

Red evidence (recorded before Green): the new CLI suite
`crates/codegraph-cli/tests/batch_m_outdated_migration.rs` compiled and failed on
its business assertions — `incremental_sync_on_outdated_v2_forces_all_files`
panicked with `an Outdated namespace must bypass every mtime/content-hash skip
(plan lines 557-565); got 3 unchanged skips in: Synced: 0 reindexed, 3 skipped
(unchanged), 0 removed`; `outdated_migration_drops_absent_tracked_files` panicked
with `a migrated namespace must be readable: StateRejected { status: Outdated {
built: 1 } }`; and both refusal tests panicked because the old sync happily
reported `Synced: 0 reindexed, 3 skipped (unchanged), 0 removed` over `Future`,
`Corrupt`, and `uninitialized` namespaces. Exit status: `test result: FAILED. 0
passed; 4 failed`.

Green evidence: the same CLI suite reports 5 passed / 0 failed (including the
nonmutation-oracle self-test); focused rebuild tests report 17 passed / 0 failed;
the Store state-gate suite reports 21 passed / 0 failed; the complete
`codegraph-watch` package, including its unit and integration suites, passes; and
the affected-package tests, all-target check, and all-target Clippy with
`-D warnings` are clean. A development `cargo test --workspace --locked` run is
green with no failures; `cargo fmt --all --check` is clean;
`bash scripts/guardrail.sh` exits 0; `git diff --check` is clean; and `Cargo.lock`
still hashes to
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`. Every Cargo
batch was preceded by `bash scripts/check-workspace-versions.sh` and used
`--locked`.

Canonical equality is proven by the new suite itself: after a forced migration the
five canonical surfaces of the migrated database `diff_canonical`-match a fresh
v2 `init` + `index --force` peer built from the same tree, both for the
all-unchanged case and for the case where a tracked file was deleted.

No pre-existing test was weakened, deleted, filtered, or ignored. Two existing
`codegraph-watch` expectations needed state-gated fixture setup only: the shared
`TestDir` now publishes an empty `Current` namespace through the shipped rebuild
finalizer, because a `Missing` namespace legitimately escalates to a migration
now, and the two ignored-path counters assert over an incremental sync.

Environment limitations, unchanged and still claimed as limitations: LSP
diagnostics rejected every changed Rust file with `LSP file path must be inside
request cwd`, so targeted Cargo `check`/`clippy` diagnostics are the accepted
fallback; and no native Windows runtime, crash, or handle behavior is claimed
from this Linux host.

Final review corrections are included in those results. `resume_full_rebuild`
now accepts only an authorization issued specifically for
`StoreWritePurpose::IncrementalSync`, in addition to checking its retained state,
so another full-rebuild capability cannot be repurposed as sync migration
authority. The refused-state byte oracle now snapshots directories, complete
regular-file bytes, and symlink targets without following aliases, fails closed
on every I/O or unsupported-kind error, and has a self-test proving that an
equal-length byte replacement is detected.

These bytes are the input to the parent's final `make fmt` and single
authoritative `make ci CARGO='cargo --locked'`. No later documentation edit is
permitted; completion is decided by that post-format gate and the subsequent
lock-hash/diff/status audit.

## Batch M item 11 — interrupted uninit Red (2026-07-26)

Scope was checked before mutation. Acceptance item 10,
`interrupted_v2_rebuild_remains_fail_closed_and_legacy_untouched`, is already
covered by the committed rebuild finalizer's deterministic pre-Current fault
matrix (`every_rebuild_fault_leaves_building_or_missing_and_unreadable`), its
fixed-lock replacement/refusal tests, and the rebuild implementation that removes
only the resolved v2 DB/WAL/SHM paths. This slice therefore adds no separate
item-10 implementation; it reuses those primitives only where item 11 shares the
same lease-validation boundary.

The item-11 public behavioral Red was added as
`crates/codegraph-cli/tests/batch_m_uninit.rs` ::
`interrupted_uninit_state_slot_is_recoverable_not_corrupt`. Exact command, after
the required successful version gate:

```text
bash scripts/check-workspace-versions.sh && cargo test -p codegraph-rs --locked --test batch_m_uninit interrupted_uninit_state_slot_is_recoverable_not_corrupt -- --exact
```

Observed exit status: `101`; compilation and fixture setup succeeded, including a
successful real `codegraph init`. The sole business assertion then failed with
`left: Missing`, `right: Uninitialized`: existing `cmd_uninit` recursively removed
the entire v2 root instead of durably publishing `phase=uninitialized` and
preserving the permanent lock, tombstone, and fixed slots. This is a lifecycle
behavior failure, not a compile, setup, fixture, or unrelated-test failure.

## Batch M item 11 — interrupted uninit focused Green (2026-07-26)

The Store now owns one crash-recoverable uninit lifecycle. It performs a
nonmutating visible-state probe, acquires the pre-existing permanent lock
exclusively without repairing it, reclassifies and authorizes under that retained
lease, publishes a newer owner-bound `phase=uninitialized` slot, ensures the
tombstone, and removes only the resolved v2 DB/WAL/SHM, config, and runtime
children. The permanent lock, tombstone, both fixed state slots, parent directory,
and every legacy-root byte remain untouched. A repeated `uninit --force`
publishes another monotonic `uninitialized` sequence before continuing cleanup,
as required by frozen Revision 14 lines 409-429.

Current-state uninit corroborates the checkpointed main database extraction stamp
while permitting live WAL/SHM residue. This is intentionally the same narrow
writer-side distinction already used by incremental sync: a live reader may
legitimately recreate sidecars, and uninit must first authenticate Current,
publish `Uninitialized`, and only then delete those sidecars. Read and publication
paths remain sidecar-free.

Deterministic Store tests inject failure after state publication, tombstone
creation, each DB/sidecar removal, and each config/runtime removal. Every result
classifies `Uninitialized`, retains both slots and the permanent lock, and leaves
legacy bytes unchanged. A complete 16-combination DB/WAL/SHM/tombstone residue
matrix stays typed `Uninitialized`; Future, Corrupt, owner-mismatch, and
missing-lock fixtures are byte-nonmutating refusals. The snapshot oracle records
directories, complete regular-file bytes, and symlink targets, and its self-test
detects equal-length replacement.

Focused commands, each preceded by the successful workspace-version gate:

```text
cargo test -p codegraph-store --locked uninit -- --nocapture
# 5 passed; 0 failed

cargo test -p codegraph-rs --locked --test batch_m_uninit -- --nocapture
# 1 passed; 0 failed
```

The public CLI acceptance runs real `init -> uninit --force -> status --json`,
proves `initialized:false`, `extractionStatus:"uninitialized"`, and untouched
legacy presence, then proves `sync`, forced/plain `index`, and graph reads all
fail without changing either state slot. A repeated uninit succeeds with sequence
`N+1`; explicit `init` then publishes `building` and `current`, rebuilds the DB,
removes the tombstone only after Current publication, and again preserves legacy
bytes. Status now exposes stable `extractionStatus`, detail, legacy-presence, and
legacy-path fields for both initialized and non-initialized output.

All eight changed Rust files returned `No diagnostics found` from
`lsp_diagnostics`. Because the implementation worktree is outside the request
CWD, diagnostics were run against a detached same-HEAD worktree with the exact
tracked diff and both new Rust files copied byte-for-byte. Native Windows runtime
or crash behavior remains unavailable on this Linux host and is not claimed.

These bytes precede the final format and single authoritative locked CI gate; no
CI success is claimed in this section.

## Batch M item 11 — first full-gate correction (2026-07-26)

The first authoritative `make ci CARGO='cargo --locked'` attempt passed formatting,
Clippy, and all earlier workspace suites before failing two existing
`batch_m_outdated_migration` tests. Both failures were exact CLI policy regressions:
`sync` returned `CodeGraph not initialized` for authenticated `Outdated` state, so
the already-implemented under-lease migration gate was unreachable. No uninit,
Store, publisher, or golden test failed.

Root cause was the item-11 change from DB-presence initialization to typed Current
initialization while `cmd_sync` still called `resolve_required_project`. The fix is
to use `resolve_required_rebuild_project` for sync discovery, matching the frozen
command matrix: authenticated Outdated/Building reaches migration;
Uninitialized reaches the Store gate only to receive its typed nonmutating refusal.
No authorization was widened.

Focused correction command, after the workspace-version gate:

```text
cargo test -p codegraph-rs --locked --test batch_m_outdated_migration --test batch_m_uninit -- --nocapture
# batch_m_outdated_migration: 5 passed; 0 failed
# batch_m_uninit: 1 passed; 0 failed
```

This correction is included before the final formatting pass and the one remaining
full-gate retry; that retry's result is not claimed here.

## Batch M item 11 — second full-gate result and stale-test correction (2026-07-26)

The second and final permitted `make ci CARGO='cargo --locked'` attempt passed the
workspace-version gate, formatting, Clippy, and all workspace tests reached through
the new Store/CLI uninit suites, Outdated migration, v2 namespace, and related CLI
coverage. It then stopped at one pre-existing `cli_commands` assertion named
`uninit_requires_force_then_removes`: the test still required the entire
`.codegraph-v2` root to disappear. That expectation directly contradicted frozen
Revision 14, which requires preserving the root, permanent lock, tombstone, and
both fixed state slots after successful cleanup. No production behavior failed at
that point.

The stale test was updated, not weakened: it still proves uninit without `--force`
is refused and nonmutating, then proves forced uninit succeeds, classifies
`Uninitialized`, preserves the lifecycle root/lock/tombstone/two slots, and removes
the v2 database. The test is renamed
`uninit_requires_force_then_preserves_recovery_state` to state the protocol
contract accurately.

Per the two-check ceiling, no third complete CI run is claimed. Closure uses one
post-format targeted command covering the corrected existing CLI suite together
with the new item-11 acceptance suite, followed by the lock-hash and scoped-diff
audit; its actual result is reported in the task handoff rather than retroactively
represented as a full-CI success.

## Batch M item 11 — independent-review blocker correction (2026-07-26)

An independent review rejected the preceding candidate on four concrete defects;
this append preserves the prior evidence and records the corrective implementation
without claiming another full workspace CI run.

1. SQLite sidecar paths no longer pass through `Path::display()`. Store uninit and
   rebuild append `-wal` / `-shm` directly to the database's native `OsString`, and
   every item-11 test helper uses the same lossless construction. A Unix-only
   regression creates a non-UTF-8 project path, proves the true native WAL/SHM are
   removed, and proves the distinct lossy-rendered lookalike WAL survives.
2. An absolute `CODEGRAPH_DIR` is exact-project-only for implicit CLI discovery.
   From a nested directory, `uninit --force` now fails with the stable remedy
   `pass the project root explicitly` and a complete namespace snapshot proves no
   byte or entry changed; the same command with the explicit project root remains
   accepted. Default/relative roots retain ancestor discovery.
3. Status ancestor discovery now uses authenticated lifecycle state rather than
   readable-Current alone. Nested status reports parent `Uninitialized`,
   `Outdated`, `Building`, `Future`, and `Corrupt` state, while a lock-only parent
   remains a non-marker. Discovery still never opens SQLite or authorizes a
   mutation; those decisions remain in Store's typed state gates. Outdated sync
   migration reachability and Uninitialized sync refusal are unchanged.
4. The public negative command matrix now compares a fail-closed whole-namespace
   snapshot: directory presence, complete regular-file bytes, symlink targets,
   and native `PathBuf` keys. Every I/O/unsupported-kind failure panics, comparison
   diagnostics list only changed paths, and an equal-length replacement self-test
   proves the oracle catches same-size mutation.

The Store uninit fault matrix now pins the exact `Interrupted.step`, exact
tombstone state, and exact ordered deletion frontier at every checkpoint while
also requiring the retained root, permanent lock, two fixed slots, unchanged
legacy namespace, and `Uninitialized` classification. Rebuild fault evidence now
independently interrupts explicit-init recovery immediately after Building and
Current publication. Building interruption retains the tombstone and remains
Building; Current interruption retains the tombstone and remains Current; both
keep two valid slots, never become accidental Corrupt, and a later explicit init
successfully reaches Current before removing the tombstone.

Focused tests for each correction passed during implementation, each after the
workspace-version gate. Final affected-package tests, diagnostics, formatting,
Clippy, and lock/scope audit are recorded in the task handoff after their actual
execution. Native Windows runtime/crash coverage remains unavailable on this Linux
host and is not claimed.

## Batch M item 12 — process writer serialization and crash recovery (2026-07-26)

Production already owned the required one-outer-lease architecture, so item 12
adds acceptance/regression evidence only. New integration target
`crates/codegraph-store/tests/writer_process_lifecycle.rs` drives actual child
processes through `begin_full_rebuild`: the holder reports `READY` only after
that shipped full-writer entry point has acquired and retained the outer
exclusive `IndexLease`, published Building, and completed its destructive
prologue. The parent launches the losing production writer only after `READY`;
the child accepts exactly `RebuildError::Lease(IndexLeaseError::TimedOut)` and
reports `LEASE_TIMED_OUT`. Any acquisition or unrelated setup/state/config
error fails the test.

While the holder still owns the lease, the parent snapshots the complete v2
namespace before the loser starts and compares it immediately after the loser
exits. The fail-closed oracle keys entries by native `PathBuf` and records root
and nested directories, complete regular-file bytes, and symlink targets without
following aliases. Every traversal, metadata, read, or unsupported-entry error
panics with bounded path/kind diagnostics; comparison never prints database
bytes. `namespace_oracle_detects_equal_length_content_mutation` replaces four
bytes with four different bytes and proves the oracle rejects the same-size
mutation. A separately staged legacy namespace is byte-identical before and
after contention, holder release, and crash recovery.

The crash test sends the acknowledged holder an OS `SIGKILL` through
`Child::kill`, asserts signal 9 rather than treating it as a harness failure,
then starts a fresh child through the same `begin_full_rebuild` production path.
That writer completes `ActiveFullRebuild::finish`, reports
`RECOVERED_CURRENT`, and the parent independently requires both typed
`ExtractionStatus::Current` and successful `Store::open_for_read`
corroboration. This proves kernel release on abnormal process termination; no
Rust `Drop` cleanup is involved in releasing the crashed holder's lease.

Initial focused execution, after the workspace-version guard:

```text
cargo test -p codegraph-store --locked --test writer_process_lifecycle -- --nocapture --test-threads=1
# 4 passed; 0 failed
```

The first compile attempt found two test-only pattern errors for the structured
`Building { built }` status; the first behavioral execution then exposed a
harness bug where the parent treated libtest's blank preamble as the READY line,
dropped the fixture, and caused child publication I/O failures. Neither was
counted as Red or production evidence. The READY reader now ignores all lines
until the exact sentinel, matching the established `index_lease` harness. No
production source, extraction, schema, node-id, golden, manifest, dependency, or
lockfile byte changed. Native Windows runtime/crash behavior remains unavailable
from this Linux environment and is not claimed.

The guarded affected-package validation subsequently passed the exact item-12
target (4/4), existing `index_lease` integration target (12/12), complete
`codegraph-store` package (175 tests, 0 failures), all-target check, and
all-target Clippy with `-D warnings`. The first trailing `cargo fmt --all --check`
reported formatting-only diffs in the new test; no behavioral diagnostic failed.
Those bytes were normalized before the final format/hash/scope audit. Direct LSP
diagnostics were attempted and rejected with the exact environment error
`LSP file path must be inside request cwd` because this required worktree is
under `/tmp/opencode/codegraph-rust-v15-impl`; the clean Cargo check and Clippy
results are the diagnostics fallback.

## Batch M item 12 correction — fail-closed snapshot reads and joined holder output (2026-07-26)

Manual review rejected two proof claims in the item-12 entry above and one
harness-lifecycle detail. The earlier claim that every metadata/read error
failed closed was false because `root.exists()` followed aliases and converted
all root metadata failures into apparent absence. The claim that regular-file
bytes were read without following aliases was also incomplete:
`symlink_metadata(path)` followed by `fs::read(path)` left a check-then-path-read
window. Finally, `Holder::crash` dropped the tail receiver without joining the
stdout reader, so its detached `tail_tx.send(...).expect(...)` could panic after
the holder was killed. The prior history remains intact; this section supersedes
only those proof-harness claims. Production behavior is unchanged.

The corrected Unix-only namespace oracle performs one no-follow
`symlink_metadata(root)` and accepts absence only for the typed
`io::ErrorKind::NotFound`; every other metadata error panics. A root symlink or
non-directory is rejected before `read_dir`. For each regular file, the oracle
records the initial no-follow regular-file identity `(st_dev, st_ino)`, opens the
path once, requires the opened handle to be regular and have that identity,
corroborates the fixed path with no-follow metadata immediately before reading,
reads the complete bytes through that same handle, and corroborates the fixed
path again afterward. Disappearance, alias replacement, kind drift, or identity
drift returns the explicit test-only `PathChangedDuringRead(PathBuf)` result.
This is pre/open/pre-read/post-read identity corroboration; it does not claim a
portable atomic path-check-and-read primitive.

The deterministic self-test
`namespace_oracle_detects_regular_path_replaced_by_symlink_before_read` replaces
the validated regular path with a symlink at the exact
`RegularPathValidated` checkpoint, with no sleep or probabilistic race. Before
the correction it failed because the snapshot returned the external target's
complete bytes as `RegularFile`; afterward it receives the explicit changed-path
result and proves the authoritative read-boundary checkpoint was never reached.
The existing equal-length mutation self-test remains unchanged and effective. A
second root-validation self-test proves typed NotFound absence and fail-closed
rejection of a root symlink, regular root, and non-NotFound metadata error.

Holder output is now owned by a `JoinHandle<Result<String, String>>`, not a
detached sender. Normal release waits for the child and joins the reader before
checking `RELEASED`; crash waits for SIGKILL termination and joins/drains the
reader before dropping receiver state; defensive `Drop` also kills/reaps the
child and joins any remaining reader. READY semantics are unchanged: the child
emits it only after `begin_full_rebuild` returns with the production exclusive
lease held.

The corrected focused target passed 6/6 after the workspace-version gate,
including the two real child-process behavior tests and both prior oracle tests.
Locked validation then passed: corrected `writer_process_lifecycle` 6/6,
existing `index_lease` 12/12, and complete `codegraph-store` 177/177 (94 unit +
6 bulk-pragmas + 12 lease + 20 classifier + 14 publisher + 4 schema + 21 Store
gate + 6 process-lifecycle; doc tests 0). Workspace all-target `cargo check` and
all-target Clippy with `-D warnings` were clean. Every Cargo batch was preceded
by `bash scripts/check-workspace-versions.sh`, and every dependency-resolving
Cargo command used `--locked`. Rust formatting and the repository doc-format
check passed after the final evidence bytes were written. Changed-file LSP was
attempted and again rejected by the environment with
`LSP file path must be inside request cwd` for the required external worktree;
the clean workspace all-target Cargo check and Clippy are the recorded fallback,
not an LSP-clean claim. No production source, manifest, dependency, schema, node
ID, golden, workspace version, or `Cargo.lock` byte is changed.

## Batch M item 12 correction #2 — directory identity-corroborated traversal (2026-07-26)

Manual review rejected one remaining claim in the correction above: regular-file
reads were identity-corroborated, but root and nested directories still went from
`symlink_metadata(directory)` to path-based `read_dir(directory)` without proving
that an opened directory handle and the fixed path identified the initially
observed directory. A replacement external-directory symlink could therefore be
enumerated before later entry checks. This append supersedes only that proof
claim; the prior history remains intact and production code is unchanged.

The Unix-only snapshot helper now captures `(st_dev, st_ino)` from the initial
no-follow observation for the root and every discovered nested directory. It
opens each directory once, requires the opened handle to be a directory with the
same identity, and rechecks the no-follow fixed path against that identity before
path-based enumeration, after the complete entry list is collected, and after
the collected entries are processed. A disappearance, alias/type replacement,
or identity drift returns `PathChangedDuringRead(PathBuf)`. A failed non-NotFound
open, `read_dir`, or entry read still fails loudly after corroboration; only the
initial root's typed NotFound maps to an empty namespace. Collected paths are not
processed until the post-enumeration check passes, so failed corroboration cannot
produce a successful authoritative snapshot.

This is an honest identity-corroborated successful-return boundary around
path-based `read_dir`, not a claim that portable `std` supplies atomic no-follow
enumeration. Exact checkpoints distinguish initial no-follow directory validation,
opened-handle plus fixed-path corroboration before enumeration, and successful
post-enumeration corroboration.

Two deterministic no-sleep tests cover both replacement frontiers. The root test
replaces the root immediately after its initial no-follow validation; the nested
test replaces an already opened/corroborated nested directory immediately before
enumeration. Each replacement is a symlink to an external directory containing a
unique sentinel entry, requires the exact changed directory in
`PathChangedDuringRead`, and contains a fail-branch assertion that the sentinel
cannot occur in any successful authoritative snapshot. The pre-existing regular
file, equal-length mutation, typed root-validation, writer contention, and
SIGKILL recovery tests remain unchanged. Focused implementation execution passed
8/8 after the workspace-version gate. Final locked package/check/Clippy/format,
LSP, lock-hash, and scope results are appended only after they run.

### 2026-07-26 Batch M item 12 final-gate formatting correction

The parent validation run for the directory-proof correction is recorded here
exactly as it happened, superseding the pending-results sentence above without
deleting it. After the workspace-version gate, the guarded locked runs passed in
order: the focused item-12 target 8/8, the existing `index_lease` target 12/12,
the complete `codegraph-store` package test suite, `cargo check --workspace
--all-targets --locked`, and `cargo clippy --workspace --all-targets --locked -D
warnings`. The chain then failed at `cargo fmt --all --check`, which reported a
single formatting-only diff in
`crates/codegraph-store/tests/writer_process_lifecycle.rs` at line 756: rustfmt
requires the nested-directory replacement callback's
`if path == nested && checkpoint == SnapshotCheckpoint::DirectoryHandleAndPathCorroborated`
condition on one line instead of the manually wrapped multiline form. Because the
commands were joined with `&&`, nothing after that check ran, so no `make
fmt-check`, `git diff --check`, or lock-hash result from that attempt is claimed.

The correction changes formatting only — no test name, assertion, checkpoint,
child-process protocol, or oracle behavior is altered. After normalizing those
bytes and appending this evidence, the reruns that actually executed and passed
are: `bash scripts/check-workspace-versions.sh`, `cargo fmt --all --check`, `make
fmt-check`, the guarded focused item-12 target, `cargo check --workspace
--all-targets --locked`, `cargo clippy --workspace --all-targets --locked -D
warnings`, `git diff --check`, and the frozen `Cargo.lock` SHA-256 audit. The
changed-file LSP diagnostics attempt and its exact result are reported in the
notepads; no LSP-clean claim is made when the request is rejected for being
outside the request cwd. Native Windows runtime/crash coverage and portable
atomic no-follow enumeration remain explicitly unclaimed, and the authoritative
full locked CI gate remains the parent's.

## Daemon watcher startup catch-up race correction (2026-07-26)

The pre-existing `daemon_single_watcher_fires_once` timing failure was reproduced
before any production edit with the exact focused target. Its first daemon event
was `watcher sync #1: 0 file(s) reindexed, 0 removed`, and the unchanged test then
failed because that line did not name `brand_new_symbol.ts`. A diagnostic-only
assertion-message expansion, which merely rebuilt the test binary, made the same
target pass; this timing switch confirmed a startup-order race rather than an
extraction defect.

Source and call-path inspection proved the exact ordering: the daemon registers
one shared `ProjectWatcher`, starts one full-project catch-up thread, and then
accepts clients. Catch-up and a queued watcher event contend for the same
exclusive writer. When catch-up wins, it indexes the new file first; the queued
watcher sync subsequently sees identical content and correctly reports zero DB
mutations. No second watcher exists, and the catch-up log is not the source of the
failing `watcher sync` line.

The correction preserves those two distinct facts. `SyncOutcome.changed_paths`
still means paths actually reindexed or removed by that sync. A new sorted watcher
event field, `trigger_paths`, records the exact debounced event batch even when a
concurrent writer already applied the bytes. `event_loop` attaches that batch only
to watcher completion outcomes, and the daemon's bounded filename tail now uses
`trigger_paths`; reindexed/removed counters remain actual mutation counters.
Direct/full sync callers retain an empty trigger set. The one-watcher topology,
catch-up concurrency, immediate client handling, debounce, and `--no-watch`
behavior are unchanged. A pure regression pins that a zero-mutation watcher
completion still retains its trigger path; no sleep, retry-until-pass, assertion
weakening, or test serialization was added.

Validation was run once after the final implementation, with
`bash scripts/check-workspace-versions.sh` first and `--locked` on every
dependency-resolving Cargo command. The exact focused daemon test passed five
consecutive runs; the complete `daemon_single_watcher` target passed 2/2,
including `daemon_no_watch_does_not_autosync`. `codegraph-watch` passed 90 unit +
2 integration tests, and `codegraph-daemon` passed all unit/integration targets.
Workspace all-target `cargo check`, workspace all-target Clippy with
`-D warnings`, `cargo fmt --all --check`, and `scripts/guardrail.sh` all passed.
Changed-file LSP diagnostics were attempted for all three modified Rust files but
the environment rejected the external worktree with `LSP file path must be inside
request cwd`; the clean workspace Cargo check and Clippy are the recorded
diagnostics fallback, not an LSP-clean claim. No dependency, schema, node-id
formula, extraction/golden output, manifest, workspace version, or item-12
process-lifecycle test was changed.

### 2026-07-26 authoritative locked-CI closure

After the complete item-12 acceptance and watcher correction bytes above were
finalized and repository formatting was applied, the parent authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'`
completed successfully. That exact final-byte run passed fmt-check, workspace
Clippy with `-D warnings`, the full workspace test suite including the Unix
writer-process lifecycle and watcher/process targets, and the no-AI/vector/LLM
guardrail. `Cargo.lock` remained frozen at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.
Changed-file LSP diagnostics remain unavailable because this required external
worktree is rejected with `LSP file path must be inside request cwd`; Cargo
check, Clippy, tests, formatting, and the guardrail are the diagnostics fallback,
not an LSP-clean claim. Native Windows runtime/crash validation remains
unavailable and is not claimed.

## Batch M acceptance closure — items 13, 14, 18, and item 19 config-core prerequisite (2026-07-26)

This append records the parent-reviewed acceptance evidence for four frozen
slices on implementation HEAD
`3863c2694583564964c36b6953dfec96046c9ab3`. The parent reviewed the exact Rust
bytes later committed by this closure; those bytes were not edited during the
closure. Their pre-commit SHA-256 values were:

| Slice    | File                                                | SHA-256                                                            |
| -------- | --------------------------------------------------- | ------------------------------------------------------------------ |
| M13      | `crates/codegraph-store/tests/index_lease.rs`       | `41a929b7ab0048ec8b385e90d3e00cead515ac79e877d7321433c0e5ece5963f` |
| M14      | `crates/codegraph-store/tests/store_state_gates.rs` | `8948cc04f30c2335cb335f85d3c8c78d122c5c30a52e3e86ddaa66f38f8f7a53` |
| M18      | `crates/codegraph-watch/src/sync.rs`                | `81705e5be3da7abea26dfb58710e2e93108707a0cd5b3f6d294746fa6ab4b2b2` |
| M18      | `crates/codegraph-watch/src/watcher.rs`             | `e6bc6c2ef64f4db000e3a8bf66f68a44ed1daddcb5beca4f3168c34cff91fb18` |
| M19-core | `crates/codegraph-core/src/config.rs`               | `e600ad0a3908655814be82e0246f01cc43f11696b4bee16276b5f3fe268d27cb` |

The frozen Revision 14 plan remained unchanged at SHA-256
`5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450`.
No manifest, lockfile, golden, schema, node-ID formula, `UPSTREAM.md`, or
`KNOWN_DIFFS.md` byte is part of these slices.

### M13 — lease owner and Store drop ordering

`index_lease` now proves shared and exclusive parent/clone ownership through
separate synchronized contender processes. Dropping a non-final parent or clone
keeps incompatible contenders blocked; dropping the final owner admits a fresh
contender immediately. A real finalized Current namespace additionally proves
that a live read `Store` retains its shared lease through both SQLite handles and
that `Store` closes those handles before its final retained lease is released.
The Windows-only replacement branch is compile/injected-branch evidence; it was
not executed on native Windows.

Parent focused result: `index_lease` **13/13 passed**. The M13 target acceptance
passed on these exact bytes.

### M14 — read/status opens never migrate

`store_state_gates` stages a physically current SQLite schema whose recorded
schema version is deliberately migration-eligible, plus exact metadata canaries.
Both `Store::open_for_read` and `Store::open_for_status` must accept the Current
namespace without running migration or metadata repair. The acceptance oracle
checks the complete namespace snapshot, absence of WAL/SHM creation, SQLite
schema rows, schema-version rows, and project-metadata rows before, during, and
after Store lifetime. Stamp-mismatch refusal is covered through the same
nonmutation oracle. SQLite sidecars are derived losslessly by appending to the
native `OsString`, never through `display()`.

Parent focused result: `store_state_gates` **22/22 passed**. The M14 target
acceptance passed on these exact bytes.

### M18 — removed directories escalate to a pattern-aware full sync

The watcher now preserves notify removal classification through the debounce
loop. Explicit `RemoveKind::Folder` is a directory removal; Windows
`RemoveKind::Any` regains directory semantics only when its normalized path is in
the watcher-owned known-directory set captured at startup and extended when new
directories are registered. This avoids guessing from a missing path or filename
extension and keeps extensionless file removals distinct.

A removed watched directory dominates its debounce burst and schedules exactly
one full-project sync so every absent tracked descendant can be removed. Ignored
directory removals remain ignored. The default full-sync closure passes the
watcher's own `WatchOptions.include` and `WatchOptions.exclude` into the scan,
rather than falling back to process-global patterns; sorted/deduplicated trigger
paths remain attached to the outcome.

Parent focused result: all **5 focused removal tests passed**; the complete
`codegraph-watch` package passed **95 unit + 2 integration tests**.

### M19-core — project-scoped config API prerequisite only

This is deliberately **M19-core, not complete item 19**. It adds
`Config::load_for_paths(cli_path, paths) -> Result<Arc<Config>>` with precedence
explicit CLI path → `APP_CONFIG` → the resolved project's
`IndexPaths::config_toml()` → defaults. The API does not consult legacy
`.codegraph/config.toml`, the process working directory, or another project's
paths, and it does not cache across projects. Its seven focused tests cover
explicit/environment/project precedence, missing defaults, malformed current
config, two-project isolation, intentional shared override, and rejection of
legacy/CWD discovery. The transitional global `init_config`/`get_config` and
legacy `Config::discover` remain for unmigrated callers.

Parent focused result: `codegraph-core` **65/65 passed**, including
`load_for_paths` **7/7 passed**. Production CLI, MCP, daemon, and watch config
plumbing has **not** migrated to this API and remains the downstream portion of
item 19.

### Parent authoritative verification and environment limits

The parent ran the focused suites above, workspace all-target check, workspace
Clippy with `-D warnings`, and formatting on these exact Rust bytes. It then ran
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'`;
the workspace-version gate, fmt-check, Clippy, complete workspace tests, and
guardrail all passed, and the terminal printed `✅ All CI checks passed!`.

Changed-file LSP diagnostics were attempted but the tool rejected this required
external worktree with `LSP file path must be inside request cwd`. The clean
locked Cargo check, Clippy, tests, and formatting are the diagnostics fallback;
this ledger does not claim LSP-clean results. The platform-independent injected
`RemoveKind::Any` branch tests cover Windows watcher classification logic, but no
native Windows runtime validation was run and none is claimed.

`Cargo.lock` remained byte-identical at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.
This evidence was appended before the closure's final `make fmt` and authoritative
locked CI run so no post-CI documentation edit can invalidate formatting.

## Batch M item 15 acceptance closure — request-scoped reader leases (2026-07-26)

The CLI and both MCP request paths now open a state-gated `Store` through a
request-scoped `CodeGraphEngine`. The retained shared `IndexLease` spans SQLite
corroboration, query execution, and complete owned result materialization, then
drops before the next request. No SQLite connection or lease remains cached in a
long-lived MCP session. Status contention is reported as `rebuilding: true` and
`initialized: false`; raw DB-path presence is not represented as a corroborated
readable Current index.

The deterministic cross-process acceptance barrier is feature-gated through
`codegraph-store/test-hooks` and is feature-unified only into package test builds.
It acknowledges shared or exclusive ownership only after kernel locking and
final fixed-path corroboration, then waits on a bounded loopback socket rather
than inferring ordering from sleeps. The reader acceptance covers CLI query,
direct stdio MCP, the real daemon proxy, streamable HTTP MCP, and status. The
writer acceptance holds the watcher's exclusive lease before SQLite open and
proves that a new read and busy status neither open nor mutate DB/WAL/SHM bytes.
The HTTP child uses a fixture-owned `CODEGRAPH_HTTP_REGISTRY_DIR`, which is
explicitly removed and checked absent after child termination.

The SQLite nonmutation oracle treats only typed `NotFound` as absence. Every
other metadata, open, read, kind, length, or identity error fails closed. It
rejects aliases and non-regular entries, reads complete bytes from one handle,
and re-corroborates that the fixed path still names that handle. Unix uses
device/inode identity; the Windows branch uses `FileIdInfo`. Executable self-tests
prove rejection of a non-regular artifact and, on Unix, an alias without reading
or changing its external target.

Replacement freshness is behavioral rather than cache-seam dependent: one
long-lived `McpServer` serves only the replacement graph on its next request
without calling `close_cached_handles()`. The retained diagnostic counts observed
DB identity changes between successful requests; it does not count engine or
connection opens. The compatibility seam only clears identity observations and
has a separate regression proving that it cannot synthesize a replacement event.
Fixture builders used by MCP tests now finalize directly created/copied SQLite
bytes with a permanent lock, extraction stamp, and `Building -> Current` state
publication through the feature-gated test helper; production read gates remain
strict.

Focused tests on the reviewed bytes passed before this append, with
`bash scripts/check-workspace-versions.sh` before Cargo batches and `--locked` on
dependency-resolving commands: `lease_lifetime` passed **5/5** and `reopen`
passed **3/3**. Targeted package check, Clippy with `-D warnings`, formatting, and
`git diff --check` also passed. Changed-file LSP diagnostics were retried for all
modified and new Rust files, but the tool rejected this required external
worktree with `LSP file path must be inside request cwd`; locked Cargo diagnostics
are the fallback, and this ledger does not claim LSP-clean results. Native Windows
runtime validation was unavailable and is not claimed; Windows-only identity
code remains compile/CI-owned evidence.

The frozen Revision 14 plan remains required at SHA-256
`5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450`, and
`Cargo.lock` remains required at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.
No schema, node-ID formula, extraction/golden byte, workflow, `UPSTREAM.md`, or
`KNOWN_DIFFS.md` change belongs to this slice. These evidence bytes are written
before formatting and the one authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'` run;
no final Green is claimed until that post-documentation gate passes.

## Batch M item 22 acceptance closure — verified v0.40.4 legacy fixture (2026-07-26)

Item 22 asserts two DIFFERENT claims about an unmodified old scanner, and keeps
them apart. Source visibility is configuration-dependent: with the DEFAULT
supported-extension configuration the published v0.40.4 `files` output is exactly
`src/app.ts`, `src/math.ts`, and `tools/greeter.py`. Storage authority is
configuration-independent: under an EXPLICIT legacy `.codegraph/codegraph.json`
override for `.json`/`.toml`, the same binary additionally reports
`.codegraph-v2/config.toml` and `.codegraph-v2/codegraph.json` as SOURCE — accepted
and documented, because the user asked for those extensions — while every v2
namespace byte stays identical, the v2-only symbol never enters the legacy
database or graph, and the v2 reader still serves that symbol afterwards. Reading
a v2 artifact's text is never the same thing as holding authority over v2 storage.

Nothing in this slice is built from this worktree. The fixture executes the REAL
published release: tag `v0.40.4`, commit
`aba40799ecacb94515f7e1690914d2accc4c8973`, version stdout `codegraph 0.40.4`.
Only the two natively-executed CI hosts are pinned, each running its own asset —
no cross-execution and no emulation. Linux x86_64 musl: archive
`10026272` bytes, archive SHA-256
`b549c0980b0f52f6b753f529322cdbc8892e03ef3736ec227a9e8f49985a3bd2`, member
`codegraph`, executable SHA-256
`1a14d195be755b27d0e1625d7d7e4662412a07d77cc0d0e518793cd50f2182d1`. Windows
x86_64 MSVC: archive `9988644` bytes, archive SHA-256
`eda7cfd6d2d0cc85fd8bd6ba66be1d7130a9b00609255730ad155ce6fa1351db`, member
`codegraph.exe`, executable SHA-256
`e52703f3a3d5bef90997ce23d9a3b49c980e6bcc1a078fdb1245ad1305a5bc09`.

`scripts/setup-legacy-fixture.sh` is the only sanctioned way to obtain that
binary. It selects the CURRENT NATIVE host's asset, downloads over HTTPS, and
verifies every declared field — archive size, archive SHA-256, extracted
executable SHA-256, and the exact `--version` stdout — before printing a path.
Exactly one archive member is extracted by name, so an archive-supplied path can
never escape the destination. The cache is digest-addressed and revalidated on
EVERY run, so a stale or corrupt entry is re-downloaded rather than trusted. An
EXIT trap sweeps the staging directory and any partial archive, and never touches
an executable that already passed full verification. There is no skip path: a
missing network, a size or digest mismatch, a missing member, or a wrong
`--version` all exit nonzero, and an unavailable fixture is a fixture-setup
FAILURE rather than a skipped test.

The manifest is the single authority for what the legacy binary must be; no
executable digest or version string is duplicated in Rust. Asset-block uniqueness
is STRUCTURAL: `[[asset]]` blocks are parsed as records so block cardinality
survives the parse, exactly one block may name the requested target regardless of
whether duplicate blocks carry a digest, that sole block must declare exactly one
`executable_sha256`, and the digest must be exactly 64 lowercase hexadecimal
characters. Missing target, duplicate target blocks, missing digest, duplicate
digest fields, and malformed digests all fail loudly, each proven by a compact
synthetic-manifest regression rather than by the production manifest.

The nonmutation oracle fails closed. Only a typed `NotFound` root yields an empty
snapshot; an existing root that is an alias, a Windows reparse point, or a
non-directory is refused with a typed error. Root and nested directories are
opened without following aliases, identity-corroborated against the fixed path
before enumeration AND re-corroborated after enumeration, so collected children
become usable only once the directory still proves to be the same object. Regular
files are read completely through one opened handle, with the handle and the fixed
path re-checked after the full read. Unix uses `(dev, ino)`; Windows uses raw
`GetFileInformationByHandleEx(FileIdInfo)` — an exact identity on both platforms,
never a size or timestamp approximation. The test-local SHA-256 is pinned to
published NIST vectors, so the executable digest gate does not rest on an
unverified hash. Deterministic checkpoint self-tests prove each gate: static root
alias refusal without reading or modifying the alias target, root replacement,
nested-directory replacement, and regular-file replacement.

Linux runtime QA on the integrated bytes: a fresh-cache setup downloaded and
verified `1a14d195…0f2182d1`; cache-only `--print` exited 0 on a valid cache and
nonzero on both a missing cache and a cache corrupted by an appended byte; normal
setup then re-downloaded and revalidated it back to the pinned digest, leaving
only the executable in the slot; unknown and surplus arguments exited 2; and an
offline attempt under `unshare -rn` against a fresh cache failed nonzero with the
fixture-setup message and left no partial archive and no extraction residue.
`bash -n` and `shellcheck` are clean.

`cargo test --locked -p codegraph-rs --test batch_m_legacy_extension_override`
passes **21/21** on the integrated bytes, with
`bash scripts/check-workspace-versions.sh` run before every Cargo batch. Targeted
package check, Clippy with `-D warnings`, formatting, guardrail, and
`git diff --check` also pass. Changed-file LSP diagnostics were attempted for the
new Rust target and the tool again rejected this required external worktree with
`LSP file path must be inside request cwd`; locked Cargo diagnostics are the
fallback and this ledger claims no LSP-clean result.

Native Windows/MSVC runtime was NOT executed and is not claimed. Windows coverage
in this slice is CI WIRING plus compile-gated code: the `windows-latest` job gains
a `bash`-shell step that selects the Windows `.zip` asset, and the exact
`FileIdInfo` identity, reparse-point refusal, and `FILE_FLAG_BACKUP_SEMANTICS`
directory opens follow the repository-proven layout already used by
`codegraph-store` and `codegraph-core`. The version-branch verifier self-test is
Unix-gated and says so explicitly on other platforms, while production
`--version` verification stays unconditional. This slice adds no dependency and
changes no schema, node-ID formula, extraction or golden byte, `UPSTREAM.md`, or
`KNOWN_DIFFS.md`; `Cargo.lock` remains required at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`. These
evidence bytes are written BEFORE repository formatting and the one authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'` run
over these exact final bytes; no final Green is claimed until that gate passes.

## Batch M item 16 acceptance closure — long-lived MCP releases handles per request (2026-07-26)

Frozen plan item 16 asks for `long_lived_v2_mcp_releases_handles_per_request`: a
long-lived shipped MCP process must let the v2 main database be REPLACED on
Windows without the compatibility close seam and then serve only the replacement
graph, using a child-process ready/continue barrier around request completion and
file replacement, explicitly selected in the native Windows CI job.

The acceptance lands as one new query-side test target,
`crates/codegraph-cli/tests/batch_m_long_lived_mcp.rs`, over the SHIPPED binary
(`CARGO_BIN_EXE_codegraph`). No production byte changed: M15 already made MCP
engines request-scoped, so this item is the behavioral proof of that ownership,
not a new mechanism.

The barrier is the MCP wire protocol, never a sleep. ONE
`codegraph serve --mcp --no-watch --path <project>` child (daemon opted out via
`CODEGRAPH_NO_DAEMON=1`, so the session is a single direct process rather than a
proxy) is driven over stdio pipes. READY is the arrival of the framed JSON-RPC
response for request 1 — rmcp writes that frame only after the request produced
its owned, fully materialized result, so the frame proves end-to-end completion
rather than mere SQL completion. The parent then acquires the namespace's
EXCLUSIVE lease through `IndexLease::acquire_exclusive_existing`; a retained
reader lease makes that acquisition fail, so it is the fail-closed observation
that the child released everything. Still holding the lease, the parent
`fs::rename`s a separately built database over the live main database file — the
Windows-specific handle proof, since `MoveFileEx` + `REPLACE_EXISTING` fails with
a sharing violation while any process holds an open handle on the destination.
CONTINUE is the next request frame, which cannot have been read earlier because it
had not been written. The 60s channel/lease deadlines are deadlock guards only; no
assertion is satisfied by waiting, and neither mtime nor process exit is used as
ordering evidence.

Both databases come from the real `codegraph init`, so the replaced namespace
keeps its own permanent lock, its own published `Current` state slots, and an
exact extraction stamp in the replaced bytes. The test additionally asserts the
replaced namespace stays sidecar-free and that the lock and state slots survive,
so no state/sidecar/stamp/lease gate is weakened or repaired.

The startup catch-up race identified in manual review is CLOSED with runtime
evidence rather than documented as a caveat. `serve --mcp --path` spawns a
one-shot catch-up sync on a detached thread and `--no-watch` does not disable it,
so a reindex could otherwise satisfy (or later undo) the post-replacement
assertions. The two distinguishing sources therefore live under a directory the
served project's own root `.gitignore` excludes, and the test PROVES the exclusion
with the shipped `codegraph_extract::engine::scan_project` — the same scanner a
sync feeds from — before the acceptance runs: the neutral shared file is scanned,
both supplied files are not. No sync can create those rows (the scanner never
yields the paths) and none can delete them (the cold removal pass only considers a
tracked path absent from disk, and both files stay present, which the test also
asserts). The remaining scannable file is byte-identical in both graphs, so the
catch-up is a proven no-op. The only way request 2 can see the replacement file is
by reading the supplied bytes.

The forbidden-seam invariant is kept and strengthened rather than weakened: it now
inspects only NON-COMMENT lines of this file's own source for a needle assembled
from two halves, so it detects a real invocation, an import, an alias, or a
wrapper while the module docs may still name the seam in prose. A companion unit
test proves the oracle sees a real call line and ignores a comment mentioning the
same name, so it cannot pass vacuously.

Three negative-control runs establish the assertions are load-bearing (each
reverted immediately, file restored to SHA-256
`773a23abf4dee6ddac8acd6cf7bee6a0600dd9203051c8efc22d9f7502f40731`): renaming the
target onto itself instead of installing the replacement fails request 2 with
`No results found for "hbtvancur"`; commenting out the `.gitignore` line fails the
scan-freeze precondition, which is exactly the reindex loophole the design closes;
and adding a real `server.close_cached_handles()` call makes the forbidden-seam
oracle fail and name the offending line.

Local Green on the integrated bytes, with
`bash scripts/check-workspace-versions.sh` run before every Cargo batch and
`--locked` on every Cargo command:
`cargo test --locked -p codegraph-rs --test batch_m_long_lived_mcp` **3/3**,
`cargo test --locked -p codegraph-rs --test lease_lifetime` **5/5**, and
`cargo test --locked -p codegraph-mcp --test reopen` **3/3**. Workspace all-target
check, Clippy with `-D warnings`, formatting, guardrail, and `git diff --check`
also pass. Changed-file LSP diagnostics were attempted for the new target and the
tool again rejected this required external worktree with
`LSP file path must be inside request cwd`; locked Cargo diagnostics are the
honest fallback and this ledger claims no LSP-clean result.

Native Windows/MSVC runtime was NOT executed here and is not claimed — this Linux
host cannot provide it. Windows coverage in this slice is CI WIRING: the existing
native `windows-latest` job gains one additional step,
`cargo test -p codegraph-rs --test batch_m_long_lived_mcp`, which selects this
exact target explicitly so the replacement acceptance runs natively and is never
reduced to compile-only or skipped. Every existing Windows step — including the
M22 legacy-fixture wiring — is byte-preserved, and the surrounding jobs, action
pins, and the `CI Success` gate are unchanged.

This slice adds no dependency and changes no schema, node-ID formula, extraction
or golden byte, `UPSTREAM.md`, or `KNOWN_DIFFS.md`; the frozen plan is untouched
at SHA-256 `5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450` and
`Cargo.lock` remains required at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`. These
evidence bytes are written BEFORE repository formatting and the one authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'` run
over these exact final bytes; no final Green is claimed until that gate passes.

## Batch M item 17 acceptance closure — a failed engine open is not cached and the next request recovers (2026-07-26)

Frozen plan item 17 asks for
`failed_engine_open_is_not_cached_and_next_request_recovers`: one long-lived
shipped MCP process must take a request whose engine/`Store` open FAILS for a real
v2 state/artifact reason, survive it, retain nothing from it, and — after the
namespace is repaired WITHOUT a restart — serve only the repaired graph on the
next request in that same process.

The acceptance lands as one new query-side test target,
`crates/codegraph-cli/tests/batch_m_failed_open_recovery.rs`, over the SHIPPED
binary (`CARGO_BIN_EXE_codegraph`). No production byte changed: M15 already made
every MCP engine request-scoped and M16 proved the per-request handle release, so
item 17 is the behavioral proof that the FAILURE path shares that ownership rather
than a new mechanism.

The staged failure is a genuine v2 inconsistency, never a stub. The served project
is really indexed by the shipped `codegraph init`; then BOTH fixed state slots are
removed and nothing else, so the namespace classifies `Missing` while its main
database and its permanent lock are still present. `Store::open_for_read` refuses
exactly that through `reject_missing_database_artifacts` with
`state is missing but a database artifact already exists at <db>`, which the test
asserts directly against the staged fixture BEFORE any server is involved. No gate
is weakened, relaxed, or repaired to manufacture the failure.

The failure is deliberately POST-RESOLUTION. Because the current-namespace DB file
still exists, `roots::probe_root` classifies the project `Indexed` and
`resolve_project_arg` resolves it, so request 1 fails inside the engine open. The
test pins that distinction: the request-1 tool error must contain
`Failed to open project at …` AND the missing-state-with-database diagnostic, and
must NOT contain `No indexed project`. A resolution miss therefore cannot be
mistaken for the engine-open failure this item is about. The failure also arrives
as an `isError` tool result rather than a JSON-RPC transport error, which is the
evidence the live session survived it.

Determinism comes from protocol frames and fail-closed gates. READY is the arrival
of request 1's complete JSON-RPC response frame — rmcp writes it only after the
owned result materialized. The parent then acquires the namespace's EXCLUSIVE
lease: a shared reader lease retained by the FAILED open makes that acquisition
fail, so it is the fail-closed proof that the failed open released everything.
Still under that lease the parent `fs::rename`s the separately built repaired
database over the live main database file — on native Windows that fails with a
sharing violation while any process holds an open handle on the destination, which
is the Windows handle proof for the failure path. Repair is then COMPLETED by the
protocol-aware `Missing -> Building -> Current` fixture finalizer
(`codegraph_store::test_support::finalize_current_test_fixture`), and completion is
observed from published state and artifact shape — status `Current`, a published
state slot, the preserved permanent lock, a sidecar-free database — never from
elapsed time. CONTINUE is request 2's frame, written only after all of that. The
60s deadlines are deadlock guards only; no assertion is satisfied by waiting, by
mtime, or by process exit, and the server is never restarted.

Startup catch-up is excluded as an explanation for BOTH halves, with runtime
evidence rather than a caveat. It cannot repair the staged namespace: a dedicated
test, `catch_up_sync_refuses_missing_state_with_database_without_mutating_bytes`,
drives the SAME `codegraph_watch::sync_project_once` entry point the catch-up
thread calls against an identically staged namespace and proves it fails with the
same state/artifact diagnostic while leaving the database bytes byte-identical, the
state slots absent, no tombstone created, and the permanent lock intact — a gate,
not a race. It also cannot invent the repaired rows: following M16's pattern the
distinguishing sources live under a root-`.gitignore` directory and the exclusion
is PROVEN in-test with the shipped `codegraph_extract::engine::scan_project`; all
three supplied files stay present on disk so the cold removal pass cannot delete
their rows either, and the single scannable neutral file is byte-identical in every
graph so a catch-up is a proven no-op.

Recovery is additionally proven not to be a silent legacy fallback: the project
carries a legacy `.codegraph/codegraph.db` holding a TRAP symbol present in no
other graph, and request 2 must neither surface the trap file nor resolve the trap
symbol (`No results found`). Request 2 must also render real search results
(`## Search Results (`) containing the repaired file, and the pre-repair symbol
must resolve to nothing in the same session — absence observed through a lookup
that finds nothing, not through a response that merely omits it.

A second test, `in_process_engine_open_failure_leaves_no_cached_state_and_reopens_repaired`,
pins the same seam in-process: `CodeGraphEngine::open` is the exact call
`execute_owned` makes per request, so a failed open followed by the same legitimate
repair must let a LATER open in the SAME process serve the repaired graph with no
memoized error, lease, or handle in the way. The forbidden-seam oracle from M16 is
carried over unchanged in spirit (structural, code-lines-only, two-half needle,
with its own unit test), so the recovery flow provably never depends on
`close_cached_handles`.

Four negative-control runs establish the assertions are load-bearing (each
reverted immediately, the file restored to SHA-256
`263c6a76a3b2412c4434c5e4be9ce08772986d94ad8ead6b0d9d790ec181b756`). Memoizing the
first failed open per project in `execute_owned` makes request 2 return the cached
`Failed to open project at … state is missing but a database artifact already
exists` error and the acceptance fails — this is the exact production regression the
item forbids, and the test detects it. Holding an extra SHARED lease across the
repair makes the exclusive acquisition time out with
`TimedOut { path: …/index.lock }`, so the no-retained-lease proof is real rather
than incidental. Skipping the protocol-aware republication leaves the namespace
`Missing` and fails the repair-completion assertion, so recovery genuinely requires
a legitimate `Current` publication. Leaving the state slots in place fails the
staging assertion `removing both fixed state slots must classify the namespace
Missing`, so the fixture cannot silently degrade into a happy-path test.

Local Green on the integrated bytes, with
`bash scripts/check-workspace-versions.sh` run before every Cargo batch and
`--locked` on every Cargo command:
`cargo test --locked -p codegraph-rs --test batch_m_failed_open_recovery` **5/5**,
`cargo test --locked -p codegraph-rs --test lease_lifetime` **5/5**,
`cargo test --locked -p codegraph-rs --test batch_m_long_lived_mcp` **3/3**, and
`cargo test --locked -p codegraph-mcp --test reopen` **3/3**. Workspace all-target
check and Clippy with `-D warnings` pass. Changed-file LSP diagnostics were
attempted for the new target and the tool again rejected this required external
worktree with `LSP file path must be inside request cwd`; locked Cargo diagnostics
are the honest fallback and this ledger claims no LSP-clean result.

Native Windows/MSVC runtime was NOT executed here and is not claimed — this Linux
host cannot provide it. M16's explicit `windows-latest` selection and every other
CI byte are preserved unchanged; item 17 adds no CI wiring, so its Windows
`MoveFileEx` behavior is documented as unexecuted rather than asserted.

This slice adds no dependency and changes no schema, node-ID formula, extraction or
golden byte, `UPSTREAM.md`, or `KNOWN_DIFFS.md`; the frozen plan is untouched at
SHA-256 `5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450` and
`Cargo.lock` remains required at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`. These evidence
bytes are written BEFORE repository formatting and the one authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'` run
over these exact final bytes; no final Green is claimed until that gate passes.

Addendum for exactness: the negative-control restore hash
`263c6a76a3b2412c4434c5e4be9ce08772986d94ad8ead6b0d9d790ec181b756` is the
pre-`make fmt` byte state of `batch_m_failed_open_recovery.rs`, which is what each
control was reverted to. Repository formatting afterwards rewrapped exactly one
function signature (`build_graph_database`) and nothing else, so the committed
target hashes
`7d5ac54647c6bb5a66235a5ca1d8f0a3988d4630078d0db4b891a52e695b2f1f`; the assertions
the controls exercised are byte-unchanged.

## Batch M item 19 acceptance closure — global HTTP uses project-scoped v2 configs (2026-07-26)

`global_http_uses_project_scoped_v2_configs` is now proven end to end, and the
process-global configuration singleton is GONE from the codebase. M19's core
prerequisite (`Config::load_for_paths`, commit `496afa2`) only added the API; this
slice migrates every production consumer onto it, so configuration is an immutable
`Arc<Config>` derived from the addressed project's resolved `IndexPaths` and threaded
explicitly through the operation that uses it.

What replaced the singleton. `codegraph-core::config` keeps exactly two loaders:
`Config::load_for_paths` (project-scoped: explicit CLI path → `APP_CONFIG` →
`IndexPaths::config_toml` → defaults) and the new `Config::load_env_or_default`
(process bootstrap: explicit path → `APP_CONFIG` → defaults, and nothing else).
`init_config`, `get_config`, `try_get_config`, `Config::discover`, and the `OnceLock`
are deleted, so no code path can consult a project or CWD legacy
`.codegraph/config.toml` and none can share one project's settings with another. The
CLI's `main` now loads the bootstrap config for ONE purpose — the logger level — and
`Cli::bootstrap_project_root` (which existed only to feed the singleton a project) is
removed with it. A residual audit over `crates/` finds zero occurrences of
`get_config`, `try_get_config`, `init_config`, or `Config::discover`.

Extension overrides and the Godot DSL are now explicit values, not discoveries.
`codegraph-extract::ext_config` becomes an immutable `ExtensionOverrides` loaded from
`IndexPaths::extension_config`; the ancestor `.codegraph/codegraph.json` tree-walk,
its process-CWD join, and its mtime cache are all deleted. The value rides
`ExtractOptions.extensions` (built by the new `ExtractOptions::for_project`) into
`scan_project`, `extract_project`, the new `extract_file_with_options`, and the new
`detect_language_with` / `extract_source_with`; `detect_language` and `extract_file`
remain as the override-free entry points for callers that address no project.
`codegraph-resolve::frameworks::godot_dsl_config` becomes an immutable
`GodotDslConfig` (`resourceFields` + `idFields`) loaded the same way, its two
mtime caches and its own tree-walk deleted, and it reaches `.tres` parsing through a
new `FrameworkExtractionContext` (project root + config) that replaces the bare
`project_root: &str` parameter of `FrameworkResolver::extract`. Tolerance is
preserved exactly: a missing, unreadable, or malformed `codegraph.json` still yields
empty overrides / an empty DSL config, so an unconfigured project behaves
byte-identically. Because nothing is cached, an edited config is observed on the next
load with no mtime dependence — `custom_ext_reload_picks_up_changes_immediately`
replaces the old sleep-based mtime-recache test and needs no sleep at all.

Sync, watcher, and daemon. `codegraph-watch::sync` gains a per-operation
`ProjectScope` (the project's `ExtractOptions` + its `FrameworkExtractionContext`),
loaded once from the addressed `IndexPaths` at the top of every entry point —
`sync_project_once`, `sync_project_once_with_progress`,
`sync_project_once_with_patterns` (the watcher's removed-directory escalation),
`sync_changed_paths`, and `sync_changed_paths_with_patterns` — and threaded into the
scan, `extract_file_with_options`, `detect_language_with`, the framework pass, and
`migrate_project`. The transitional `scan_options()` that read the singleton is gone.
`WatchPolicy` now carries the project's overrides (`with_extension_overrides`) so
`should_handle_file` agrees with the scan on a project-declared custom extension, and
the new `WatchOptions::for_project` / `watch_options_for_project` derive
include/exclude, debounce, and the enable flag from that project's own config (an
explicit `CODEGRAPH_WATCH_DEBOUNCE_MS` and `--no-watch`/`CODEGRAPH_NO_WATCH` still
win, so the documented escape hatches stay authoritative). `DaemonOptions.include` /
`.exclude` are deleted: the daemon loads the project's watch config itself, so a
daemon can no longer inherit whichever project its launcher started in. Startup
catch-up is untouched and still runs; it simply goes through the same per-project
load.

MCP. `CodeGraphEngine` holds the addressed project's `Arc<Config>`, loaded per
request beside the request-scoped store (M15's ownership model is unchanged), and its
four on-disk source reads go through one `read_project_source` that refuses a file
larger than THAT project's `indexing.max_file_size`. This is what makes the
acceptance observable over HTTP: extraction already skips an oversized file, so
serving its full text through a graph tool would contradict the project's own policy;
the refusal reuses the existing unreadable-file rendering, so no new output shape is
introduced.

Behavioral Red (recorded before implementation, at the real public surface — the
core API already existed, so an API-absence or compile failure would not qualify).
Against clean `73833fa` with only the new acceptance target added,
`cargo test --locked -p codegraph-rs --test global_http_project_scoped_config` fails
2/2. The project-scoping half fails with
`alpha's own include must force its gitignored Tools/ in: ["src/app.ts", "src/math.ts", "tools/greeter.py"]`
— the process-global singleton, bootstrapped from whatever root the CLI resolved,
never applied alpha's own current-root `include`. The `APP_CONFIG` control fails with
`without APP_CONFIG beta must use its own config again: ["Tools/helper.ts", "src/app.ts", "src/math.ts", "tools/greeter.py"]`.
A third, isolated Red run (temporary target, deleted afterwards) drove the ONE global
HTTP process against clean HEAD and failed with
`alpha's own 120-byte max_file_size must refuse src/app.ts` while the response body
carried all ten lines of `src/app.ts` — the direct proof that one HTTP process served
project A under a configuration A never declared.

Green. `cargo test --locked -p codegraph-rs --test global_http_project_scoped_config`
**2/2**. The acceptance target proves, in ONE process with `APP_CONFIG` unset and
hostile LEGACY `.codegraph/config.toml` + `.codegraph/codegraph.json` planted in BOTH
projects (each asking for the other's `include` and a 7-byte size cap): index and
sync scope each project by its own config in both directions; the single global
`serve --http` (no `--path`) honors alpha's 120-byte `max_file_size` and beta's
default for the SAME `src/app.ts` — requested alpha → beta → alpha, so neither
ordering nor reuse can hide a bleed; each project's `codegraph_files` listing carries
only its own tree; and the live watcher auto-syncs a new `Tools/` file for alpha
while never adopting it for beta. The control
`app_config_overrides_both_projects_including_codegraph_dir_collision` proves
`APP_CONFIG` INTENTIONALLY supersedes both projects' own configs, that pointing both
projects at ONE absolute `CODEGRAPH_DIR` still yields distinct identity-suffixed
current roots (root, DB, and `config.toml` all differ, both roots exist side by
side), and that with `APP_CONFIG` unset again each project falls back to its own
config — the override is process-wide, not sticky state on disk.

Affected-package Green, with `bash scripts/check-workspace-versions.sh` before every
Cargo batch and `--locked` on every Cargo command: `codegraph-core` **66**,
`codegraph-extract` **all suites green** (349 unit + 17 integration suites, including
the rewritten `custom_ext` 7/7 and `coverage_ext_config` 6/6), `codegraph-resolve`
**642 unit + 12 integration suites**, `codegraph-watch` **98 unit + 2 integration**,
`codegraph-mcp` **256 unit + integration suites green**, `codegraph-daemon` **all
suites green**, and `codegraph-rs` **612 tests, 0 failures** across every target.
Workspace `cargo check --locked --workspace --all-targets` and
`cargo clippy --locked --workspace --all-targets -- -D warnings` pass.

Existing coverage was migrated, never weakened. `custom_ext`,
`coverage_ext_config`, `godot_dsl`, `godot_idfields_cwd`, and
`godot_idfields_determinism` now write the config where production reads it (the
project's current root) and load it explicitly; each gained a NEW negative control
proving a legacy `.codegraph/codegraph.json` is never adopted
(`legacy_codegraph_json_is_never_read`, `custom_ext_ignores_legacy_and_other_projects`,
`legacy_dsl_config_is_never_adopted`). `codegraph-watch` gained
`sync_project_once_ignores_a_legacy_project_config`,
`two_projects_in_one_process_use_their_own_configs`, and
`project_extension_overrides_reach_scan_and_incremental_sync`; the pre-existing
`sync_project_once_indexes_gitignored_dir_named_in_include` lost its
singleton-dependent `if` guard and now asserts unconditionally. The two daemon tests
that replicated the binary's `init_config` startup no longer need to. No test was
deleted, skipped, or weakened to pass.

Determinism and caching. No path-keyed cache was introduced: every consumer loads an
immutable per-operation value, which is why the two deleted mtime caches needed no
replacement and why the golden byte-stability suite is unaffected (goldens,
node-ID formula, schema, and extraction semantics are all untouched, and the
`codegraph-bench` equivalence oracle passes as part of the workspace run).

Limitations, stated honestly. Changed-file LSP diagnostics were attempted and the
tool again rejected this required external worktree with
`LSP file path must be inside request cwd`; locked Cargo check/Clippy/tests are the
fallback and this ledger claims no LSP-clean result. Native Windows/MSVC runtime was
NOT executed and is not claimed. This slice adds no dependency and changes no schema,
node-ID formula, extraction or golden byte, `UPSTREAM.md`, or `KNOWN_DIFFS.md`; the
frozen plan is untouched at SHA-256
`5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450` and `Cargo.lock`
remains required at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`. These evidence
bytes are written BEFORE repository formatting and the one authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'` run over
these exact final bytes; no final Green is claimed until that gate passes.

## Batch M item 20 acceptance closure — daemon rendezvous lifecycle under `uninit --force` (2026-07-27)

Frozen plan item 20 (lines 590-612, 787-797). Two named acceptances plus the paired
fail-closed case now exist as real-process tests in
`crates/codegraph-cli/tests/batch_m_daemon_uninit_lifecycle.rs`:
`daemon_start_during_uninit_observes_uninitialized_and_tombstone_before_publish`,
`uninit_shutdown_control_drains_without_pid_kill`, and
`unresponsive_daemon_leaves_recoverable_uninitialized_without_kill`.

Behavioral Red on the pre-change bytes (compiling test, real failures — not a build
error and not a missing environment):
`cargo test --locked -p codegraph-rs --test batch_m_daemon_uninit_lifecycle` →
**0 passed, 3 failed**. `daemon_start_...` failed with "lease barrier was not reached
before its finite deadline" (startup took NO shared lease at all, so it never reached
the store's post-acquisition checkpoint); `uninit_shutdown_control_...` failed with
"the daemon never published its v2 rendezvous at
…/.codegraph-v2/daemon.pid" (the rendezvous was still written under the legacy
`.codegraph` root); `unresponsive_daemon_...` failed because `uninit --force`
SUCCEEDED while a live recorded owner had never been drained.

Green: **3/3**. Ordering evidence is deterministic, never a sleep. The concurrent-start
test publishes the authoritative `uninitialized` slot and then the tombstone while it
still holds uninit's exclusive lease, and the competing `serve --mcp` child is stopped
at the store's test-only post-acquisition SHARED-lease barrier; the "published nothing"
snapshot is taken at that exact checkpoint and re-checked after the child exits,
against BOTH the v2 identities and the legacy `.codegraph` spellings. The drain test
asserts process exit status, `signal() == None`, removal of pid + socket, removal of
the v2 database, and recoverable `Uninitialized`; the fail-closed test asserts the
recorded live owner is still running, that NO runtime child was removed, and that a
repeated `uninit --force` resumes the cleanup idempotently once the owner is gone.

Production slice. `IndexPaths` is now the sole authority for every rendezvous path:
`crates/codegraph-daemon/src/paths.rs` derives pid/log/socket from
`IndexPaths::resolve` and every accessor is FALLIBLE, so an unsafe or unresolvable
configured root fails closed instead of reconstructing a `.codegraph*` path (the
out-of-root POSIX-tmpdir and Windows namespaced names are
`sha256("codegraph-v2-" || projectIdentity)[..8]`, provably distinct from the legacy
`sha256(path)` name). `crates/codegraph-daemon/src/control.rs` (new) owns the
versioned, project-identity-bound control frame plus `request_daemon_shutdown`.
`session.rs` answers a control frame at the hello seam, BEFORE any engine or
data-request lease exists — which is exactly why it can be served while
`uninit --force` holds the namespace exclusively. `lib.rs` gates startup through a
caller-supplied `StartupGate` whose returned capability is retained across
pid/socket publication and dropped only afterwards. `codegraph-store`'s
`uninit_index_with_drain` runs the drain INSIDE the retained exclusive lease, after
both durable markers and before any child removal; a drain error is
`UninitError::DaemonNotDrained`, fail-closed. `codegraph-watch` gained
`SyncCancellation` so a shutdown refuses queued lease loops and interrupts a running
one instead of waiting out the 30s lease budget.

### Orchestrator-caught defects and their corrections

The orchestrator manually READ the M20 diff and rejected the first candidate even
though `batch_m_daemon_uninit_lifecycle` was 3/3 and `control_shutdown` was 3/3. Both
suites passed **with a hardcoded-success mutant still in the tree**: an un-restored
negative control at `session.rs` wrote `shutdown_reply_line(true)` and the compiler's
`unused variable: drained` was the only signal. The lesson recorded here is that a
green suite is not evidence when no test observes the real call site — the incomplete
case was covered only by a stub responder and by a direct unit call to
`shutdown_reply_line(false)`, neither of which routes through `serve_control_frame`.

Corrections, each with a load-bearing test:

1. **Success ACK despite an incomplete drain.** `run_accept_loop_async` logged
   `!drained` and still sent success. `ShutdownRequest::ack` now carries the drain
   RESULT (`oneshot::Sender<bool>`), `serve_control_frame` serializes that value, and
   `ControlAck::for_drain(false)` is what the caller maps to
   `ShutdownOutcome::Unresponsive`. Pinned by
   `an_exhausted_drain_budget_replies_incomplete_instead_of_success`, which drives a
   REAL daemon with `DaemonOptions::drain_budget = 0` and a connected silent client:
   it reads `drained: false` off the wire. Mutant proof — reinstating
   `shutdown_reply_line(true)` (with `let _ = drained;`) turns exactly this test red
   (`left: true, right: false`) while the other three stay green; the byte was then
   restored and the suite re-run 4/4.
2. **No active session close, and a Unix-only mechanism.** Shutdown merely waited.
   `SessionRegistry` now owns a `tokio::sync::watch` closing signal
   (`close_all_sessions` / `is_closing`), which is platform-independent; the Unix
   raw-fd half-close remains only as an acceleration. Both the first-line read and the
   rmcp serve loop race that signal, so a silent client and a long-lived rmcp client
   are both ended by the daemon. `send_replace` (not `send`) is required: `send` fails
   when no receiver exists yet, which would let a session accepted after the signal
   start serving. Pinned by
   `authorized_shutdown_closes_a_long_lived_session_before_acknowledging` (the silent
   client's socket must EOF) — disabling `close_all_sessions` turns it red with
   `drained: false`.
3. **Unbounded watcher join before the bounded wait.** The old order was
   `lease_loops.cancel(); drop(watcher); drain(...)`, and `ProjectWatcher::Drop →
stop_inner → join` can block for a whole extraction pass, making the incomplete-ACK
   path unreachable. `ProjectWatcher` now separates signalling from joining:
   `begin_shutdown()` (stop OS events, cancel loops, ask the loop to exit; never
   joins), `is_finished()` (a flag the loop sets as its last action), and `detach()`
   (release the handle so no destructor re-introduces the join). The daemon calls
   `begin_shutdown` → `close_all_sessions` → `drain_watcher_loops_and_sessions(...)`
   → `stop()` only when drained, else `detach()`. Pinned by
   `begin_shutdown_and_detach_never_block_on_a_running_sync` (a barrier-blocked sync,
   so "still running" is deterministic) and by the daemon-side
   `drain_reports_completion_only_when_loops_and_sessions_reach_zero`, which asserts
   an unfinished watcher ALONE yields an incomplete drain.
4. **Owned-rendezvous cleanup could unlink a replacement owner's socket.**
   `cleanup_owned_lock` now RETURNS ownership and `cleanup_owned_rendezvous` removes
   the socket only when the pid record still names this process; an owner mismatch
   preserves both the replacement record and its socket. Pinned by
   `cleanup_owned_rendezvous_preserves_a_replacement_owners_record_and_socket`.
5. **Slot-only startup validation.** The gate previously checked
   `Store::extraction_status == Current` plus tombstone absence, which would still
   publish a rendezvous for a `Current` slot whose database was deleted, whose
   sidecars reappeared, or whose extraction stamp is stale.
   `authorize_daemon_startup` now uses `Store::open_for_read`, which acquires ONE
   bounded shared lease and, under it, corroborates the full contract (owner-bound
   slots, tombstone absence, DB presence, sidecar-freedom, exact stamp from the
   checkpointed main file). The returned `Store` OWNS that same lease, so retaining it
   across publication needs no second acquisition — no nested lock, no TOCTOU window.
6. **Foreign control versions leaked into the data path.** `parse_control_frame`
   rejected `codegraph_control == 0`, which would have routed those bytes into the
   JSON-RPC executor. Every line that deserializes as a `ControlFrame` is now
   recognized and refused explicitly; authorization stays the only gate. Pinned by
   `a_foreign_protocol_version_is_never_authorized` (0, +1, `u8::MAX`) and by
   `an_unauthorized_frame_is_refused_without_shutting_the_daemon_down`, which sends a
   foreign identity, a foreign version, and a foreign action against a live daemon and
   asserts each is answered `drained: false` with the rendezvous intact and the daemon
   still serving.

A finite caller-side wait is now honest on every platform: `interprocess`'s
send/recv timeouts are Unix-only, so `request_daemon_shutdown` runs the exchange on a
worker thread and bounds the WAIT with a monotonic channel deadline. No PID is ever
signalled on any path; `uninit` only ever REPORTS the recorded pid.

Migrated coverage, never weakened. Pre-existing daemon/CLI tests changed only because
the rendezvous moved to the v2 current root: they now resolve pid/socket/log through
`IndexPaths` instead of hardcoding `.codegraph/daemon.*`, and their project fixtures
canonicalize a real directory because `IndexPaths` derives a PHYSICAL identity.
`lease_lifetime.rs`'s barrier listener now accepts EVERY arrival instead of only the
first, because a daemon legitimately reaches the shared-lease checkpoint twice (its
startup capability, then a per-request lease) — the test releases the startup arrival
and still asserts the per-request one, which strengthens rather than loosens it. No
test was deleted, skipped, ignored, or timing-loosened.

Verification, with `bash scripts/check-workspace-versions.sh` before every Cargo batch
and `--locked` on every Cargo command: `codegraph-rs`
`batch_m_daemon_uninit_lifecycle` **3/3**; `codegraph-daemon` `control_shutdown`
**4/4** and the whole package green (98 unit + every integration suite);
`codegraph-watch` **green**; `codegraph-store` uninit suite **8/8** including the two
new drain tests; `codegraph-rs` **all targets green**;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean.

Limitations, stated honestly. Changed-file LSP diagnostics were attempted and the tool
again rejected this external worktree with `LSP file path must be inside request cwd`;
locked Cargo check/Clippy/tests are the fallback and no LSP-clean result is claimed.
Native Windows/MSVC runtime was NOT executed: the cross-platform claim covers the
mechanism (a `tokio::sync::watch` close signal and a thread-bounded caller wait, with
no Unix-only primitive on the correctness path) and compilation, not a Windows run.
This slice adds no dependency and changes no schema, node-ID formula, extraction or
golden byte; the frozen plan is untouched at SHA-256
`5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450` and `Cargo.lock`
remains at SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`. These evidence
bytes are written BEFORE repository formatting and the one authoritative
`bash scripts/check-workspace-versions.sh && make ci CARGO='cargo --locked'` run over
these exact final bytes; no final Green is claimed until that gate passes.

## Batch M item 20 correction — rendezvous cleanup ownership order (2026-07-27)

Manual orchestrator review of the committed `df86a5a` found one remaining ownership
race and rejected it. `cleanup_owned_rendezvous` called `cleanup_owned_lock` FIRST
(which corroborates ownership and removes the pid record), then unlinked the socket.
The published pid record IS the single-instance exclusion — `try_acquire_daemon_lock`
claims it with `create_new` — so between those two operations the namespace was
unowned: a replacement daemon could legitimately claim the record and bind the SAME
socket path, and the departing process's later unlink then destroyed the LIVE
daemon's socket while its record still advertised it. Every client would fail to
attach with no artifact left to explain why.

Correction: the order is inverted and ownership is re-corroborated at BOTH mutation
boundaries. `cleanup_owned_rendezvous` now (1) corroborates that the record names
this pid and LEAVES IT PUBLISHED, (2) unlinks the socket while that record still
excludes every competing start, (3) removes the record only via `cleanup_owned_lock`,
which re-reads ownership immediately before deleting. An owner mismatch observed on
entry still preserves both the replacement record and its socket and reports `false`.
No PID is signalled, no wait was introduced, and the permanent index lock, state
slots, tombstone, and DB are never named by this path.

Crash behavior is explicitly asymmetric and that asymmetry is why this order was
chosen. A crash between (2) and (3) leaves a pid record naming a now-dead pid and no
socket: a client's attach to the recorded socket fails fast (the bounded attach from
Fix A, never a hang), and the next start clears the record through
`clear_stale_daemon_lock` / `clear_stale_daemon_socket` because the recorded pid is
provably not alive — self-healing residue. The old order's failure mode was the
opposite and unrecoverable: a LIVE daemon with no socket, which nothing heals.

Tests. A narrow, crate-local seam `cleanup_owned_rendezvous_with(..., checkpoint)`
exposes the two boundaries (`OwnershipCorroborated`, `SocketRemoved`); production
`cleanup_owned_rendezvous` passes a no-op, so no production behavior is conditional
on a test. `a_replacement_start_at_the_cleanup_midpoint_keeps_its_own_rendezvous`
drives a REAL `try_acquire_daemon_lock` from inside the callback at the former
vulnerable midpoint — ordered by the call itself, with no sleep — and asserts the
competing start is refused with `Taken { existing.pid == departing }`, that the
socket is removed before the record, that the record is removed last, and that a
second cleanup pass by the departed owner is refused after the replacement rebinds.
`a_crash_between_cleanup_boundaries_leaves_only_self_healing_residue` asserts at the
`SocketRemoved` boundary that the socket is gone while the record is STILL published,
then proves `clear_stale_daemon_socket` clears that residue. The pre-existing
`cleanup_owned_rendezvous_preserves_a_replacement_owners_record_and_socket`
(mismatch-before-entry) is unchanged.

Negative control, executed: reinstating the exact old pid-record-first body turns
BOTH new tests red and nothing else — `a_replacement_start_...` fails with "a
competing start must NOT be able to claim the record before the departing owner has
finished unlinking its socket" (the competing `try_acquire` returned `Acquired`), and
`a_crash_between_...` fails with "the record is STILL published as exclusion at this
boundary". The corrected bytes were then restored exactly (`grep -c MUTANT` → 0) and
the suite re-run 16/16.

Verification, with `bash scripts/check-workspace-versions.sh` before every Cargo batch
and `--locked` on every Cargo command: `codegraph-daemon` **100 unit + every
integration suite green** (11 `test result: ok`, rc=0), `codegraph-rs`
`batch_m_daemon_uninit_lifecycle` **3/3**, `cargo clippy --locked -p codegraph-daemon
--all-targets -- -D warnings` clean, and the final authoritative
`make ci CARGO='cargo --locked'` over these exact bytes.

Limitations. Changed-file LSP diagnostics were attempted on
`crates/codegraph-daemon/src/lock.rs` and refused again with
`LSP file path must be inside request cwd` (external worktree); locked Cargo
check/Clippy/tests are the fallback and no LSP-clean result is claimed. The race and
its fix are exercised on the Unix filesystem-socket arm, which is where a socket path
can be unlinked at all; Windows uses a namespaced pipe with no filesystem entry, so
the `#[cfg(not(unix))]` arm removes nothing and only the record ordering applies —
native Windows runtime was NOT executed and is not claimed. No dependency, version,
`Cargo.lock`, schema, node-ID, golden, state-protocol, or permanent-index-lifecycle
byte changed; `Cargo.lock` remains SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` and the frozen
plan is untouched at SHA-256
`5b64aa335fb32cd228d98404c2e44153e9134d26a912ecb02d71fcf5c5798450`.

### Same correction pass — the startup gate refused a killed daemon's live sidecars

Verifying the cleanup fix surfaced a SECOND, independent defect introduced by
`df86a5a`'s fifth correction. That correction routed `authorize_daemon_startup`
through `Store::open_for_read`, whose `Current` contract demands a sidecar-FREE
database. A daemon that is KILLED rather than closed leaves `-wal`/`-shm` behind on
an otherwise untouched `Current` namespace, so every replacement start then failed
with `Current index state has an unexpected SQLite sidecar at …/codegraph.db-wal`
and published no rendezvous — exactly the recovery path `spawn_detached_daemon`
exists for.

This was NOT found by reading code. `make ci CARGO='cargo --locked'` failed on the
pre-existing `spawn_detached_daemon_twice_no_stale_deadlock` (kill the first daemon,
clear its stale artifacts, respawn) with "second daemon pid after respawn"; repeated
`cargo test --locked -p codegraph-rs --test daemon_spawn` reproduced it in 2 of 6
runs, and a hand-built repro confirmed the refusal message directly from a planted
`codegraph.db-wal`. The intermittency is why it was mistaken for load flake earlier
in this session: the sidecars survive only when the SIGKILL lands while a connection
is open, so the same test passes whenever the kill happens to land between
connections. That earlier "known flake" note was wrong, and this ledger corrects it.

Fix: `Store::open_for_daemon_startup` — `open_for_read` with `allow_live_sidecar`,
the SAME relaxation the incremental-sync writer and the uninit continuation already
take, and for the same reason (the extraction stamp is read from the
already-checkpointed MAIN-file bytes, so nothing about the version gate changes).
Every other gate is untouched: owner-bound state slots, tombstone absence, DB
presence, the exact stamp, and the retained shared lease held across pid/socket
publication.

New acceptance `a_killed_daemons_live_sidecars_do_not_block_a_replacement_start`
plants both sidecars on a real indexed project, proves a replacement daemon
publishes its v2 rendezvous, then — with those same sidecars still present — drains
it via `uninit --force` and proves the now-tombstoned namespace is STILL refused.
That second half is what shows the relaxation did not weaken the state contract.
Negative control, executed: reverting the single call site to `Store::open_for_read`
turns exactly this test red ("the daemon never published its v2 rendezvous at
…/.codegraph-v2/daemon.pid") while the other three acceptances stay green; the
corrected byte was then restored and the suite re-run 4/4. Repeated
`--test daemon_spawn` is now 8/8 green where it was 4/6.

### Correction — the sidecar relaxation was REVERTED, and doing so re-exposed a real defect

The section immediately above is superseded on the SCOPE question and CORRECTED on the
evidence question. Two separate things happened in `f691415`, and only one was
authorized.

**Retained in full: the rendezvous-cleanup ownership-ordering fix.** `record_names`,
`RendezvousCleanupCheckpoint`, `cleanup_owned_rendezvous_with`, the
socket-before-record ordering, the second-boundary re-corroboration, and both
`a_replacement_start_at_the_cleanup_midpoint_keeps_its_own_rendezvous` and
`a_crash_between_cleanup_boundaries_leaves_only_self_healing_residue`.
`crates/codegraph-daemon/src/lock.rs` is byte-identical to its `f691415` content,
SHA-256 `a14583b40c8695318f07552fc9633622fd4356581bec7c5ad16cfe89203814fe`.

Negative control, re-executed over these exact bytes. Reinstating the old
pid-record-first body — `cleanup_owned_lock` called BEFORE the socket unlink and before
the `OwnershipCorroborated` checkpoint, with no second-boundary re-corroboration —
turned BOTH retained tests red in one run:
`a_replacement_start_at_the_cleanup_midpoint_keeps_its_own_rendezvous` with "a competing
start must NOT be able to claim the record before the departing owner has finished
unlinking its socket" (`lock.rs:617`), and
`a_crash_between_cleanup_boundaries_leaves_only_self_healing_residue` with "the record
is STILL published as exclusion at this boundary" (`lock.rs:673`) — 98 passed, 2 failed.

Worth recording precisely, because it shows the two tests guard different bytes: an
INTERMEDIATE mutant that released the record early but left the `OwnershipCorroborated`
checkpoint above it turned only the crash test red (99 passed, 1 failed) while the
midpoint test still passed, since the competing start then ran before the release. Only
the faithful old ordering — release first, checkpoint after — reproduces the original
race the midpoint test was written for. The corrected bytes were then restored via
`git checkout --` (`grep -c MUTANT` → 0, file SHA-256 back to
`a14583b40c8695318f07552fc9633622fd4356581bec7c5ad16cfe89203814fe`) and
`codegraph-daemon --lib` re-run **100/100**.

**Reverted as out of scope: the daemon-startup SQLite sidecar relaxation.**
`Store::open_for_daemon_startup` and its private `open_for_read_with_sidecar_policy`
helper are removed from `crates/codegraph-store/src/connection.rs`;
`authorize_daemon_startup` calls `Store::open_for_read` again and its doc comment
again describes the full `Current` contract INCLUDING sidecar-freedom, exactly as at
`df86a5a`; and the acceptance
`a_killed_daemons_live_sidecars_do_not_block_a_replacement_start` is removed.
`git diff df86a5a..HEAD` is empty for both `connection.rs` and `main.rs`, and
`batch_m_daemon_uninit_lifecycle` is back to its original 3 tests, **3/3** green.

The scope objection is upheld: relaxing a state/artifact contract that was verified at
`df86a5a` is its own change with its own evidence, and it does not ride along inside a
cleanup-ordering fix. The objection to the removed test also stands on its own terms —
it planted two ZERO-BYTE sidecars, which is not what a killed daemon actually leaves,
so it did not prove the relaxation was necessary.

**But the underlying defect is REAL, and this pass measured it directly.** It was
expected to stay open and unproven; it did not. Measurements taken on this Linux host
over the reverted bytes:

- The PRE-EXISTING acceptance `spawn_detached_daemon_twice_no_stale_deadlock` (kill the
  first daemon, remove its stale pid/socket, respawn) fails **3 of 10** locked runs at
  the default test-thread count, always on the same assertion, `second daemon pid after
respawn` at `crates/codegraph-cli/tests/daemon_spawn.rs:227`. Two independent batches
  of 10 gave 3 failures each (runs 3/5/7, then runs 1/2/5).
- A 0.03 s-interval watcher captured the failing runs' own daemon log verbatim:
  `Error: running as detached MCP daemon: refusing to start a daemon for
/tmp/codegraph-daemon-spawn-twice-…/mini: Current index state has an unexpected SQLite
sidecar at …/.codegraph-v2/codegraph.db-wal; index state is current and its
uninitialized tombstone is absent. No daemon pid, socket, or log was published. Run
`codegraph init` to rebuild.`
- The same watcher captured the residue those runs actually left behind:
  `codegraph.db-shm` at **32768 bytes** — non-empty, a live shared-memory header — and
  `codegraph.db-wal` at 0 bytes. Contrast run: a PASSING run's namespace held
  `codegraph.db` alone, no sidecars. So the failures correlate exactly with surviving
  sidecars, and the residue is a real hard-killed daemon's, not a planted placeholder.
- The intermittency is timing, not load. With `-- --test-threads=1` the same suite ran
  **12 of 12** green; the failures only appear when the two tests in the file run
  concurrently and the SIGKILL lands while a connection is open. A standalone
  `serve --mcp` + `kill -9` loop with no client attached left NO sidecars in 10 of 10
  iterations, which is why this was mistaken for load flake earlier. That earlier
  "known flake" note was wrong, and it stays corrected.

So the answer to "does a hard-killed daemon block a replacement start" is **YES,
intermittently** — when the kill lands while a connection is open, the sidecars survive
and the strict `Current` gate refuses every subsequent start until `codegraph init` is
re-run.

**This defect is PRE-EXISTING, not introduced by this revert — measured, not argued.**
`daemon_spawn.rs`, `connection.rs`, and `main.rs` are all byte-identical to `df86a5a`,
and the retained `lock.rs` cleanup ordering is not on this path (the test removes the pid
record and socket itself, and the refusal happens in the store gate before any
rendezvous is published). To close that off by measurement rather than reasoning,
`lock.rs` was temporarily swapped to its `df86a5a` content (SHA-256
`a25868072cfcf31477bad9d77376edda27f4727eae2f76a2eb3e00120463b35e`) — i.e. the tree with
the retained fix REMOVED — and the same suite still failed **1 of 10** locked runs on the
identical assertion at `daemon_spawn.rs:227`. The retained fix was then restored and
`lock.rs` re-proved byte-identical to `f691415` (`a14583b4…`, empty
`git diff f691415 -- crates/codegraph-daemon/src/lock.rs`). So the flake exists with and
without the cleanup fix; the sidecar gate is its cause, and this pass neither introduced
nor cured it.

**Consequence, stated plainly rather than papered over:** over the reverted bytes,
`make ci CARGO='cargo --locked'` is NOT reliably green — it fails whenever
`spawn_detached_daemon_twice_no_stale_deadlock` hits the ~3-in-10 timing window. The
reverted tree is honest about scope and carries a pre-existing intermittent red. No
test was weakened, skipped, slept, or deleted to hide this, and no relaxation was
re-introduced under another name.

What remains genuinely OPEN is the FIX, not the defect: whether the right answer is a
sidecar-tolerant startup read, a checkpoint-and-clear on startup, or a narrower
recovery path is undecided and belongs to its own plan item with its own failing-first
evidence. The measurements above are that item's starting evidence, and they replace
the zero-byte-placeholder test that was removed.

Verification of this correction, with `bash scripts/check-workspace-versions.sh` before
every Cargo batch (exit 0, workspace 0.40.4 across all 10 packages) and `--locked` on
every Cargo command: `codegraph-daemon --lib` **100/100** including both retained cleanup
tests; `codegraph-rs --test batch_m_daemon_uninit_lifecycle` **3/3** (its original three);
`codegraph-rs --test daemon_spawn` **2/2** on a single run and **12/12 runs** green at
`--test-threads=1`, but **3 of 10 runs** red at the default thread count for the
pre-existing sidecar reason measured above.

The final gate, `bash scripts/check-workspace-versions.sh && make ci CARGO='cargo
--locked'` over these exact bytes, was run **four** times: **exit 2, exit 0, exit 0,
exit 2**. Both failures are the same single assertion, `daemon_spawn.rs:227 second daemon
pid after respawn`; `fmt --check`, `clippy --workspace --all-targets -D warnings`, and
`scripts/guardrail.sh` passed every time, and 76 other test-binary result lines were `ok`
in the failing runs too. So this tree is **NOT reliably green**, and that is reported as a
fact rather than smoothed over by re-running until a green appears: the gate is a coin
flip on a pre-existing defect that predates both this correction and `f691415`.

One further confirmation run of that same gate was executed after this ledger text was
finalized, so that the last gate run covers these exact committed bytes; its exit code is
reported in the closeout summary rather than restated here, since no code byte differs
between it and the four runs above.

Limitations. Changed-file LSP diagnostics were attempted on BOTH
`crates/codegraph-store/src/connection.rs` and `crates/codegraph-cli/src/main.rs` and
both were refused with `LSP file path must be inside request cwd` — the worktree at
`/config/workspace/ProdDir/AI/.cgworktrees/v15-impl` is still outside the request cwd,
even though it was expected to be inside it — so no LSP-clean result is claimed; locked
Cargo build/Clippy/tests are the fallback. The sidecar measurements are Linux-only; native
Windows/MSVC runtime is unavailable on this host and nothing about it is claimed. No
dependency, version, `Cargo.lock`, schema, node-ID, golden, state-protocol, or
permanent-index-lifecycle byte changed; `Cargo.lock` remains SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

## The killed-daemon sidecar defect — RECOVERY, not relaxation (2026-07-28)

This closes the item the section above left explicitly OPEN: "whether the right answer is
a sidecar-tolerant startup read, a checkpoint-and-clear on startup, or a narrower
recovery path is undecided". The answer is checkpoint-and-clear under proven-dead
ownership. The strict read gate is unchanged.

### Confirmed Red, and a correction to the measured failure rate

The orchestrator recorded 7 PASS / 1 FAIL over 8 locked runs of
`spawn_detached_daemon_twice_no_stale_deadlock`. That was re-measured on this host before
any code changed and **did NOT reproduce: 28 of 28 consecutive locked runs passed**
(`cargo test --locked -p codegraph-rs --test daemon_spawn
spawn_detached_daemon_twice_no_stale_deadlock -- --exact`, exit 0 each time). Running one
test by name serializes what the earlier whole-file runs raced, and the residue survives
only when the SIGKILL lands while a connection is open — so the earlier 3-in-10 and
1-in-8 figures and this 0-in-28 are all consistent, and the honest statement is that the
end-to-end test is an UNRELIABLE detector of this defect, not that the defect is absent.

So the defect was instead confirmed DIRECTLY against the production daemon-start path,
which is deterministic. A real `codegraph init` project was given a genuinely
un-checkpointed log by a child process that opened the DB, set `wal_autocheckpoint=0`,
committed one `project_metadata` row, and died without closing SQLite — leaving
`codegraph.db-wal` at **8272 bytes** and `codegraph.db-shm` at **32768 bytes**. Running the
real gate over it (`CODEGRAPH_DAEMON_INTERNAL=1 codegraph serve --mcp --path …`) refused
verbatim:

`Error: running as detached MCP daemon: refusing to start a daemon for …: Current index
state has an unexpected SQLite sidecar at …/.codegraph-v2/codegraph.db-wal; index state is
current and its uninitialized tombstone is absent. No daemon pid, socket, or log was
published. Run `codegraph init` to rebuild.`

That is the same refusal, reproduced 100% of the time, from residue that is a real dead
writer's rather than a zero-byte placeholder.

### Root cause

`Store::open_for_read`'s `Current` contract requires sidecar-freedom, which is correct as
a READ contract — the rebuild finalizer checkpoints and closes before publishing
`Current`, so a reappeared `-wal` means something wrote outside the state protocol. But a
SIGKILLed daemon's residue is indistinguishable from that at the artifact level, and the
gate offered no way back: every later start was refused until `codegraph init` rebuilt the
namespace, discarding the whole index over a recoverable log.

### The fix, and why recovery beat relaxing the read gate

`f691415` relaxed the gate (`open_for_daemon_startup` = read + `allow_live_sidecar`).
That makes the daemon read a namespace whose main-file bytes are NOT the whole truth: the
`-wal` may hold committed pages the deserialized main-file image cannot see, so the daemon
would serve a silently stale graph, and the residue would persist indefinitely. Recovery
instead makes the artifact shape match the contract, so the daemon reads through the
identical strict path afterwards and the residue is gone for good.

New `Store::recover_stale_current_sidecars(paths, deadline, cancelled) -> Result<bool>`
(`crates/codegraph-store/src/connection.rs`):

1. Refuses unless the state is `Current` and the tombstone is ABSENT — checked before any
   lease, so a tombstoned or non-`Current` namespace is never touched.
2. Reports `Ok(false)` immediately when no `-wal`/`-shm` exists (nothing to recover).
3. Acquires the ONE outer EXCLUSIVE lease via the existing
   `IndexLease::acquire_exclusive_existing`. A timeout or cancellation is `Ok(false)`,
   not an error: a live holder means startup proceeds to its unchanged verdict.
4. Opens through `Store::open_for_write` under a NEW narrow purpose
   `StoreWritePurpose::StaleSidecarRecovery`, which corroborates the full `Current`
   contract (tombstone, DB presence, exact main-file extraction stamp) and tolerates the
   sidecars only because their presence is the reason it was called.
   `StoreWritePurpose::IncrementalSync` was deliberately NOT reused: its semantics are
   "one incremental sync", including escalation to a full rebuild for
   `Missing`/`Outdated`/`Building`, none of which this repair may ever do.
5. Calls the existing `finish_current_mutation` (`wal_checkpoint(TRUNCATE)` then an
   explicit `close`), then `remove_checkpointed_sidecars`, which REFUSES with the new
   `StoreError::WalNotFolded` if `-wal` still carries bytes. A non-empty log is never
   unlinked; `-shm` is removed only after the log is proven folded, because it is derived
   shared memory with no durable content. A leftover rollback `-journal` is left alone on
   purpose — the write-capable open is what lets SQLite replay and retire a hot journal,
   and hand-unlinking one discards committed pages.

CLI side (`crates/codegraph-cli/src/main.rs`), before the UNCHANGED gate:

- `previous_daemon_owner_may_be_live` is a READ-ONLY liveness predicate over the pid
  record. `clear_stale_daemon_lock` was not reused because it REMOVES the record as a side
  effect, and that record is the single-instance exclusion `try_acquire_daemon_lock`
  claims moments later — removing it here would open a double-start window. Fail-closed:
  a missing record is dead, an unreadable or EMPTY record is treated as LIVE (an empty
  record is an in-flight `create_new` placeholder, matching the lock layer).
- `recover_dead_owner_sidecars` runs the repair only when that predicate says dead, and
  swallows any error to a `warn!`; the authoritative verdict is always the gate's.
- `STALE_SIDECAR_RECOVERY_TIMEOUT = 500ms`, deliberately far below the 30s
  `REBUILD_LEASE_TIMEOUT`. This is a best-effort repair on the latency-critical startup
  path, and the only legitimate reason the exclusive lease is unavailable is that a live
  cooperating holder owns the namespace — including a long-lived direct-mode MCP reader,
  which holds a shared lease for its whole server lifetime and therefore blocks exclusive
  acquisition on its own. In that case there is nothing to recover, so startup must reach
  its normal verdict in half a second rather than stall for 30.
- `crates/codegraph-daemon/src/lock.rs` is byte-for-byte untouched (SHA-256
  `a14583b40c8695318f07552fc9633622fd4356581bec7c5ad16cfe89203814fe`), and nothing here
  signals or kills any pid.

`grep -rn "open_for_daemon_startup\|open_for_read_with_sidecar_policy" crates/` and
`grep -rn "a_killed_daemons_live_sidecars" crates/` both return nothing (exit 1):
`8c66848`'s revert stands, and no sidecar-tolerant READ path exists under any name.

### New tests and the negative control

`crates/codegraph-cli/tests/daemon_stale_wal_recovery.rs`:

- `a_dead_owners_uncheckpointed_wal_is_recovered_on_daemon_startup` re-invokes the test
  binary as a child that commits a row with `wal_autocheckpoint=0` and then `abort()`s, so
  the residue is a REAL dead writer's. It asserts the `-wal` is NON-EMPTY before startup
  and that the probe row is absent from a sidecar-free COPY of the main file, then drives
  the production `spawn_detached_daemon` path and requires a published rendezvous. After
  startup it re-reads that main-file-only copy and requires the probe row to be
  PRESENT — proving the log was folded in, not discarded — and asserts the permanent lock
  survives and the state is still `Current`.
- `the_same_stale_residue_under_a_tombstone_is_still_refused` is the paired fail-closed
  control: identical non-empty residue plus the tombstone still refuses, folds nothing
  (the `-wal` is still non-empty afterwards), and publishes neither pid nor socket.

Negative control, executed: the single `recover_dead_owner_sidecars` call site was
disabled and the recovery test went RED on the real refusal text
(`… unexpected SQLite sidecar at …/codegraph.db-wal`) while the tombstone control stayed
green — so the test genuinely exercises the production path, not a helper. The call site
was restored and `crates/codegraph-cli/src/main.rs` re-proved as SHA-256
`3f745cf88dc30937adacf23341674974009a68861db016707fde1932e1736acf`, the pre-mutation
hash.

### Verification

`bash scripts/check-workspace-versions.sh` was run before every Cargo batch (exit 0,
workspace 0.40.4 across all 10 packages) and `--locked` was passed to every Cargo command.

- `spawn_detached_daemon_twice_no_stale_deadlock`: **20 of 20** consecutive locked runs
  green, zero failures (plus the 28 pre-change runs noted above).
- `--test daemon_stale_wal_recovery`: 3/3 (both acceptances plus the child-process
  helper).
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: exit 0.

**Correction to the previous section's `make ci` statement.** That section said
`make ci CARGO='cargo --locked'` is NOT reliably green over the reverted bytes, and it was
right at `8c66848`: the sidecar defect was unfixed and the end-to-end test could hit it.
That record stands as written for those bytes. Over THESE bytes the cause is removed —
the daemon now recovers the residue instead of refusing forever — so the final gate's
measured result is reported in the closeout summary for these exact committed bytes rather
than inherited from that earlier measurement.

Limitations. Changed-file LSP diagnostics were attempted on
`crates/codegraph-store/src/connection.rs` and
`crates/codegraph-cli/tests/daemon_stale_wal_recovery.rs`; both were refused with `LSP
file path must be inside request cwd` (the worktree at
`/config/workspace/ProdDir/AI/.cgworktrees/v15-impl` is outside the request cwd), so no
LSP-clean result is claimed and locked Cargo build/Clippy/tests are the fallback. The new
test file is `#![cfg(unix)]`: the child-abort WAL plant and the socket handshake are
Unix-specific. On Windows the recovery itself is portable by inspection — it uses only
`IndexLease`, SQLite pragmas, and `std::fs::remove_file`, and the sidecar paths are built
with `OsString::push` so wide paths stay lossless — but native Windows/MSVC runtime is
unavailable on this host and no Windows runtime claim is made. The remaining gap is that
`spawn_detached_daemon_twice_no_stale_deadlock` stays a probabilistic detector of this
defect; the deterministic coverage is the new file.

A SEPARATE pre-existing intermittent failure was found while running the final gate and is
recorded rather than left implicit: `make ci` failed once on
`formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home`
(`crates/codegraph-cli/src/main.rs:6209`, `assertion failed: elv.is_file()`).
`cargo test --locked -p codegraph-rs --bin codegraph` reproduced it in **2 of 5** runs with
these changes applied and in **4 of 8** runs with them fully STASHED away, i.e. on the
untouched `8c66848` tree — so it is pre-existing and unrelated. It is an in-process
`HOME`/`XDG_DATA_HOME` env race across `#[test]`s in the same binary; nothing here touches
that code path, and no test was weakened, skipped, slept, or deleted over it. It is a
known open item, not something this pass introduced or cured.

No dependency, version, `Cargo.lock`,
schema, node-ID, golden, state-protocol, or permanent-index-lifecycle byte changed;
`Cargo.lock` remains SHA-256
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

## Batch B1 — C++ explicit operator calls resolve to the operator method (2026-07-28)

Ports upstream `6103f5e` (`fix(cpp): resolve explicit operator calls
(a.operator+(b)) to the operator method`, upstream #1268 / issue #1247). Behavior
only — the TypeScript regex/AST shapes were re-derived against
`tree-sitter-cpp 0.23.4` in this workspace, not transliterated.

### Red (documented, with the actual wrong output)

Five new `#[test]`s in `crates/codegraph-extract/src/walker.rs` drive the REAL
extraction path (`extract_source` → `Walker::extract_call`) and failed on the
pre-change bytes. `cargo test --locked -p codegraph-extract --lib
cpp_explicit_operator` → **5 failed, 1 passed** with these observed values:

| assertion                                                 | expected              | ACTUAL on pre-change bytes |
| --------------------------------------------------------- | --------------------- | -------------------------- |
| `a.operator+(b)`                                          | ref `a.operator+`     | `["a"]`                    |
| `p->operator+(b)`                                         | ref `p.operator+`     | `["p"]`                    |
| `a.operator[](3)` / `a.operator()(1)` / `a.operator==(b)` | 3 operator refs       | `["a", "a", "a"]`          |
| `a.operator == (b)` / `a.operator [] (3)` (spaced)        | compact operator refs | `["a", "a"]`               |
| `this->operator+(*this)`                                  | bare ref `operator+`  | `["this"]`                 |

The bare receiver name is exactly upstream's reported defect: the callee is read
from the `function` field, but tree-sitter-cpp cannot parse an `operator_name` in
field position, so the `operator_name` is stranded in an ERROR sibling and the
`function` field holds only the receiver. `cpp_explicit_operator_call_drops_complex_receiver`
passed pre-change for the WRONG reason (no operator ref existed at all), so it is
a guard, not a Red.

Two resolver `#[test]`s in `crates/codegraph-resolve/src/name_matcher.rs` were the
second Red: `cpp_operator_dot_shape_is_a_method_call_shape` failed with
`parse_method_call("a.operator+", Cpp)` returning **`None`** (expected
`Some(("a","operator+"))`) — the operator's symbol chars fail `match_dot_call`'s
selector-like method part — and
`cpp_explicit_operator_call_resolves_via_receiver_type` failed at
`.expect("explicit operator call resolves")`, i.e. `match_method_call` returned
`None` even with the recovered name.

### Green (minimal)

- `crates/codegraph-extract/src/lang/cpp.rs`: new
  `recover_explicit_operator_call` returning `ExplicitOperatorCall::{Callee,Drop}`.
  It finds an `operator_name` inside an ERROR child of the `call_expression`,
  compacts a spaced SYMBOLIC name (`operator ==` → `operator==`, word forms like
  `operator new` keep their space), normalizes `->` to `.`, emits the bare
  operator name for a `this` receiver, and returns `Drop` for a receiver that is
  not a simple identifier/member chain (`w.obj()->operator+`) so exact-name
  matching cannot guess among unrelated same-named operators.
- `crates/codegraph-extract/src/walker.rs`: `extract_call` consults it first, for
  `Language::Cpp` only.
- `crates/codegraph-resolve/src/name_matcher.rs`: new
  `match_cpp_operator_dot_call` admitted by `parse_method_call` and
  `is_inferable_receiver_call` for `Language::{C,Cpp}` AFTER the plain dotted
  pattern, so `a.operatorTable` is unaffected and `a.operator` (no symbol) stays
  an ordinary dotted call.

### Golden row delta (`reference/golden/cpp/`, additions only)

New fixture `crates/codegraph-bench/fixtures/cpp/operators.cpp` (struct `Vec2`
with `operator+`/`operator[]`/`get`, three explicit-operator call sites, one
plain-member control). `git diff --numstat reference/golden/` showed ONLY `cpp/`
paths: `files.json +8/-0`, `nodes.json +198/-0`, `edges.json +120/-0`,
`colby.db` binary; `refs.json` and `schema.sql` byte-identical. Deleted-line
count across the three JSON files: **0** — every pre-existing declaration keeps
its node ID and row.

Added rows: 9 `operators.cpp` nodes (file, `struct Vec2`, methods
`Vec2::operator+` @3 / `Vec2::operator[]` @4 / `Vec2::get` @5, functions
`explicit_operator_call` @8 / `explicit_subscript_call` @12 /
`explicit_pointer_operator_call` @16 / `plain_member_call` @20) and 4 `Calls`
edges, all `resolvedBy=instance-method` at confidence 0.9:
`explicit_operator_call → Vec2::operator+`,
`explicit_pointer_operator_call → Vec2::operator+`,
`explicit_subscript_call → Vec2::operator[]`, and the control
`plain_member_call → Vec2::get`. Before this change the three operator call sites
produced an unresolved `a`/`p` ref and NO edge.

Recipe fidelity: `docs/equivalence.md` says `cp
/tmp/cg-fixture-cpp/.codegraph/codegraph.db`, but Batch M moved the index to the
isolated v2 namespace, so the database is actually written to
`/tmp/cg-fixture-cpp/.codegraph-v2/codegraph.db`. That path substitution is the
ONLY deviation, and it was proven inert: re-running the documented recipe over
the five PRE-EXISTING fixture files alone regenerated `nodes.json`, `edges.json`,
`refs.json`, `files.json` and `schema.sql` byte-identical to the committed golden
(`cmp -s` on all five → IDENTICAL) before `operators.cpp` was added.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0 (workspace 0.40.4, 10
  packages), run before every Cargo batch; every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-extract --lib cpp_` → exit 0, **56 passed, 0
  failed** (includes the 5 new Reds now green plus the pre-existing
  `cpp_operator_lt_call_not_stripped`, `cpp_template_arg_call_strips_to_base`,
  `strip_cpp_template_args_cases`, UE/export-macro and namespace tests).
- `cargo test --locked -p codegraph-resolve --lib -- cpp_explicit_operator
cpp_operator_dot` → exit 0, **2 passed**.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, **26
  passed, 0 failed**, including `generated_golden_matches_committed_cpp_fixture`
  and `cpp_db_is_self_equivalent_to_cpp_golden`, and every non-cpp oracle
  (`godot`, `ruby`, `mini`, `metal`, `cuda`, `arkts`, `solidity`, `nix`,
  `terraform`, `erlang`, `cfml`).
- `git diff --stat reference/golden/` → only `cpp/` paths; godot, ruby and the
  original upstream corpus unchanged.

`lsp_diagnostics` was NOT usable here: it has consistently refused this worktree
with `LSP file path must be inside request cwd` (the worktree at
`/config/workspace/ProdDir/AI/.cgworktrees/v15-impl` is outside the request cwd),
so locked Cargo build/Clippy/test is the honest fallback and no LSP-clean result
is claimed. No dependency, version or `Cargo.lock` byte changed.

## Batch B2 — strip template args from out-of-line method receiver qualifiers (2026-07-28)

Ports upstream `4dd29ea` (`fix(cpp): strip template args from out-of-line method
receiver qualifiers`, upstream #1309 / issue #1286). Behavior only.

### Red (documented, with the actual wrong output)

Three new `#[test]`s in `crates/codegraph-extract/src/walker.rs` drive the REAL
extraction path (`extract_source` → `Walker::extract_method` →
`CppSpec::get_receiver_type`). `cargo test --locked -p codegraph-extract --lib --
cpp_out_of_line cpp_multiline_template` → **3 failed, 0 passed**:

| assertion                                            | expected                   | ACTUAL on pre-change bytes                                                                                                                   |
| ---------------------------------------------------- | -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `template <typename T> T Box<T>::get()` + `set`      | `["Box::get", "Box::set"]` | `["Box<T>::get", "Box<T>::set"]`                                                                                                             |
| out-of-line method shares its class node's qualifier | `Box::get`                 | `Box<T>::get` (class node is `Box`)                                                                                                          |
| ICU-shaped multi-line parameter list                 | `ApiHelper::validate`      | `"ApiHelper<CType,\n                   CPPType,\n                   kSentinelConstantForTheHelperTemplateClassInstanceGuardLong>::validate"` |

The third Red is the NAME_MAX shape upstream reports: the qualified name carried
embedded newlines and the whole `<…>` block.

### Green (minimal)

`CppSpec::get_receiver_type` (`crates/codegraph-extract/src/lang/cpp.rs`) now runs
the receiver qualifier through `strip_cpp_template_args` — the SAME normalization
#1043 already applies to base-class refs — and returns `None` when stripping
leaves nothing. `strip_cpp_template_args` was widened from private to
`pub(crate)` in `walker.rs`; its body is unchanged, so the #1043 templated-base
behavior is bit-identical.

### Negative control, executed

The single new call was reverted in place
(`strip_cpp_template_args(&parts[..len-1].join("::"))` → `parts[..len-1].join("::")`)
and all three tests went RED again on the same wrong values; the file was then
restored and re-proved as SHA-256
`42fcc019fe88ce2178a7ace9eec32e53778d65bfdddeeda2afbd21e16ff35a43`, the
pre-mutation hash. The tests therefore exercise the production path, not a helper
call.

### Golden row delta (`reference/golden/cpp/`, additions only)

New fixture `crates/codegraph-bench/fixtures/cpp/template_method.cpp`
(`template <typename T> class Box` with `get`/`set` declared inline and defined
out-of-line). `git diff --numstat reference/golden/` showed ONLY `cpp/` paths:
`files.json +8/-0`, `nodes.json +88/-0`, `edges.json +45/-0`, `colby.db` binary;
`refs.json` and `schema.sql` byte-identical (`git diff --stat` on both is empty).
Deleted-line count across the three JSON files: **0**.

Added rows: 4 `template_method.cpp` nodes (file, `class Box` @2, `method Box::get`
@12, `method Box::set` @17) and 5 `contains` edges — `file → class Box`,
`file → Box::get`, `file → Box::set`, and critically `class Box → Box::get` /
`class Box → Box::set`. Those last two are the fix: with a `Box<T>::` qualifier
the out-of-line definitions never matched the `Box` class node, so the
class→method containment did not exist.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo build --locked --release -p codegraph-rs` → exit 0 (golden regeneration
  binary).
- `cargo test --locked -p codegraph-extract --lib -- cpp_out_of_line
cpp_multiline_template` → exit 0, **3 passed**.
- `cargo test --locked -p codegraph-extract -p codegraph-resolve -p
codegraph-bench` → exit 0, **1382 passed across 35 test binaries, 0 failed**,
  including `generated_golden_matches_committed_cpp_fixture` and the untouched
  `..._godot_fixture` / `..._ruby_fixture` / `..._mini_fixture` oracles.
- `git diff --numstat reference/golden/` → only `cpp/` paths; godot, ruby and the
  original upstream corpus unchanged.

`lsp_diagnostics` remains unusable in this worktree (`LSP file path must be inside
request cwd`), so locked Cargo is the fallback; no LSP-clean claim is made. No
dependency, version or `Cargo.lock` byte changed.

## Batch B3 — compose namespace prefix into out-of-line method qualified names (2026-07-28)

Ports upstream `e437918` (`fix(cpp): compose namespace prefix into out-of-line
method qualified names`, upstream #1310 / issue #1291). Behavior only.

### Red (documented, with the actual wrong output)

Two new C++ `#[test]`s plus one cross-language control in
`crates/codegraph-extract/src/walker.rs` drive the REAL extraction path
(`extract_source` → `Walker::extract_method` → the `receiver_type` →
`qualified_name` composition). `cargo test --locked -p codegraph-extract --lib --
cpp_out_of_line_method_in_namespace cpp_receiver_that_respells
go_receiver_method_qualified` → **2 failed, 1 passed**:

| assertion                                                                  | expected                                      | ACTUAL on pre-change bytes                                                                                        |
| -------------------------------------------------------------------------- | --------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `namespace simulator { … ManifestStartup::Apply(...) {} }`                 | `simulator::ManifestStartup::Apply`           | `["ManifestStartup::Apply"]` — the class node meanwhile carried `simulator::ManifestStartup`, so the two DIVERGED |
| `namespace sim { void sim::M::f(){} void M::g(){} }` + global `sim::M::f2` | all of `sim::M::f`, `sim::M::g`, `sim::M::f2` | `["sim::M::f", "M::g", "sim::M::f2"]` — the RELATIVE form `M::g` lost its namespace                               |

`go_receiver_method_qualified_name_unaffected_by_namespace_composition` (Go
`func (s *Server) Start()` → `Server::Start`) passed pre-change and is the
cross-language non-regression control, not a Red.

### Green (minimal)

New `Walker::compose_receiver_qualified_name(receiver, name)` in
`crates/codegraph-extract/src/walker.rs`, called from the ONE
`qualified_name: receiver_type.map(...)` site in `extract_method`. It prepends the
active `namespace_prefix`, anchored at the first prefix segment the receiver
re-spells, so `namespace sim { void sim::M::f() {} }` yields `sim::M::f` and never
`sim::sim::M::f`. `namespace_prefix` is pushed only by `visit_cpp_node`, so it is
empty for every other language and Go/Rust/Kotlin/Lua receivers pass through the
identical `{receiver}::{name}` string.

### Negative control, executed

The single call site was reverted in place to the old
`receiver_type.map(|receiver| format!("{receiver}::{name}"))` and BOTH C++ tests
went RED again on the same wrong values while the Go control stayed green; the
file was then restored and re-proved as SHA-256
`2b7e3601743ed319d09a61a73f05246d5a17fa4b3b269bf389e190d0428990ab`, the
pre-mutation hash.

### Golden row delta (`reference/golden/cpp/`, additions only)

Two new fixture files —
`crates/codegraph-bench/fixtures/cpp/namespaced_member.hpp` (the namespaced class
declaration) and `namespaced_member.cpp` (the out-of-line definition inside the
same namespace block plus a fully-qualified call from OUTSIDE it). `git diff
--numstat reference/golden/` showed ONLY `cpp/` paths: `files.json +16/-0`,
`nodes.json +132/-0`, `edges.json +60/-0`, `colby.db` binary; `refs.json` and
`schema.sql` byte-identical. Deleted-line count across the three JSON files:
**0**.

Added rows: 6 nodes — `file namespaced_member.hpp`, `class
simulator::ManifestStartup` @4; `file namespaced_member.cpp`, its
`import namespaced_member.hpp`, `method simulator::ManifestStartup::Apply` @4,
`function run_manifest` @9 — and 6 edges: the `imports` edge (`resolvedBy=import`,
0.92), four `contains` edges, and the decisive
`calls run_manifest → simulator::ManifestStartup::Apply` with
`resolvedBy=qualified-name` at confidence 0.85. Before this change the method
indexed as `ManifestStartup::Apply`, the fully-qualified call site matched
nothing, and that `Calls` edge did not exist.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo build --locked --release -p codegraph-rs` → exit 0.
- `cargo test --locked -p codegraph-extract --lib -- cpp_out_of_line_method_in_namespace
cpp_receiver_that_respells go_receiver_method_qualified` → exit 0, **3 passed**.
- `cargo test --locked -p codegraph-extract -p codegraph-resolve -p
codegraph-bench` → exit 0, **1385 passed across 35 test binaries, 0 failed**,
  including `generated_golden_matches_committed_cpp_fixture`,
  `cpp_db_is_self_equivalent_to_cpp_golden`, and the untouched godot / ruby /
  mini oracles.
- `git diff --numstat reference/golden/` → only `cpp/` paths.

`lsp_diagnostics` remains unusable in this worktree (`LSP file path must be inside
request cwd`); locked Cargo is the fallback. No dependency, version or
`Cargo.lock` byte changed.

## Batch B4 — blank leading C attribute macros so functions index under real names (2026-07-28)

Ports upstream `b6a05d1` (`fix(c): blank leading attribute macros so functions
index under real names`, upstream #1311 / issue #1211). Behavior only.

### Red (documented, with the actual wrong output)

Four new `#[test]`s in `crates/codegraph-extract/src/walker.rs` drive the REAL
extraction path (`extract_source` for `Language::C` → `CSpec::pre_parse` → the
walker). `cargo test --locked -p codegraph-extract --lib -- c_leading_attr_macro
c_isolation_table c_plain_typedef_return` → **3 failed, 1 passed** on the
pre-change bytes:

| assertion                                                    | expected  | ACTUAL on pre-change bytes |
| ------------------------------------------------------------ | --------- | -------------------------- |
| `SEC_ATTR UINT32 Foo (VOID) { … }`                           | `["Foo"]` | `["UINT32"]`               |
| `SEC_ATTR VOID Foo (VOID) { }`                               | `["Foo"]` | `["VOID"]`                 |
| `SEC_ATTR unsigned int f(void)` (control, must be untouched) | `["f"]`   | `["int"]`                  |

The name is the RETURN TYPE, not the function — exactly upstream's defect (their
grammar/spacing lost the name into the parameter list, `"(VOID)"`; this Rust
workspace's `tree-sitter-c 0.24.2` loses it into the return type instead). Either
way the real symbol is unfindable.

Honest scoping note: `c_isolation_table_functions_all_index_under_real_names`
PASSED pre-change and is therefore a guard, not a Red. With `#define`/`typedef`
lines present ahead of them, that particular multi-declaration shape already
recovered the names in this grammar version; it is kept because it pins the whole
issue table (including the `RawAttr` raw-`__attribute__` and `OneNamedArg`
non-void-param rows) against future regressions. The `c_plain_typedef_return...`
control's `SEC_ATTR unsigned int f(void)` row was ALSO red pre-change and is now
green — a real gain, not just a control.

### Green (minimal)

New `blank_c_leading_attr_macros` in `crates/codegraph-extract/src/lang/c.rs`,
called from `CSpec::pre_parse` BEFORE the existing content-gated CUDA blank. It is
structural, not a curated macro list: a line-leading ALL-CAPS token of ≥3 chars
followed by TWO identifier tokens (`*` allowed between them for pointer returns)
and then `(` — the `MACRO Ret name(` definition shape. The macro is replaced with
equal-length ASCII spaces, so byte offsets and therefore all line/column values
are preserved, exactly like the C++ blanks. `MACRO name(` calls, `#define` lines
(they start with `#`, which `^[ \t]*` cannot skip), and mid-line uses are rejected
by construction; a source with no match is returned unchanged.

Four unit tests in `lang/c.rs` pin the blank itself: the definition shape, byte
and newline-count preservation, pointer returns, and five untouched shapes
(plain typedef return, ALL-CAPS call, `#define` line, multi-word builtin return,
mid-line use).

### Negative control, executed

`CSpec::pre_parse` was edited in place to skip the new blank
(`let blanked = source.to_string();`). Both `c_leading_attr_macro_*` tests went RED
again on the same wrong values (`["UINT32"]`, `["VOID"]`) while the four
`blank_c_leading_attr_macro_*` unit tests stayed green — proving those unit tests
alone do NOT cover the production path and the walker tests do. The file was
restored and re-proved as SHA-256
`d4735def134c9fa94d26fb132ec210bd67e2a993883488ee89b32e409a09aae7`, the
pre-mutation hash.

### Golden row delta (`reference/golden/cpp/`, additions only)

New fixture `crates/codegraph-bench/fixtures/cpp/attr_macro.c` — the corpus's
first `.c` file, so it exercises the C walker rather than the C++ one. `git diff
--numstat reference/golden/` showed ONLY `cpp/` paths: `files.json +8/-0`,
`nodes.json +132/-0`, `edges.json +45/-0`, `colby.db` binary; `refs.json` and
`schema.sql` byte-identical. Deleted-line count across the three JSON files:
**0**.

Added rows: the `files.json` entry (`"language": "c"`, `node_count` 6) and 6
nodes — `file attr_macro.c`, `type_alias UINT32` @4, and four functions with
their REAL names and real return types: `GoodName` @6 (`rt=VOID`), `LostName` @9
(`rt=UINT32`), `NoAttr` @13 (`rt=UINT32`, the no-macro control), `PtrRet` @17
(`rt=UINT32`) — plus 5 `contains` edges from the file to each. Before this change
`GoodName` would have indexed as `VOID` and `LostName`/`PtrRet` as `UINT32`,
colliding on the return-type names and losing all three real symbols.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo build --locked --release -p codegraph-rs` → exit 0.
- `cargo test --locked -p codegraph-extract --lib -- blank_c_leading_attr_macro
c_leading_attr_macro c_isolation_table c_plain_typedef_return c_header_cuda
c_plain_untouched` → exit 0, **10 passed** (the pre-existing
  `c_header_cuda_content_blanked` and `c_plain_untouched` prove the new blank did
  not disturb the CUDA path or plain C).
- `cargo test --locked -p codegraph-extract -p codegraph-resolve -p
codegraph-bench` → exit 0, **1393 passed across 35 test binaries, 0 failed**,
  including `generated_golden_matches_committed_cpp_fixture`,
  `cpp_db_is_self_equivalent_to_cpp_golden`, and the untouched godot / ruby / mini
  oracles.
- `git diff --numstat reference/golden/` → only `cpp/` paths.

`lsp_diagnostics` remains unusable in this worktree (`LSP file path must be inside
request cwd`); locked Cargo is the fallback. No dependency, version or
`Cargo.lock` byte changed.

## Batch B4 CORRECTION — the leading-attr-macro blank over-triggered; tightened to require same-file `#define` proof (2026-07-28)

The `e95072c` section ABOVE stands as the record of what was shipped and why it
passed its own tests; it is left intact deliberately. This section corrects it.
Orchestrator review REJECTED `e95072c`: its `blank_c_leading_attr_macros` over-fires
and DAMAGES correct extraction of ordinary C. This is a follow-up commit; `e95072c`
was NOT amended.

### The measured regression (reproduced here, not re-derived)

Probe file `/tmp/cprobe/edk.c` (firmware-flavoured C, NO `#define` in it):

```c
EFI_STATUS EFIAPI DriverEntry (VOID) { return 0; }
CONST CHAR8 *GetName (VOID) { return 0; }
STATIC void helper (void) { }
UINT32 Untouched (void) { return 0; }
```

Method: build `target/debug/codegraph` at each commit's `lang/c.rs`, then
`codegraph init /tmp/cprobe` and
`sqlite3 /tmp/cprobe/.codegraph-v2/codegraph.db "select name, start_line,
coalesce(return_type,'<NULL>') from nodes where kind='function' order by
start_line;"`. Note the DB path is `.codegraph-v2/`, not `.codegraph/`.

| source line                            | parent `288d892` | `e95072c` | verdict              |
| -------------------------------------- | ---------------- | --------- | -------------------- |
| `EFI_STATUS EFIAPI DriverEntry (VOID)` | `EFI_STATUS`     | `EFIAPI`  | **REGRESSION**       |
| `CONST CHAR8 *GetName (VOID)`          | `CONST`          | `CHAR8`   | improvement          |
| `STATIC void helper (void)`            | `STATIC`         | `<NULL>`  | **information lost** |
| `UINT32 Untouched (void)`              | `UINT32`         | `UINT32`  | untouched, correct   |

Raw output, verbatim:

```
===== PARENT 288d892 =====        ===== HEAD e95072c =====
DriverEntry|1|EFI_STATUS          DriverEntry|1|EFIAPI
GetName|5|CONST                   GetName|5|CHAR8
helper|9|STATIC                   helper|9|<NULL>
Untouched|12|UINT32               Untouched|12|UINT32
```

The function NAMES were already correct at the parent in all four rows
(`DriverEntry`, `GetName`, `helper`, `Untouched`), so #1311's "name is lost"
symptom does NOT occur for these shapes at all — `attr_macro.c` only reproduces it
through the specific `VOID`/`UINT32` typedef spacing it was written around. A green
suite proved nothing because the fixture encoded only the author's assumed world.

### Root cause

The regex was purely structural: `(?m)^[ \t]*([A-Z][A-Z0-9_]{2,})\s+[A-Za-z_]\w*[\s*]+[A-Za-z_]\w*\s*\(`
— "line-leading ALL-CAPS token + two identifiers + `(`" ⇒ blank the first token.
That shape ALSO matches `RETURN_TYPE CALLCONV name(` and `KEYWORD_ALIAS Ret name(`,
where the token blanked is the return type itself. `EFI_STATUS` / `UINT32` /
`CHAR8` are typedef'd return types; `STATIC` / `EXTERN` / `INLINE` / `CONST` are
macro aliases for keywords. All are lexically identical to a true attribute macro,
so no all-caps-plus-length heuristic can separate them. The token's SPELLING is not
admissible evidence.

### The tightened rule

`blank_c_leading_attr_macros` now blanks a leading token ONLY when the SAME
translation unit proves it is attribute-like: the file contains an OBJECT-LIKE
`#define TOKEN …` whose replacement text is either EMPTY or contains an attribute
construct (`__attribute__`, `__attribute`, `__declspec`, `__asm`, `__pragma`,
`_Pragma`). New helper `attribute_like_defines(source) -> BTreeSet<&str>` collects
that set (a `BTreeSet` keeps membership order-independent, so extraction stays
deterministic); function-like `#define F(x) …` is rejected because it is never used
as a bare leading token. When the set is empty the function returns the source
unchanged, so the whole pass is inert on any file without such a `#define`.

Unknown token ⇒ do nothing. Never blank on suspicion.

Measured per-row behaviour of the tightened version on the SAME `edk.c` (no
`#define` present, so the pass never activates):

| source line                            | tightened    | reason                                                   |
| -------------------------------------- | ------------ | -------------------------------------------------------- |
| `EFI_STATUS EFIAPI DriverEntry (VOID)` | `EFI_STATUS` | no `#define EFI_STATUS` here ⇒ untouched, matches parent |
| `CONST CHAR8 *GetName (VOID)`          | `CONST`      | no `#define CONST` here ⇒ untouched, matches parent      |
| `STATIC void helper (void)`            | `STATIC`     | no `#define STATIC` here ⇒ untouched, matches parent     |
| `UINT32 Untouched (void)`              | `UINT32`     | never matched the shape anyway                           |

Byte-identical to `288d892` on all four rows — verified by re-running the same
`codegraph init` + `sqlite3` probe with the tightened release binary:

```
===== TIGHTENED, no #define in edk.c =====
DriverEntry|1|EFI_STATUS
GetName|5|CONST
helper|9|STATIC
Untouched|12|UINT32
```

Honest note on `STATIC` and `CONST`: if a file DID carry `#define STATIC static`
or `#define CONST const`, those replacements are neither empty nor attribute
constructs, so `attribute_like_defines` still rejects them and the tokens stay
untouched — pinned by
`attribute_like_defines_rejects_types_keywords_and_function_like_macros`. They are
only ever blanked if a project literally writes `#define STATIC` (empty) or
`#define STATIC __attribute__((…))`, in which case blanking is correct.

#1311's fixture still works because `#define SEC_ATTR
__attribute__((section(".init")))` is visible INSIDE `attr_macro.c`: `SEC_ATTR` is
still blanked and `LostName` still indexes under its real name. Confirmed on the
regenerated fixture DB:

```
attr_macro.c|1|<NULL>      UINT32|4|<NULL>     GoodName|6|VOID
LostName|9|UINT32          NoAttr|13|UINT32    PtrRet|17|UINT32
```

A cross-file probe (`/tmp/cprobe2/hdr_defined.c`, which carries the `SEC_ATTR`
define AND the firmware shapes) shows both behaviours coexisting in one file:
`LostName2` keeps its real name with `rt=UINT32` (macro blanked) while
`DriverEntry2` keeps `EFI_STATUS` and `helper2` keeps `STATIC` (untouched).

### Documented limitation (deliberate under-fix)

`pre_parse` receives `(source, file_path)` and sees ONE file. A macro
`#define SEC_ATTR __attribute__((…))` living in a HEADER and used in a `.c` yields
NO evidence, so that source is left untouched and #1311's symptom PERSISTS for
that (very common) layout. That is the honest behaviour: a cross-file macro table
would be non-deterministic with respect to include order, and guessing from
spelling is exactly what produced the regression above. Under-fixing is
recoverable; corrupting ordinary C is not. Pinned by
`c_leading_attr_macro_without_a_visible_define_is_left_untouched`, which asserts
the pre-blank output (`name=UINT32`, `rt=SEC_ATTR`) rather than pretending it is
fixed.

### Negative control, EXECUTED both ways

New walker test
`c_leading_typedef_return_and_keyword_alias_macros_keep_their_return_types`
encodes the four `edk.c` rows as an explicit table asserting the return types the
parent commit produced (`EFI_STATUS`, `CONST`, `STATIC`, `UINT32`), with
`#define SEC_ATTR __attribute__((…))` present so the pass is ACTIVE and the test
is about the RULE, not about the pass being switched off.

1. Old permissive regex temporarily restored in place (`[A-Z][A-Z0-9_]{2,}` capture,
   no `attribute_like_defines` gate). `cargo test --locked -p codegraph-extract
--lib -- c_leading_typedef_return_and_keyword_alias_macros_keep_their_return_types`
   → **exit 101, 1 failed**:

   ```
   left:  [("DriverEntry", 2, "EFIAPI"), ("GetName", 5, "CHAR8"),
           ("helper", 8, "<NULL>"),      ("Untouched", 10, "UINT32")]
   right: [("DriverEntry", 2, "EFI_STATUS"), ("GetName", 5, "CONST"),
           ("helper", 8, "STATIC"),          ("Untouched", 10, "UINT32")]
   ```

   The `left` column reproduces the shipped regression exactly.

2. Tightened version restored from the pre-mutation copy; file SHA-256 re-proved as
   `067baf6a609931b9cbd62cd0449cdc782c105bf59e1f79923914788961214616`. Same test →
   exit 0, **1 passed**.

The four `e95072c` walker tests were also retargeted: they now include the
`#define` line, because without it the tightened rule (correctly) does nothing —
they were previously green only by virtue of the over-permissive regex.

### Golden row delta

**ZERO.** `git diff --numstat reference/golden/` → EMPTY output; no golden file
changed by one byte, `cpp/` included. Every pre-existing node ID survives (nothing
in any golden corpus has a leading token with an attribute-like same-file
`#define`, so the tightening is inert there). Independently confirmed by
regenerating the C++ fixture per the `docs/equivalence.md` recipe with the
tightened release binary: the regenerated `nodes.json`, `edges.json`, `files.json`,
`refs.json` and `schema.sql` are all byte-IDENTICAL to the committed ones
(`cmp -s` → 0 for each). `colby.db` differs only in SQLite page-level bytes, which
is why the oracle compares the canonical JSON, not the raw DB.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo build --locked -p codegraph-extract` → exit 0.
- `cargo build --locked --release -p codegraph-rs` → exit 0.
- `cargo test --locked -p codegraph-extract --lib -- c_leading_typedef_return
c_leading_attr_macro c_isolation_table c_plain_typedef blank_c_leading_attr_macro
attribute_like_defines c_header_cuda c_plain_untouched` → exit 0, **16 passed**
  (includes the pre-existing `c_header_cuda_content_blanked` and `c_plain_untouched`,
  proving the CUDA path and plain C are undisturbed).
- Negative control with the OLD regex restored → exit **101**, 1 failed (values above).
- `cargo test --locked -p codegraph-bench` → exit 0, **26 passed** in
  `tests/equivalence.rs`, including `generated_golden_matches_committed_cpp_fixture`,
  `cpp_db_is_self_equivalent_to_cpp_golden`, and the untouched godot / ruby / mini /
  metal / cuda / arkts / solidity / terraform / erlang / nix / cfml oracles.
- `git diff --numstat reference/golden/` → EMPTY (zero golden bytes changed).
- `make fmt` → exit 0 (ledger prose written BEFORE formatting, so `fmt-check` is
  clean at CI time).
- `make ci CARGO='cargo --locked'` → exit 0 as the FINAL gate, **2928 passed across
  115 test binaries**, `✅ All CI checks passed!` (fmt-check + clippy `-D warnings`
  - workspace test + `scripts/guardrail.sh`). No byte was changed after it.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

An EARLIER `make ci` attempt failed on ONE unrelated test,
`codegraph-watch watcher::tests::begin_shutdown_is_nonblocking_and_detach_skips_the_join`
(`Option::unwrap()` on `None` at `watcher.rs:1944` — `ProjectWatcher::start`
returned `Ok(None)`, i.e. the in-process `HOME`/env mutation of a concurrently
running `policy.rs` test made the watch policy refuse the temp root). Nothing in
this change touches `codegraph-watch`; `git diff --stat` covers only
`codegraph-extract` + two docs files. Re-run in isolation: the single test → exit 0
five times in a row, and the whole `-p codegraph-watch --lib` suite → exit 0 six
times in a row (100 passed each). It is the same class of in-process env race as
the known `install_completions_writes_zsh_fish_elvish_into_home` flake and was NOT
weakened, skipped or modified. The subsequent full `make ci` was green twice.

That first failing run also left four untracked SQLite byproducts
(`reference/golden/{arkts,godot}/colby.db-{wal,shm}`) from the equivalence oracle;
they are not in `.gitignore`, so they were deleted before staging. The tracked
golden files were never modified.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path must
be inside request cwd`); locked Cargo is the fallback, as in every prior batch. No
dependency, version or `Cargo.lock` byte changed.

## Batch C1 — calls through an imported singleton resolve to the method (2026-07-28)

Ports upstream `2ec877b` (`fix(resolution): calls through an imported singleton
resolve to the method`, upstream #1315 / issue #1292). Resolution behavior only;
no extraction, schema or node-ID change.

### Red (documented, with the actual wrong output)

Five new `#[test]`s in `crates/codegraph-resolve/src/import_resolver.rs` drive the
REAL production entry point `resolve_via_import` over the in-crate `TestContext`
graph. The shape is upstream's exact repro: `src/store.ts` exports
`class ReproStore { notifyJoinGuildStatus() }` plus
`export const reproStore = new ReproStore();`, and `src/caller.ts` calls
`reproStore.notifyJoinGuildStatus()` after `import { reproStore } from './store'`.

`cargo test --locked -p codegraph-resolve --lib -- imported_singleton` on the
pre-change bytes → **1 failed, 4 passed**:

| assertion                                              | expected                       | ACTUAL on pre-change bytes |
| ------------------------------------------------------ | ------------------------------ | -------------------------- |
| `imported_singleton_call_resolves_to_the_class_method` | `method:notifyJoinGuildStatus` | `constant:reproStore`      |

That is the defect verbatim: `find_exported_symbol` resolves the base to the
exported CONSTANT and the `Calls` edge lands there, so `callers
notifyJoinGuildStatus` misses every cross-file use while the identical same-file
call resolves to the method through local-variable receiver inference (#1108).
The other four tests are the guard set (member READ still targets the value;
uninferable initializer keeps the constant; a member the type does not declare
keeps the constant; a same-named local in another function must not donate a
type) — they passed pre-change and must keep passing, so they are guards, not Reds.

### Green (minimal)

- `crates/codegraph-resolve/src/name_matcher.rs`: `resolve_method_on_type`,
  `normalize_inferred_type_name`, `local_receiver_type_patterns` and
  `regex_escape` widened from private to `pub(crate)`. No body changed — this
  mirrors upstream's `export` of the same three helpers.
- `crates/codegraph-resolve/src/import_resolver.rs`: new
  `resolve_imported_instance_member`, consulted in the non-namespace branch of
  `resolve_via_import`'s member-descend BEFORE `resolve_static_member`. It fires
  only for `EdgeKind::Calls` on a `Constant`/`Variable` target, reads the value's
  type from ITS OWN declaration lines (`value.start_line..=value.end_line`) via
  the shared #1108 pattern table, and resolves the member through
  `resolve_method_on_type` at confidence 0.85 / `ResolvedBy::InstanceMethod`.
  Validation failure returns `None`, so the pre-existing constant edge stands —
  a silent keep, never a fabricated edge. This is the same discipline as B1
  (`1839504`): when the receiver cannot be typed, emit nothing rather than fall
  through to bare-name guessing.

The declaration-slice bound is why the line-6 local in
`imported_singleton_type_is_read_only_from_its_own_declaration_lines` cannot type
the line-9 constant.

### Golden row delta

NONE. `git diff --stat 87a46dc..HEAD -- reference/golden/` → **empty output**,
and `git status --porcelain reference/golden/` → empty. Expected: the classification
is golden-neutral, and the fixture corpus contains no imported-singleton call shape,
so no golden row can move.

### Negative control, executed

Replaced the `return Some(instance_member)` with a discarding
`let _ = resolve_imported_instance_member(...)` (call kept so the function stays
live) and re-ran: `imported_singleton_call_resolves_to_the_class_method` went RED
again with the SAME wrong value `constant:reproStore`; the four guards stayed
green. Restored from the pre-control copy and verified
`sha256sum -c` → `crates/codegraph-resolve/src/import_resolver.rs: OK`
(`1a1743ff88e8d7682ea61b0aa6ef91cba42e9bb1cb38a9864c5addc2068efa51`).

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0 (workspace 0.40.4, 10
  packages), run before every Cargo batch; every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-resolve --lib -- imported_singleton` → exit 0,
  **5 passed**.
- `cargo test --locked -p codegraph-resolve --lib` → exit 0, **649 passed, 0
  failed**.
- `cargo clippy --locked -p codegraph-resolve --all-targets -- -D warnings` → exit 0.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, **26
  passed, 0 failed** (unchanged 26/26).
- `git diff --stat 87a46dc..HEAD -- reference/golden/` → empty.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path
must be inside request cwd`); locked Cargo clippy/test is the honest fallback and
no LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte changed.

## Batch C2 — literal-receiver builtins and nested locals stop fabricating call edges (2026-07-28)

Ports upstream `c472cfb` (`fix(resolution): literal-receiver builtins and nested
locals stop fabricating call edges`, upstream #1317 / issue #1230). Two
independent defects, fixed independently in one behavior commit — exactly as
upstream framed them, because the issue's repro needs both to stop producing the
wrong edge.

### Red (documented, with the actual wrong output)

**Extraction half** — five new `#[test]`s in
`crates/codegraph-extract/src/walker.rs` drive the REAL path
(`extract_source` → `Walker::extract_call` / `extract_object_name_call`).
`cargo test --locked -p codegraph-extract --lib -- literal_receiver identifier_receiver`
on the pre-change bytes → **5 failed, 1 passed**:

| assertion (language, shape)                                                                                    | expected      | ACTUAL on pre-change bytes                                    |
| -------------------------------------------------------------------------------------------------------------- | ------------- | ------------------------------------------------------------- |
| Python `", ".join(sorted(x))`                                                                                  | no `join` ref | `["join", "sorted"]`                                          |
| Python `[1,2].append`, `{'k':1}.keys`, `{1,2}.union`, `(1,2).count`, `1.5.hex`, `None.__str__`, `True.__str__` | no refs       | `["append","keys","union","count","hex","__str__","__str__"]` |
| TS `"x".toUpperCase`, `[1,2].map`, `` `t`.trim ``, `/re/.test`, `0xff.toString`, `true.toString`               | no refs       | `["toUpperCase","map","trim","test","toString","toString"]`   |
| Java `"x".trim()`                                                                                              | no ref        | `["\"x\".trim"]`                                              |
| PHP `"x"->foo()`                                                                                               | no ref        | `["\"x\".foo"]`                                               |

`identifier_receiver_calls_are_unaffected_by_the_literal_guard` passed pre-change
— it is the guard proving `sep.join` / `s.trim` survive, not a Red.

**Resolution half** — five new `#[test]`s in
`crates/codegraph-resolve/src/name_matcher.rs` drive `match_by_exact_name` over
the in-crate `Ctx`. `cargo test --locked -p codegraph-resolve --lib -- nested_local
class_member_candidate top_level_and_unparented` on the pre-change bytes →
**2 failed, 3 passed**:

| assertion                                           | expected | ACTUAL on pre-change bytes                                           |
| --------------------------------------------------- | -------- | -------------------------------------------------------------------- |
| `nested_local_is_unreachable_from_another_function` | `None`   | `Some(target_node_id: "function:join", confidence: 0.9, ExactMatch)` |
| `nested_local_is_unreachable_from_another_file`     | `None`   | `Some(target_node_id: "function:join", confidence: 0.9, ExactMatch)` |

The three passing ones (`nested_local_resolves_from_inside_its_container`,
`class_member_candidate_is_not_scope_filtered`,
`top_level_and_unparented_candidates_are_not_scope_filtered`) are the
must-not-regress guards, including the C++ namespace-prefix shape whose
qualified name carries `::` with no container node.

**End-to-end half (the load-bearing one)** — one new `#[test]` in
`crates/codegraph-resolve/src/resolver.rs`,
`literal_receiver_and_nested_local_produce_no_fabricated_call_edge`, indexes
upstream's exact `repro.py` from disk through `codegraph_extract::extract_file`
and runs the PRODUCTION `resolve_and_persist`, then reads persisted `edges`.
The decoy is present by construction: the only project `join` is nested inside
`format_fields`, so a bare-name fallback visibly binds `", ".join` to it. On the
pre-change bytes the nested `join` had **three** callers
(`["function:37f4985b…", "function:dd8ad4c1…", "function:f41fd935…"]` — itself
via `"-".join`, its container, and `report_missing`) where exactly one is
correct.

### Green (minimal)

- `crates/codegraph-extract/src/walker.rs`: new `LITERAL_RECEIVER_KINDS` +
  `is_literal_receiver`, consulted at BOTH call-extraction receiver sites — the
  generic member-expression arm in `extract_call` and the Java/Kotlin/PHP
  `object`/`name` arm in `extract_object_name_call`. A literal receiver returns
  early, emitting NOTHING. Nested calls in the arguments are visited
  independently, which is why `sorted(...)` survives.
  The kind list is upstream's, plus `encapsed_string`: this workspace's
  `tree-sitter-php` reports a double-quoted literal receiver under that kind,
  which upstream's set omits. That extra entry was derived by probing the real
  grammars in this workspace across Python/TS/Java/PHP/Ruby/Kotlin/Swift/Rust/
  Dart/Scala/C#/Go — testing shapes the fix was NOT designed for, per the Batch B
  lesson — and every kind in the list was observed in receiver position by that
  probe or is upstream's verbatim entry.
- `crates/codegraph-resolve/src/name_matcher.rs`: new `is_lexically_reachable`,
  applied as a filter on `match_by_exact_name`'s candidate list. A `Function`
  candidate whose `qualified_name` parent resolves to a same-file
  `Function`/`Method` that ENCLOSES it by line range survives only when the ref
  is in that same file AND inside the container's line range. Class members
  (parent is class-like), top-level symbols (no `::`) and C++ namespace prefixes
  (no container node) are untouched.

Same discipline as B1 (`1839504`) and C1: when a receiver cannot aid type
inference, emit NOTHING rather than a bare name, because a bare name falls
through to exact-name matching and guesses among unrelated same-named symbols.

### Golden row delta

NONE. `git diff --stat 87a46dc..HEAD -- reference/golden/` → **empty output**;
`git status --porcelain reference/golden/` → empty. All 26 equivalence oracles
pass unchanged, so no fixture in the corpus contains a literal-receiver call or a
cross-scope nested-local call. (Upstream measured −27 edges on excalidraw and
byte-identical output on `requests`; our fixture corpus lands in the
byte-identical class.)

### Negative control, EXECUTED both ways

1. Extraction guard neutered (`if is_literal_receiver(...) { return; }` →
   `let _ = is_literal_receiver(...);` at both sites, resolution filter intact):
   all 5 extraction Reds went RED again with the SAME wrong ref lists, AND the
   end-to-end resolver test went RED with the nested `join` back to 3 callers.
   Restored; `sha256sum -c` → both files OK.
2. Resolution filter neutered (`.filter(|n| is_lexically_reachable(...) || true)`,
   extraction guard intact): the two nested-local Reds went RED again with the
   SAME `Some(function:join, 0.9, ExactMatch)`; the end-to-end test stayed GREEN
   (with literal receivers emitting nothing, `report_missing` no longer reaches
   the decoy). Restored; `sha256sum -c` → both files OK.

Green hashes: `walker.rs`
`42834fe71029cbe6977c8a699f995940911fa6432d64707c38677785a57f132c`,
`name_matcher.rs`
`47724ae2a13ffdcf103260b1a2f66a426aafc81696c08da7bbfb227a42fefe51`.

Control 2 is the honest reading of the two-defect split: the end-to-end test is
load-bearing for the EXTRACTION half, and the two unit Reds are load-bearing for
the RESOLUTION half. Neither half alone covers both, which is why both are
committed together and both controls were run.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-extract` → exit 0 (382 lib + every
  integration target green).
- `cargo test --locked -p codegraph-resolve` → exit 0 (655 lib + integration
  targets green).
- `cargo clippy --locked --workspace --all-targets -- -D warnings` → exit 0.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, **26
  passed, 0 failed** (unchanged 26/26).
- `cargo test --locked --workspace` → **exit 101 on the first run**, failing ONLY
  `codegraph-rs formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home`
  (`assertion failed: dir.join(".config/fish/completions/codegraph.fish").is_file()`
  at `main.rs:6202`; the printed path shows fish completions written to a
  DIFFERENT temp home — the known in-process `HOME`/`XDG_DATA_HOME` race). That
  test is unrelated to resolution or extraction and was NOT weakened, skipped or
  modified. Re-run in isolation via `-p codegraph-rs --bin codegraph` → exit 0
  four times in a row (1 passed each). The full `cargo test --locked --workspace`
  re-run → **exit 0**, 121 `test result: ok` lines, zero failures.
- `git diff --stat 87a46dc..HEAD -- reference/golden/` → empty.

The first workspace run also left two untracked SQLite byproducts
(`reference/golden/cpp/colby.db-{wal,shm}`) from the oracle; they are not
gitignored, so they were deleted before staging. No tracked golden file changed.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path
must be inside request cwd`); locked Cargo clippy/test is the honest fallback and
no LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte changed.

## Batch C3 — resolved-ref cleanup keys on the row id, so batch boundaries stop dropping sibling call sites (2026-07-28)

Ports upstream `e871c49` (#1269/#1270). Base for this entry: `285d591`
(Batch C2). One commit, resolution/store only.

### The old key, quoted verbatim

`crates/codegraph-store/src/queries.rs` held two tuple-keyed deletes:

```sql
DELETE FROM unresolved_refs WHERE from_node_id = ? AND reference_name = ? AND reference_kind = ?
```

```sql
DELETE FROM unresolved_refs WHERE from_node_id = ? AND reference_name = ? AND reference_kind = ? AND id <= ?
```

`(from_node_id, reference_name, reference_kind)` is NOT unique — the table has no
UNIQUE constraint, and two calls to the same name from the same enclosing node
differ only in `line`/`col`/`id`. Neither statement carried a `LIMIT`, so
resolving ONE call site deleted EVERY row sharing its tuple.

Two further facts made this reachable rather than theoretical. First, the
`id <= max_id` guard on the batched variant proves the author already knew the
tuple repeats — its own doc said a duplicate tuple in a LATER batch has
`id > max_id` and is preserved. That guard protects ACROSS batches and does
nothing WITHIN one, where every sibling satisfies `id <= max_id`. Second, the
non-batched variant's doc claimed "Deletes one row per tuple", which was false;
that sentence is corrected in this commit.

The row id was available the whole time and simply discarded:
`unresolved_refs.id` is `INTEGER PRIMARY KEY AUTOINCREMENT`,
`row_to_unresolved_ref` already read it into `UnresolvedRef.id`, and
`to_ref_view` then dropped it because `RefView` had no field to hold it — leaving
the resolver able to rebuild only the coarse tuple.

### Red (documented, with the actual observed drop)

A throwaway probe module in `resolver.rs`, driven through the production path
(`extract_file` → `upsert_nodes`/`insert_edges`/`insert_unresolved_refs` →
`resolve_and_persist`), on this Python fixture:

```python
def run(thing):
    thing.render()      # line 12 — `thing` not yet bound, does NOT resolve
    thing = Widget()
    thing.render()      # line 14 — resolves to Widget::render
```

Observed (`cargo test --locked -p codegraph-resolve --lib scratch_observe_sibling_rows
-- --nocapture` → exit 0, printing):

```text
REF from=function:70b72f5… name=thing.render kind=Calls line=12
REF from=function:70b72f5… name=thing.render kind=Calls line=14
RESOLVED 2 UNRESOLVED 1
  OK name=thing.render line=14 -> method:830ab53… by=InstanceMethod
  NO name=thing.render line=12
```

and then, decisively, ZERO `REMAIN` lines: the store's `unresolved_refs` table was
EMPTY. Resolution itself was correct — it reported line 12 unresolved — but
cleanup of line 14 deleted line 12's row too. The unresolved sibling was
silently erased from the table that exists to record it, so it can never be
retried and its edge can never appear. The probe module was reverted
(`git checkout --` → tree clean) before any fix; its observation is the Red.

### Green (minimal)

Thread the ROW ID from the stored row through resolution to cleanup. No coarse
fallback key is retained anywhere — keeping one would reintroduce the defect.

- `crates/codegraph-resolve/src/types.rs`: `RefView` gains
  `row_id: Option<i64>`. `None` means "this view was never persisted" — a
  `FrameworkResolver` synthesizes `RefView`s in memory and tests build them by
  hand; such a view names no row and must delete nothing.
- `crates/codegraph-resolve/src/resolver.rs`: `to_ref_view` populates
  `row_id: reference.id`; `ref_view_to_unresolved` now carries it back
  (`id: reference.row_id`) instead of hardcoding `None`, so the round-trip is
  lossless while a framework-synthesized view still persists with `id: None`.
  Both delete call sites collect `filter_map(|r| r.original.row_id)`, so an
  unpersisted view is skipped rather than widened into a tuple.
- `crates/codegraph-store/src/queries.rs`: `delete_resolved_unresolved_refs`
  takes `&[i64]` and runs `DELETE FROM unresolved_refs WHERE id = ?`.
  `delete_resolved_unresolved_refs_up_to` is REMOVED rather than re-signatured:
  its entire reason for existing was the `id <= max_id` cross-batch guard, and an
  id from batch N cannot name a row in batch N+1, so the batched caller is
  already precise with the plain row-id delete. Its doc rationale moved onto the
  surviving function, and the false "Deletes one row per tuple" sentence is
  replaced with the actual guarantee plus why the old key was unsafe.
- 47 `RefView` construction sites across `crates/codegraph-resolve/`
  (frameworks, `name_matcher`, `import_resolver`, `framework`, both integration
  tests) updated explicitly. `RefView` gained no `Default`, and no
  `..Default::default()` was introduced, so the compiler had to see every site.
  `snapshot_equivalence.rs` threads the real `unresolved.id`.

The batched-resolution doc comment on `resolve_and_persist_batched` is corrected
in the same commit: its byte-equivalence argument previously rested on the
tuple-keyed delete being deferred; it now rests on the row-id key, which is
strictly stronger (a batch can reach neither a later batch's row nor a sibling
sharing its tuple).

### Load-bearing tests

- `codegraph-store` `delete_resolved_refs_by_row_id_removes_only_the_named_row`
  SUPERSEDES `delete_resolved_refs_precise_and_bounded`. The old test used three
  DISTINCT names (`Alpha`/`Beta`/`Gamma`), which is exactly why it never caught
  this: with no tuple collision in the fixture, the missing `LIMIT` was
  unobservable. The new test inserts TWO tuple-identical `helper` rows differing
  only in `line`, deletes the first by id, and asserts the second survives with
  its own id and line — then asserts a repeat delete of an already-gone id is a
  no-op.
- `codegraph-resolve` `resolving_one_call_site_keeps_its_tuple_identical_sibling_row`
  drives the PRODUCTION path on the Red's fixture (pattern copied from C2's
  accepted `literal_receiver_and_nested_local_produce_no_fabricated_call_edge`):
  it asserts the fixture really seeds two tuple-identical rows, that exactly one
  resolves, and that the UNRESOLVED sibling is still in `unresolved_refs`
  afterwards at a different line.
- `to_ref_view_and_back_roundtrip` extended to assert the id survives both
  directions (`Some(7)`), plus a new
  `ref_view_to_unresolved_keeps_framework_ref_row_id_absent` pinning the
  never-persisted case at `None`.

### Golden row delta

NONE. `git diff --stat 285d591..HEAD -- reference/golden/` → **empty output**;
`git status --porcelain reference/golden/` → empty. Expected: the manifest
classifies C3 as "exact persisted row IDs; golden-neutral", and the change only
narrows WHICH rows a delete removes — it never alters extraction, resolution
decisions, edge construction, or insertion order. All 26 equivalence oracles pass
unchanged. No golden was regenerated.

### Negative control, EXECUTED

Restored tuple semantics BEHIND the new row-id signature (each id looked up to
its `(from,name,kind)` tuple, then the old tuple-keyed `DELETE` executed), so
only the delete's precision changed and nothing else:

- `cargo test --locked -p codegraph-store --lib delete_resolved_refs_by_row_id_removes_only_the_named_row`
  → **exit 101**, `assertion left == right failed; left: 1, right: 2` at
  `queries.rs:2522` — both `helper` rows gone, only `Other` left.
- `cargo test --locked -p codegraph-resolve --lib resolving_one_call_site_keeps_its_tuple_identical_sibling_row`
  → **exit 101**, `assertion failed: the UNRESOLVED sibling must survive cleanup
of its resolved twin; left: 0, right: 1` at `resolver.rs:3626`.

Both tests are load-bearing, at both layers. Restored from backup;
`sha256sum -c` → `queries.rs: OK`, `resolver.rs: OK` (verify exit 0). Green
hashes: `queries.rs`
`42deaa32da2a6b0053ec974da5891d2af34386d3c219bdcb63fef54bed71d5f2`,
`resolver.rs`
`072c66b36f1b061d06c58809f7306b6358526eda88d6966d7eca62413d01090b`,
`types.rs`
`a7084d442d91708073a7e4ec44f72ad96da7e6df914b0cec4bc094b0065fbe8b`.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo build --locked --workspace --all-targets` → first attempt exit 101
  (`delete_resolved_unresolved_refs_up_to` not found — the superseded store test
  still called it); after replacing that test, clean.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` → exit 0.
- `cargo test --locked -p codegraph-store --lib delete_resolved_refs_by_row_id_removes_only_the_named_row`
  → exit 0.
- `cargo test --locked -p codegraph-resolve --lib resolving_one_call_site_keeps_its_tuple_identical_sibling_row`
  → exit 0.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, **26
  passed, 0 failed** (unchanged 26/26).
- `cargo test --locked --workspace` → **exit 101 on the first run**, failing ONLY
  `codegraph-rs formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home`
  — the KNOWN pre-existing in-process `HOME`/`XDG_DATA_HOME` race documented in
  the Batch C2 entry, unrelated to resolution or the store. It was NOT weakened,
  skipped, `#[ignore]`d or modified. Re-run in isolation → exit 0, 1 passed.
- The first workspace run left two untracked SQLite byproducts
  (`reference/golden/cfml/colby.db-{wal,shm}`) from the oracle; they are not
  gitignored, so they were deleted before staging. No tracked golden changed.
- `git diff --stat 285d591..HEAD -- reference/golden/` → empty.

`lsp_diagnostics` was attempted on `queries.rs` and again refused this worktree
(`LSP file path must be inside request cwd`); locked Cargo clippy/test is the
honest fallback and no LSP-clean result is claimed. No dependency, version or
`Cargo.lock` byte changed. No Windows/MSVC runtime validation is claimed — this
work ran on Linux only.

---

## Batch A1 — non-ASCII identifiers reach their definers through the real search surfaces (2026-07-28)

Ports upstream issue #1372. Base for this entry: `29f635b` (Batch C3). One
commit, scoring + a new CLI/MCP integration test.

### What the current build ACTUALLY did (measured before any edit)

The reported upstream symptom — `explore` returning _no relevant code_ for a
pure-CJK query while `query` finds the file — does NOT reproduce here. Measured
on the real binary (`target/debug/codegraph`, built from `29f635b`) against a
throwaway project holding `示例模块.lua` (defining `M.示例函数`), `cafe.py`
(`café_lookup`) and `uni.py` (`Ünicode`):

```text
$ codegraph query "示例模块"      → file 示例模块.lua           (found)
$ codegraph query "示例函数"      → method 示例函数              (found)
$ codegraph query "café_lookup"  → function café_lookup        (found)
$ codegraph query "Ünicode"      → class Ünicode                (found)
$ codegraph explore "示例模块"    → "## Exploration: 示例模块", 3 symbols,
                                    the file's verbatim source
```

FTS5 tokenization is NOT the crux: `nodes_fts` is a `unicode61`-tokenized
contentless-mirror table (`schema.rs:100`), and `unicode61` already treats CJK,
Cyrillic, Greek, Kana, Hangul and accented Latin codepoints as token
characters. Non-ASCII names are indexed and retrievable. So no FTS/schema
change was made and none is needed — the golden `.schema` is untouched.

The defect that IS present is in RANKING, one layer above FTS: the scoring
tokenizer in `crates/codegraph-graph/src/query/scoring.rs` is ASCII-only, so a
non-ASCII query word produces ZERO scoring tokens and therefore contributes
NOTHING to `score_path_relevance`. Probe (throwaway test, reverted):

```text
word="示例模块" terms=[] path_relevance=0
word="модуль"   terms=[] path_relevance=0
word="μονάδα"   terms=[] path_relevance=0
word="サンプル"  terms=[] path_relevance=0
word="모듈"      terms=[] path_relevance=0
word="café"      terms=["caf"]          path_relevance=13
word="samplemodule" terms=["samplemodule"] path_relevance=13
```

`extract_search_terms` split on `!c.is_ascii_alphanumeric()`, so an entire
non-ASCII word was discarded; `café` survived only as the truncated `caf`
because `é` acted as a separator; `normalize_name_token` likewise erased
non-ASCII, making project-name filtering meaningless for such repos.

### Red (documented, with the actual wrong output)

New test `crates/codegraph-cli/tests/unicode_search_cli.rs`, driving the REAL
binary (`init`, then `query --json`, plus a `serve --mcp` stdio session). Each
of SEVEN scripts gets a module file named in that script defining a function,
paired with an ASCII-named DECOY file defining the SAME function name — so only
path relevance can separate them — plus a two-character CJK case (`模块`, a
whole word below the ASCII 3-char floor).

`cargo test --locked -p codegraph-rs --test unicode_search_cli` → exit 101,
2 of 4 failing. Verbatim, the wrong ranking:

```text
the query names the module file, so its definer must rank first:
[cjk] query "示例模块" "handlercjk": definer 示例模块.py at Some(1), decoy zdecoy_cjk.py at Some(0)
[cyr] query "модуль" "handlercyr": definer модуль.py at Some(1), decoy zdecoy_cyr.py at Some(0)
[grk] query "μονάδα" "handlergrk": definer μονάδα.py at Some(1), decoy zdecoy_grk.py at Some(0)
[kana] query "サンプル" "handlerkana": definer サンプル.py at Some(1), decoy zdecoy_kana.py at Some(0)
[kor] query "모듈" "handlerkor": definer 모듈.py at Some(1), decoy zdecoy_kor.py at Some(0)
[short] query "模块" "handlershort": definer 模块.py at Some(1), decoy zdecoy_short.py at Some(0)
```

and through the MCP `codegraph_search` contract over real stdio:

```text
MCP search must rank the named module's definer above its decoy:
## Search Results (3 found)

### handlercjk (function)
zdecoy_cjk.py:1
### handlercjk (function)
示例模块.py:1
```

The ASCII control case (`samplemodule handlerascii`) PASSED before the fix —
that is the point: the wrong answer was specific to non-ASCII input. Measured
scores on the real binary: definer and decoy tied at exactly `75.8908` for CJK
(the file name contributed 0), where the ASCII pair separated `88.8908` vs
`75.8908`. The tie then resolved to whichever row FTS returned first, which is
why the decoy won.

### Green (minimal)

`crates/codegraph-graph/src/query/scoring.rs`, three edits, no new dependency:

- `extract_search_terms` word split: `!c.is_ascii_alphanumeric()` →
  `!c.is_alphanumeric()`, so a non-ASCII word survives as one token and
  `café_módulo` no longer shatters into `caf`/`dulo`.
- New `meets_min_token_len` replaces the bare `< 3` check: ASCII keeps the
  three-character floor byte-for-byte; a token containing any non-ASCII
  character is admitted at two, because an unsegmented script packs a whole word
  into two characters (`模块` IS "module").
- `normalize_name_token`: `is_ascii_alphanumeric` → `is_alphanumeric`, so
  project-name-token filtering sees non-ASCII repo names.

The camel/acronym/compound/snake helpers were deliberately left ASCII-only —
case humps are not a concept in unsegmented scripts, and widening them would
change ASCII tokenization. No FTS query string, BM25 weight, tie-break or SQL
was touched, so ranking stays a pure function of the index (proven by the
repeat-query determinism test).

### Golden row delta

None. `git diff --stat 29f635b..HEAD -- reference/golden/` → EMPTY, and
`git status --short reference/golden/` → empty.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**26 passed, 0 failed** (unchanged 26/26).

### Negative control, EXECUTED

`git stash push -- crates/codegraph-graph/src/query/scoring.rs` (production
change only; the new test kept) → `cargo test --locked -p codegraph-rs --test
unicode_search_cli` → exit 101, exactly the two Red failures above. `git stash
pop` restored it; restored-file SHA-256 of `scoring.rs`
`2fcaaef957f8d20bcc530df7e50382dd8b451c105e8ea769e87532126b5ce34c` (before the
clippy follow-up below), and with the fix re-applied the same command → exit 0,
4 passed.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` → **first
  attempt exit 101**: `needless_character_iteration` on
  `token.chars().any(|c| !c.is_ascii())`. Rewritten to `!token.is_ascii()`
  (identical semantics); re-run → exit 0.
- `cargo test --locked -p codegraph-rs --test unicode_search_cli` → exit 0,
  4 passed.
- `cargo test --locked -p codegraph-graph` → exit 0 (all targets green,
  including the pre-existing `extract_search_terms` / `score_path_relevance` /
  `normalize_name_token` unit tests, which were NOT modified).
- `cargo test --locked -p codegraph-mcp` → exit 0, including
  `rmcp_parity` (15 golden MCP fixtures) and `golden_mcp`.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.

`lsp_diagnostics` was attempted on `scoring.rs` and again refused this worktree
(`LSP file path must be inside request cwd`); locked Cargo clippy/test is the
honest fallback and no LSP-clean result is claimed. No dependency, version or
`Cargo.lock` byte changed. No Windows/MSVC runtime validation is claimed — this
work ran on Linux only.

---

## Batch A2 — multi-hump field-name queries reach their definers (2026-07-28)

Ports upstream `1de7e8f` (#1319). Base for this entry: `8c80331` (Batch A1). One
commit: a new store query, the query-layer seeding pass that consumes it, and a
CLI integration test.

### Red (documented, with the actual wrong output)

New test `crates/codegraph-cli/tests/multi_hump_query_cli.rs`, driving the REAL
binary (`init`, then `query --json`). The fixture holds, per query shape, three
files:

- the DEFINER `src/profileController.js` — `getProfileInfoV2`,
  `updateUserProfileIdMapping`, `loadOrderStateSnapshot`;
- a PROSE decoy `src/prose_decoy.js` — a constant whose signature/docstring
  merely MENTIONS the words (`const NOTES = "profileInfo userProfileId …"`);
- a NAME decoy `src/name_decoy.js` — callables whose lowercase names CONTAIN the
  query run at NO segment boundary (`xxprofileinfoxx`, `xxuserprofileidxx`,
  `xxorderstatexx`). This is the required decoy: a naive `LIKE %needle%` binds
  to it, and would do so with a SHORTER name, i.e. it would win.

`cargo test --locked -p codegraph-rs --test multi_hump_query_cli` → exit 101.
Verbatim:

```text
multi-hump field-name queries must reach their definers:
query "profileInfo": definer getProfileInfoV2 ABSENT; ranking was [("NOTES", "src/prose_decoy.js")]
query "profile_info": definer getProfileInfoV2 ABSENT; ranking was [("NOTES", "src/prose_decoy.js")]
query "ProfileInfo": definer getProfileInfoV2 ABSENT; ranking was [("NOTES", "src/prose_decoy.js")]
query "userProfileId": definer updateUserProfileIdMapping ABSENT; ranking was [("NOTES", "src/prose_decoy.js")]
query "user_profile_id": definer updateUserProfileIdMapping ABSENT; ranking was []
query "orderState": definer loadOrderStateSnapshot ABSENT; ranking was [("NOTES", "src/prose_decoy.js")]
```

The definer was not merely mis-ranked, it was ABSENT: FTS5 matches whole tokens
with a trailing prefix, so `"userProfileId"*` matches the STRING inside `NOTES`
but never the INFIX inside `updateUserProfileIdMapping`. `user_profile_id`
returned nothing at all. Ground truth from `sqlite3`, confirming the definers
were in the index the whole time:

```text
sqlite> select nodes.name, nodes.file_path from nodes_fts join nodes
        on nodes_fts.id=nodes.id where nodes_fts match '"profileInfo"*';
NOTES|src/unrelated.js

sqlite> select kind,name,file_path from nodes where lower(name) like '%userprofileid%';
function|xxuserprofileidxx|src/name_decoy.js
function|updateUserProfileIdMapping|src/profileController.js
```

That second row pair is exactly why substring containment alone is NOT the fix.

### Green (minimal)

Three pieces, no new dependency, no schema change:

1. `crates/codegraph-store/src/queries.rs` — new
   `callable_nodes_by_name_infix(needle, limit)`: callables only
   (`function`/`method`/`component`, upstream's "kind whitelist" so hot
   single-word terms can't crowd definers out of the length-ordered batch),
   matching `lower(name) LIKE %needle%` OR the separator-stripped form (so
   `user_profile_id` reaches `userProfileId` and vice-versa), ordered
   `length(name), name, file_path, start_line` — fully specified, never
   SQLite's incidental row order.
2. `crates/codegraph-graph/src/query/scoring.rs` — `identifier_segments`,
   `is_multi_segment_identifier`, `name_segments_contain_run`: the hump/acronym/
   separator splitter and the CONTIGUOUS-segment-run filter.
3. `crates/codegraph-graph/src/query/mod.rs` — `seed_multi_segment_definers`,
   called after the exact-name supplement and before rescoring. It fires ONLY
   for a multi-segment token that is not already an exact symbol name
   (`nodes_by_lower_name` empty), caps candidates at 50 and seeds at most 3 per
   token, and admits a candidate only when its own segments contain the query's
   segment run contiguously.

The boundary filter is what rejects `xxprofileinfoxx`: following the accepted
Batch C precedent, when a candidate cannot be shown to DEFINE the queried field,
nothing is emitted rather than a guess. A silent miss beats a wrong answer.

### A rejected first attempt, recorded

The first Green also added `.then_with(|| a.node.id.cmp(&b.node.id))` as a
tie-break in `sort_by_score_desc`. That made
`cargo test --locked -p codegraph-graph` fail (exit 101) on the pre-existing
golden search oracle:

```text
case `filter_only_kind_method` query `kind:method`: id ordering mismatch
 got:  [3c01f33…, 89ae38f0…, d9785bc2…, f501ba98…]
 want: [d9785bc2…, 3c01f33…, f501ba98…, 89ae38f0…]
```

All four score exactly `11`, and the golden order is the upstream's
`ORDER BY name` (`__init__`, `greet`, `increment`, `value`) preserved by Rust's
STABLE sort. An id tie-break would have replaced a meaningful order with a hash
order. The tie-break was REVERTED; determinism is instead guaranteed by the
fully-specified `ORDER BY` in the new store query plus the existing stable sort,
and is asserted by `multi_hump_ranking_is_deterministic_across_repeated_queries`.
The golden was NOT touched.

### Golden row delta

None. `git diff --stat 29f635b..HEAD -- reference/golden/` → EMPTY;
`git status --short reference/golden/` → empty.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**26 passed, 0 failed**.

### Negative control, EXECUTED

`git stash push` of the three production files (test kept) →
`cargo test --locked -p codegraph-rs --test multi_hump_query_cli` → exit 101,
exactly the six Red lines above. `git stash pop` restored them; restored SHA-256:

- `crates/codegraph-graph/src/query/mod.rs`
  `fed17893c2ea1f1ff6c7778bf06f0742c08a1bb82cb0002742e3f213ba5dcadd`
- `crates/codegraph-graph/src/query/scoring.rs`
  `af689c1df8224cb11909bd33048a798bdf34345e438d9e75b89ce424314e32bb`
- `crates/codegraph-store/src/queries.rs`
  `2b499860f476cdb1cb7245504928b4c78790aa7a2bf8cdd4bb5e63f18b39d2d2`

With the fix re-applied the same command → exit 0, 4 passed.

### Post-fix behaviour on the real binary

```text
$ codegraph query "profileInfo" --json
  39.000 function getProfileInfoV2           src/profileController.js
   4.736 constant NOTES                      src/unrelated.js
$ codegraph query "user_profile_id" --json
  39.000 function updateUserProfileIdMapping src/profileController.js
```

The name decoys are absent from both.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` → **first
  attempt exit 101** (`doc_lazy_continuation` on the new test's module doc list);
  a blank `//!` line was added and re-run → exit 0.
- `cargo test --locked -p codegraph-rs --test multi_hump_query_cli` → exit 0,
  4 passed.
- `cargo test --locked -p codegraph-store --lib callable_infix…` → exit 0.
- `cargo test --locked -p codegraph-graph --lib identifier_segments…` → exit 0.
- `cargo test --locked -p codegraph-graph -p codegraph-store -p codegraph-mcp`
  → exit 0 (after the tie-break revert above).
- `cargo test --locked -p codegraph-rs --test multi_hump_query_cli --test
unicode_search_cli --test explore_node_cli --test cli_commands` → exit 0.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.

`lsp_diagnostics` again refused this worktree (`LSP file path must be inside
request cwd`); locked Cargo clippy/test is the honest fallback and no LSP-clean
result is claimed. No dependency, version or `Cargo.lock` byte changed. No
Windows/MSVC runtime validation is claimed — this work ran on Linux only.

---

## Batch A3 — `find_path` enqueues each work item exactly once, and a test can prove it (2026-07-28)

Ports upstream issue #1359. Base for this entry: `b20260ae` (Batch A2). One
commit, `codegraph-graph` traversal only.

### The defect, quoted verbatim from the pre-fix source

`crates/codegraph-graph/src/graph/mod.rs`, `find_path`:

```rust
let mut visited = HashSet::new();
...
    if visited.contains(&node_id) { continue; }
    visited.insert(node_id.clone());
...
    for edge in outgoing {
        if !visited.contains(&edge.target)
            && let Some(next_node) = next_nodes.get(&edge.target)
        {
            let mut next_path = path.clone();
            ...
            queue.push_back((edge.target.clone(), next_path));
        }
    }
```

`visited` is inserted at DEQUEUE, so it cannot stop a fan-in layer from pushing
the same target once per predecessor. `traverse_bfs` in the same file already
carries the separate enqueue-once guard from #1090 (`unvisited_neighbor_ids` +
the per-insertion check); `find_path` never got it. Each redundant push also
clones the whole `Vec<PathStep>`, so wasted work and peak memory scale with EDGE
count rather than node count.

The shortest path stays correct — duplicates are dropped at dequeue — so the
defect is INVISIBLE from `find_path`'s return value. That is why the
instrumentation is part of the fix.

### Green (minimal)

- New `PathSearchStats { enqueued, dequeued, duplicate_dequeues }` and
  `find_path_instrumented`, which returns the path AND the queue accounting.
  `find_path` is now a thin wrapper over it, so its signature and behaviour for
  every existing caller are unchanged.
- An `enqueued: HashSet<String>` seeded with `from_id`, consulted in BOTH the
  `want_ids` prefilter and the per-edge push condition, and inserted at push
  time — mirroring `traverse_bfs`'s #1090 shape.

Counters live in the instrumented function only; no logging was added, so the
contract is assertable from a test rather than eyeballed in output.

### Red (documented, with the actual wrong output)

New test `crates/codegraph-graph/tests/enqueue_once.rs` (4 tests). Red was
produced by stripping ONLY the guard (seeding `enqueued` empty, dropping both
checks and the insert) while KEEPING the instrumentation seam, so the failure is
a real assertion on real counts, not a compile error:

```text
running 4 tests
test find_path_does_not_re_enqueue_a_shared_successor_in_a_diamond ... FAILED
test find_path_enqueues_each_work_item_exactly_once_over_a_fan_in_hub ... FAILED

assertion `left == right` failed: a, b, c, d, e — `d` is reachable from both b
and c but must be enqueued ONCE: PathSearchStats { enqueued: 6, dequeued: 6,
duplicate_dequeues: 1 }
  left: 6
 right: 5

each work item must be enqueued at most once: enqueued 74 for 18 reachable
nodes (fan-in edges: 73) — stats PathSearchStats { enqueued: 74, dequeued: 74,
duplicate_dequeues: 56 }
```

`74` pushes for `18` reachable nodes — the count tracked the 73 fan-in edges, exactly
the quadratic the issue describes. With the guard restored: `18` pushes,
`0` duplicate dequeues (`cargo test --locked -p codegraph-graph --test
enqueue_once` → exit 0, 4 passed).

The other two tests pin what must NOT change and passed both ways: the shortest
of two routes still wins, an unreachable target still reports no path, and a
3-cycle still terminates with exactly 3 pushes.

### Golden row delta

None. `git diff --stat 29f635b..HEAD -- reference/golden/` → EMPTY.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**26 passed, 0 failed**. The oracle run left two untracked SQLite byproducts
(`reference/golden/metal/colby.db-{wal,shm}`), not gitignored; deleted before
staging. No tracked golden changed.

### Negative control, EXECUTED

The guard-strip above IS the negative control, run in both directions: guard
removed → exit 101 with the two assertions quoted verbatim; guard restored from
the saved copy → exit 0, 4 passed. Restored-file SHA-256 of
`crates/codegraph-graph/src/graph/mod.rs`:
`0a0a1d93dd1c5dd39f305c0c37ef66327791e547d5fbae46763ac06495c87c57`
(measured with `sha256sum` after the restore).

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo build --locked -p codegraph-graph --all-targets` → exit 0.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` → exit 0.
- `cargo test --locked -p codegraph-graph --test enqueue_once` → exit 0,
  4 passed.
- `cargo test --locked -p codegraph-graph -p codegraph-mcp -p codegraph-rs`
  → exit 0 (63 `test result: ok` lines, no FAILED).
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.

`lsp_diagnostics` again refused this worktree (`LSP file path must be inside
request cwd`); locked Cargo clippy/test is the honest fallback and no LSP-clean
result is claimed. No dependency, version or `Cargo.lock` byte changed. No
Windows/MSVC runtime validation is claimed — this work ran on Linux only.

---

## Batch A4 — `node <symbol> -f <file>` returns the pinned definition's source body (2026-07-28)

Ports upstream `ce983a0` (#1314). Base for this entry: `efc6fcd7` (Batch A3).
One commit: the CLI flag, the engine's symbol+file pin, and tests.

### Red (documented, with the actual wrong output)

Two distinct defects, both measured on the real binary built from `efc6fcd7`
against a temp project with TWO `setState` definitions (`src/alpha.ts` holding
`ALPHA_MARKER`, `src/beta.ts` holding `BETA_MARKER`):

1. **The CLI had no `-f` at all.** `codegraph node setState -f src/beta.ts`:

```text
error: unexpected argument '-f' found
  tip: to pass '-f' as a value, use '-- -f'
Usage: codegraph node [OPTIONS] <TARGET>
```

The tool's own `codegraph_node` schema says "pass `file`/`line` to pin one", and
the ambiguity render tells the agent to pick one — but the shell had no way to.

2. **The MCP tool IGNORED `file` when `symbol` was present.** `handle_node` only
   consulted `file_hint` when `symbol` was ABSENT (`if symbol_raw.is_none() &&
let Some(file_hint)`), so a pinned request fell through to the unpinned path.
   Driven over real stdio with
   `{"symbol":"setState","file":"src/beta.ts","includeCode":true}`:

```text
**2 definitions named "setState"**
Returning 2 in full — pick the one you need (no Read required).

## setState (function)
**Location:** src/alpha.ts:1
...  const ALPHA_MARKER = next + 1;
---
## setState (function)
**Location:** src/beta.ts:1
...  const BETA_MARKER = next + "!";
```

Both overloads, i.e. the pin did nothing. `cargo test --locked -p codegraph-rs
--test node_file_pin_cli` → exit 101, 4 of 5 failing (the 5th pins the UNPINNED
behaviour and passed both before and after).

### Green (minimal)

- `crates/codegraph-cli/src/main.rs`: new `-f/--file` on `Command::Node`, wired
  through `cmd_node`. When present it sends
  `{"symbol": target, "file": file, "includeCode": true}` — carrying
  `includeCode` exactly like the bare-symbol branch, which is the upstream's
  named root cause. File-view mode and the bare-symbol path are untouched.
- `crates/codegraph-mcp/src/engine.rs`: `handle_node` now filters its matches by
  the `file` hint when both `symbol` and `file` are given. One survivor renders
  as a single definition (body + trail); several survivors render the ambiguity
  view over the SURVIVORS only; zero survivors returns a not-found
  `Symbol "X" not found in "<hint>"` rather than falling back to an arbitrary
  overload — the same "silent miss beats a wrong answer" rule as Batch C.
- New `file_path_matches_hint`: exact repo-relative path, a path suffix on a
  SEGMENT boundary, or a bare basename; `\` normalized to `/` so a
  Windows-style hint pins the same node. `src/myauth/session.ts` is NOT matched
  by `auth/session.ts`, and `mysession.ts` is NOT matched by `session.ts`.

### Proof through the REAL user-facing surface

    $ codegraph node setState -f src/beta.ts
    ## setState (function)
    **Location:** src/beta.ts:1
    **Signature:** `(next: string): string`
    1	export function setState(next: string): string {
    2	  const BETA_MARKER = next + "!";
    3	  return BETA_MARKER;
    4	}

    $ codegraph node setState -f alpha.ts        # basename pin
    ## setState (function)
    **Location:** src/alpha.ts:1
    1	export function setState(next: number): number {
    2	  const ALPHA_MARKER = next + 1;
    3	  return ALPHA_MARKER;
    4	}

    $ codegraph node setState -f src/nowhere.ts
    Symbol "setState" not found in "src/nowhere.ts"

The source body appears in stdout, and only the pinned overload's.

### Golden row delta

None. `git diff --stat 29f635b..HEAD -- reference/golden/` → EMPTY;
`git status --short reference/golden/` → empty.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**26 passed, 0 failed**. The 15 golden MCP fixtures still reach parity
(`rmcp_parity`, `golden_mcp` green): none of them passes `symbol`+`file`, so the
new branch is unreachable for them.

### Negative control, EXECUTED

`git stash push -- crates/codegraph-mcp/src/engine.rs
crates/codegraph-cli/src/main.rs` (tests kept) →
`cargo test --locked -p codegraph-rs --test node_file_pin_cli` → exit 101 with 4
of 5 RED (the unpinned-behaviour test still ok). `git stash pop` restored both;
restored SHA-256:

- `crates/codegraph-mcp/src/engine.rs`
  `ad925f79c47c3660b2a0406cf173018cf91560d046fb54f3a2282e1c82e0747a`
- `crates/codegraph-cli/src/main.rs`
  `aa580663f7c1ee8a7dc4f5cf41676a8c33946929ff89353c4ecee2c15b936231`

Re-run with the fix restored → exit 0, 5 passed.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` → exit 0.
- `cargo test --locked -p codegraph-rs --test node_file_pin_cli` → exit 0,
  5 passed.
- `cargo test --locked -p codegraph-mcp --lib file_path_matches_hint…` → exit 0.
- `cargo test --locked -p codegraph-mcp --lib ext_node_symbol_plus_file…` →
  exit 0.
- `cargo test --locked -p codegraph-mcp -p codegraph-rs` → **exit 101**, failing
  ONLY `codegraph-rs formatter_and_env_tests::install_completions_writes_zsh_
fish_elvish_into_home` — the KNOWN pre-existing in-process `HOME`/
  `XDG_DATA_HOME` race documented in the Batch C2/C3 entries, unrelated to the
  `node` command. NOT weakened, skipped, `#[ignore]`d or modified. Re-run in
  isolation (`--bin codegraph <that test>`) → exit 0, 1 passed.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.
- `make ci CARGO='cargo --locked'` was run repeatedly as the final Batch A gate.
  The LAST run, executed after the last byte of this commit (code, tests and this
  prose, then `make fmt`), exited **0** with `✅ All CI checks passed!`. Earlier
  runs in the same session hit two KNOWN pre-existing flakes, both reported here
  rather than hidden:
  - `codegraph-rs formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home`
    — the in-process `HOME`/`XDG_DATA_HOME` race documented in the Batch C2/C3
    entries (2 of 6 runs). Passes in isolation: exit 0, 1 passed.
  - `codegraph-rs batch_m_legacy_extension_override::verify_legacy_binary_rejects_a_wrong_executable_version`
    — fails with `run configured legacy binary …/stub-legacy --version: Text
file busy (os error 26)` (2 of 6 runs). The test writes a stub executable
    and immediately execs it; under a loaded multi-threaded workspace run the
    write handle can still be open. Run in isolation SIX consecutive times: exit
    0 every time, zero `Text file busy` hits. This test file is NOT in Batch A's
    diff (`git diff --name-only 29f635b..HEAD` does not list it); its last commit
    is `25b78a8`, before this batch.
    Neither test was weakened, skipped, `#[ignore]`d, given a sleep, or modified in
    any way. Gate runs left untracked SQLite byproducts under `reference/golden/`
    (`metal/`, `solidity/` `colby.db-{wal,shm}`), which are not gitignored; they
    were deleted before staging and no tracked golden changed.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

`lsp_diagnostics` again refused this worktree (`LSP file path must be inside
request cwd`); locked Cargo clippy/test is the honest fallback and no LSP-clean
result is claimed. No dependency, version or `Cargo.lock` byte changed. No
Windows/MSVC runtime validation is claimed — the `\`-normalization is asserted
by a unit test on Linux only.

```

```

## Batch D1 — the `<Route>` opening-tag scan is bounded by the tag itself, not by 400 fixed bytes (2026-07-28)

Ports upstream issue **#1348**. Investigated first, and the located scan was
**NOT** already bounded in the sense the issue means: it had a fixed 400-byte
forward window that never stopped at the tag's own end.

### The located scan, quoted

Not in `crates/codegraph-extract/src/embedded/` and not in the `lang/{jsx,tsx}.rs`
specs (those only delegate to the JavaScript spec) — the scan is in the React
framework resolver, `crates/codegraph-resolve/src/frameworks/react.rs:152-155`
(pre-change):

```rust
// React Router <Route .../> (v5/v6) (react.ts:158-198).
for tag in route_tag_regex().find_iter(content) {
    let window = byte_window(content, tag.start(), 400);
    let Some(path_match) = route_path_attr().captures(window) else {
```

with `byte_window` at `react.rs:515-521`:

```rust
fn byte_window(content: &str, start: usize, max_bytes: usize) -> &str {
    let mut end = start.saturating_add(max_bytes).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[start..end]
}
```

So the scan was byte-capped (no unbounded read, no quadratic blow-up) but
**never cut at the opening tag's terminator**. That is the #1348 defect: the cap
was the _only_ bound, so one tag's window bled into its siblings' attributes.
The call site reached is real: `ReferenceResolver::extract_and_persist_frameworks_with`
(`resolver.rs:836-845`) → `ReactResolver::extract`, driven from
`codegraph-cli/src/main.rs:4081` and `codegraph-watch/src/sync.rs:453`.

### Measured Red — real indexed project, ground truth read from SQLite

Scratch project `/tmp/d1-red` (`package.json` with a `react` dep, `src/App.js`
carrying the standard v6 nested shape from the issue: a parent `path="/dashboard"`
that renders `<Outlet/>`, a pathless `index` child, and a `path="settings"`
sibling). `target/debug/codegraph init .`, then
`sqlite3 .codegraph-v2/codegraph.db`:

    $ sqlite3 … "select kind,name,start_line from nodes where kind='route' order by start_line;"
    route|/dashboard|9
    route|settings|10        <- WRONG: this is the pathless index route; it borrowed
    route|settings|11           its sibling's path, so `settings` is duplicated

    $ sqlite3 … "select (select name from nodes where id=e.source),
                        (select name from nodes where id=e.target), e.line
                 from edges e where e.source like 'route:%';"
    /dashboard|DashboardHome|9   <- WRONG: /dashboard has no element of its own
    settings|DashboardHome|10    <- WRONG pair: index route mislabeled `settings`
    settings|Settings|11         <- correct

3 route nodes where 2 are right, and **2 of 3 route→component edges wrong**,
matching the upstream report edge for edge. This is observed database state, not
a compile or setup failure.

Pathological input measured too: `/tmp/d1-path/src/patho.js` — an unterminated
`<Route path="/a"` (no `>`), then a 200 000-character run of `<`, then a
well-formed `<Route path="/b" element={<Comp/>} />` (200 129 bytes total). The
byte cap meant the old code did NOT hang (0.317 s wall clock, `Indexed 1 files`),
so the honest finding is: **no unbounded scan, but wrong attribution** — the `/a`
tag reached `Comp` in the unit-level reproduction (see the negative control
below, where lifting the cap yields `[("Comp", 2), ("Comp", 4)]` instead of
`[("Comp", 4)]`).

### Green

`route_opening_tag_window` replaces the raw 400-byte window. It bounds the scan
twice: `opening_tag_end` walks to the tag's own `>` (skipping `{…}` expression
containers and quoted values, so `element={<Comp/>}` and `path="a>b"` do not end
the tag early), and `ROUTE_OPENING_TAG_SCAN_LIMIT` caps the search itself so an
unterminated tag can never scan to end-of-file. A malformed tag additionally
stops at the next `<Route`, so even then it cannot borrow a sibling's attributes.

Same project re-indexed with the rebuilt binary:

    route|/dashboard|9
    route|settings|11
    settings|Settings|11

Exactly the single correct edge the issue asks for **for the `<Route>` JSX form**.

> **Scope correction (added by Batch F3).** As committed, this batch fixed only the
> `<Route>` JSX form. The data-router (`createBrowserRouter([…])`) form kept the same
> borrowing defect behind a fixed 300-byte window, so #1348 was NOT fully ported by
> Batch D1 alone. Batch F3 below closes that half; #1348 is complete only with both
> batches. The sentence above is scoped accordingly.

### The bound and why that number

`ROUTE_OPENING_TAG_SCAN_LIMIT = 2048`, justified in its doc comment from tag
lengths actually measured on this branch (true length to the brace-aware
terminator, computed with a Python walker over each fixture):

| Opening tag shape                                                           | Bytes |
| --------------------------------------------------------------------------- | ----- |
| lazy, error-bounded v6 data route over six attribute lines (`/tmp/d1-long`) | 278   |
| the same plus `hydrateFallbackElement`, `shouldRevalidate`, `handle={{…}}`  | 525   |
| prettier-wrapped tag with twelve extra `data-*` attributes (`/tmp/d1-wide`) | 700   |

2048 is ~3x the widest measured tag, and strictly wider than the 400-byte window
it replaces, so nothing the old code could reach is lost. The first draft used
512; the 700-byte measurement showed 512 would truncate a legitimate tag, so the
constant was raised before commit rather than after.

### Tests

Three new tests in `crates/codegraph-resolve/tests/frameworks.rs`, all driving
the real `ReactResolver::extract`:

- `react_route_window_stops_at_tag_end_not_at_sibling_routes` — the #1348 nested
  shape; asserts the exact route-node list AND the exact reference list.
- `react_route_window_is_bounded_for_unterminated_tag_and_bare_angle_run` — the
  pathological input: unterminated `<Route path="/a"`, a 200 000-char run of `<`,
  then a well-formed route; asserts `/a` does not reach the far-away element.
- `react_route_window_keeps_long_multiline_opening_tag_intact` — a 705-byte
  prettier-wrapped tag with twelve props; asserts the fixture exceeds 400 bytes
  and that both its `path` and its `element` still extract. This pins the bound
  from the outside: tightening the constant below the tag length reddens it.

### Golden row delta

None. `git diff --stat 7d0253b..HEAD -- reference/golden/` → EMPTY.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**26 passed, 0 failed**. No golden fixture contains a `<Route`/`createBrowserRouter`
construct (`grep -rn '<Route\|createBrowserRouter' reference/golden/ crates/codegraph-bench/fixtures/`
→ no matches), so the new branch is unreachable for them. The equivalence run left
untracked `reference/golden/godot/colby.db-{wal,shm}`; both were deleted before
staging and no tracked golden changed.

### Negative control, EXECUTED (three mutants)

1. Production line reverted to `byte_window(content, tag.start(), 400)`, tests
   untouched → `cargo test --locked -p codegraph-resolve --test frameworks` →
   **exit 101**, 2 failed. Actual assertion output:

       assertion `left == right` failed: the pathless index route must not borrow its sibling's path
         left: [("/dashboard", 6), ("settings", 7), ("settings", 8)]
        right: [("/dashboard", 6), ("settings", 8)]

   plus `react_route_window_keeps_long_multiline_opening_tag_intact` FAILED.

2. Cap lifted to `usize::MAX` (terminator logic kept) → **exit 101**, 1 failed:

       assertion `left == right` failed: the unterminated /a tag must not reach the /b element 200KB later
         left: [("Comp", 2), ("Comp", 4)]
        right: [("Comp", 4)]

   — proof the cap itself is load-bearing, not decoration.

3. Cap tightened to `400` → **exit 101**, 1 failed:
   `the element of a 705-byte opening tag must still be seen` — proof the bound
   cannot be shrunk without truncating legitimate input.

Restored after each mutant by copying back the green file; restored SHA-256 of
`crates/codegraph-resolve/src/frameworks/react.rs`:
`a4bbc7a36305bc8bd901b15daf867a3546b94ea76dc854afa86e90f263f8bb7d`.
Re-run with the fix restored → exit 0, 31 passed.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, run before every Cargo
  batch; every Cargo command used `--locked`.
- `cargo build --locked -p codegraph-rs` → exit 0 (binary used for the SQLite
  ground-truth measurements above).
- `cargo test --locked -p codegraph-resolve --test frameworks` → exit 0,
  31 passed.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  commit (code, tests, this prose, then `make fmt`); result recorded below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path
must be inside request cwd`); locked Cargo clippy/test is the honest fallback and
no LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte
changed. No native Windows/MSVC runtime validation is claimed — everything above
ran on Linux.

## Batch D2 — every DFM object closes at its own matching `end` (2026-07-28)

Ports upstream issue **#1350**. Investigated first, then MEASURED against real
indexed database state. Finding, split honestly in two halves:

- **Nesting was ALREADY correct.** The extractor already keeps an `object … end`
  stack and parents each new object to `stack.last()`, so a nested object never
  attached to the file root. Measured to four levels deep with siblings at two
  depths — every `contains` edge was already right. This half is a
  **reclassification: no defect, pinned by a new regression test.**
- **`end_line` was WRONG for every object.** Each component reported
  `end_line == start_line`, i.e. a one-line span, so no container's span covered
  its children and no object reached its own `end`. This half is a **real
  defect, fixed.**

### The located logic, quoted

`crates/codegraph-extract/src/embedded/dfm.rs:59-131` (pre-change). The stack
held bare id strings, so once an object was pushed there was no way back to its
node to close it — the `end` branch popped and discarded:

```rust
let mut stack = vec![file_id.to_string()];
…
if let Some(captures) = object_re.captures(line) {
    let name = captures.get(2).unwrap().as_str().to_string();
    let type_name = captures.get(3).unwrap().as_str().to_string();
    let mut node = default_node(
        self.file_path,
        Language::Pascal,
        NodeKind::Component,
        name.clone(),
        format!("{}#{name}", self.file_path),
        line_num,
        line_num,   // <- end_line seeded to the HEADER line and never updated
        0,
        line.len() as i64,
    );
    node.signature = Some(type_name);
    let node_id = node.id.clone();
    result.nodes.push(node);
    result
        .edges
        .push(contains_edge(stack.last().unwrap(), &node_id));
    stack.push(node_id);
    continue;
}
…
if end_re.is_match(line) && stack.len() > 1 {
    stack.pop();   // <- the matching `end` line is discarded
}
```

### Measured Red — real indexed project, ground truth read from SQLite

Scratch project `/tmp/d2-red` with two fixtures: `src/Deep.dfm` (four levels —
`MainForm` > `TopPanel` > `InnerPanel` > `DeepButton` — plus sibling pairs at two
depths and a multiline `Columns = <…>` block whose `end` / `end>` lines must not
close anything) and `src/Broken.dfm` (truncated: three objects, zero `end`).
Indexed with the pre-change `target/debug/codegraph`, then:

    sqlite3 .codegraph-v2/codegraph.db \
      "select kind, name, qualified_name, start_line, end_line, file_path \
       from nodes order by file_path, start_line;"

Observed (pre-change), verbatim:

    file|Broken.dfm|src/Broken.dfm|1|7|src/Broken.dfm
    component|BrokenForm|src/Broken.dfm#BrokenForm|1|1|src/Broken.dfm
    component|OrphanPanel|src/Broken.dfm#OrphanPanel|3|3|src/Broken.dfm
    component|LostButton|src/Broken.dfm#LostButton|5|5|src/Broken.dfm
    file|Deep.dfm|src/Deep.dfm|1|34|src/Deep.dfm
    component|MainForm|src/Deep.dfm#MainForm|1|1|src/Deep.dfm
    component|TopPanel|src/Deep.dfm#TopPanel|4|4|src/Deep.dfm
    component|InnerPanel|src/Deep.dfm#InnerPanel|6|6|src/Deep.dfm
    component|DeepButton|src/Deep.dfm#DeepButton|8|8|src/Deep.dfm
    component|DeepLabel|src/Deep.dfm#DeepLabel|12|12|src/Deep.dfm
    component|SiblingButton|src/Deep.dfm#SiblingButton|16|16|src/Deep.dfm
    component|BottomPanel|src/Deep.dfm#BottomPanel|20|20|src/Deep.dfm
    component|Items|src/Deep.dfm#Items|22|22|src/Deep.dfm

**Wrong rows: all 8 `Deep.dfm` components.** Every one reports
`end_line == start_line`. Ground truth from `cat -n src/Deep.dfm`: `MainForm`
ends at 33, `TopPanel` at 19, `InnerPanel` at 15, `DeepButton` at 11,
`DeepLabel` at 14, `SiblingButton` at 18, `BottomPanel` at 32, `Items` at 31.
So a caller asking "what lines is `TopPanel`?" was told line 4 only, and no
container's span contained its children. This is observed database state, not a
compile or setup failure.

`qualified_name` was NOT flattened — it is `{file}#{name}` by upstream design
(`dfm-extractor.ts`), and the existing test already pins that form, so it is not
touched here.

Nesting, measured in the same run:

    contains|Broken.dfm@1|BrokenForm@1
    contains|BrokenForm@1|OrphanPanel@3
    contains|OrphanPanel@3|LostButton@5
    contains|Deep.dfm@1|MainForm@1
    contains|MainForm@1|TopPanel@4
    contains|TopPanel@4|InnerPanel@6
    contains|InnerPanel@6|DeepButton@8
    contains|InnerPanel@6|DeepLabel@12
    contains|TopPanel@4|SiblingButton@16
    contains|MainForm@1|BottomPanel@20
    contains|BottomPanel@20|Items@22

Every edge correct, including at depth 3 and across the multiline block —
**already right**, hence the reclassification above. Reported as measured rather
than restated as a defect.

### Green

`OpenBlock { id, node_index }` replaces the bare id on the stack: the file root
carries `node_index: None`, each real object carries its index in
`result.nodes`. The `end` branch now closes the block it actually popped:

```rust
if end_re.is_match(line) && stack.len() > 1 {
    let closed = stack.pop().unwrap();
    if let Some(index) = closed.node_index {
        result.nodes[index].end_line = line_num;
    }
}
```

Because the `end_line` is written from the popped frame, the value can only ever
come from that object's own matching `end` — a sibling's terminator is
structurally unreachable. Same project re-indexed with the rebuilt binary:

    file|Broken.dfm|src/Broken.dfm|1|7|src/Broken.dfm
    component|BrokenForm|src/Broken.dfm#BrokenForm|1|1|src/Broken.dfm
    component|OrphanPanel|src/Broken.dfm#OrphanPanel|3|3|src/Broken.dfm
    component|LostButton|src/Broken.dfm#LostButton|5|5|src/Broken.dfm
    file|Deep.dfm|src/Deep.dfm|1|34|src/Deep.dfm
    component|MainForm|src/Deep.dfm#MainForm|1|33|src/Deep.dfm
    component|TopPanel|src/Deep.dfm#TopPanel|4|19|src/Deep.dfm
    component|InnerPanel|src/Deep.dfm#InnerPanel|6|15|src/Deep.dfm
    component|DeepButton|src/Deep.dfm#DeepButton|8|11|src/Deep.dfm
    component|DeepLabel|src/Deep.dfm#DeepLabel|12|14|src/Deep.dfm
    component|SiblingButton|src/Deep.dfm#SiblingButton|16|18|src/Deep.dfm
    component|BottomPanel|src/Deep.dfm#BottomPanel|20|32|src/Deep.dfm
    component|Items|src/Deep.dfm#Items|22|31|src/Deep.dfm

All 8 spans now match `cat -n` exactly. The `contains` edge list is byte-identical
to the Red run (re-queried and compared) — nesting was not disturbed.

### Unterminated objects: honest omission, not a fabricated span

`Broken.dfm`'s three objects never reach an `end`, so nothing pops them and their
`end_line` stays at the header line. That is deliberate, following the precedent
that an undeterminable target emits nothing rather than a guess: the extractor
does not claim a span it never observed, does not borrow the file end (7), and
does not panic. `result.errors` stays empty and the `contains` chain is still
built, so the structure is still usable.

### `start_line` invariance — node-ID safety

Node IDs are `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}` where
`line` is `start_line` (`shared::default_node` → `generate_node_id(file_path,
kind, &name, start_line.max(1) as u32)`). **This change writes only `end_line`.**
The `default_node(…)` call — including both line arguments and therefore the id —
is untouched; the single new write is `result.nodes[index].end_line = line_num;`
after the node already exists. No `start_line`, `name`, `kind`, `file_path` or
`qualified_name` value changes, so **no node ID shifts**. Confirmed against the
measured tables above: every `start_line` in the Green table equals the Red one.
An earlier draft also updated `end_column`; that was reverted before commit to
keep the diff to the one field the issue is about.

### Tests

Two new tests in `crates/codegraph-extract/tests/markup_risk_languages.rs`,
driving the real `extract_source` through `extract_fixture`, plus two new
fixtures (`DeepForm.dfm`, `BrokenForm.dfm`; no existing fixture reflowed, so no
existing `start_line` moved):

- `lang_markup_risk_dfm_spans_close_at_their_own_matching_end` — asserts the
  EXACT `(name, start_line, end_line)` triple for all 8 objects, then repeats the
  depth-2 (`InnerPanel` 6..15) and depth-3 (`DeepButton` 8..11) spans separately,
  asserts the same-depth sibling pair (`DeepButton` 8..11 vs `DeepLabel` 12..14)
  plus a non-overlap assertion so an off-by-one that swaps their terminators is
  caught, pins `Items` 22..31 across the multiline `Columns = <…>` block, and
  re-asserts all 8 `contains` edges and two handler reference lines.
- `lang_markup_risk_dfm_unterminated_object_keeps_its_header_span` — the
  truncated fixture; asserts the exact three spans (`1..1`, `3..3`, `5..5`), that
  `errors` is empty (no panic, no error node), and that the `contains` chain is
  still complete.

The existing `lang_markup_risk_dfm_uses_custom_component_extractor` is unchanged
and still green, so the ported behavior did not disturb `MainForm.dfm`.

### Golden row delta

None. `git diff --stat b7ff1f8..HEAD -- reference/golden/` → **EMPTY** (exit 0).
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**26 passed, 0 failed**. No golden corpus or bench fixture contains a `.dfm` or
`.fmx` file (`find reference/golden crates/codegraph-bench/fixtures -iname '*.dfm'
-o -iname '*.fmx'` → no matches), so the DFM extractor is unreachable for them,
matching the frozen manifest's `#1350 → golden effect: none`. The equivalence run
left untracked `reference/golden/ruby/colby.db-{wal,shm}`; both were deleted
before staging and no tracked golden changed.

### Negative control, EXECUTED (three mutants)

Each mutant is a change to `dfm.rs` ONLY; the tests were never touched.

1. `end_line` write removed (exact pre-#1350 behavior: pop and discard) →
   `cargo test --locked -p codegraph-extract --test markup_risk_languages` →
   **exit 101**, 1 failed:

       assertion `left == right` failed: each object must span from its own header line to its own matching end
         left: [("BottomPanel", 20, 20), ("DeepButton", 8, 8), ("DeepForm", 1, 1), ("DeepLabel", 12, 12), ("InnerPanel", 6, 6), ("Items", 22, 22), ("SiblingButton", 16, 16), ("TopPanel", 4, 4)]
        right: [("BottomPanel", 20, 32), ("DeepButton", 8, 11), ("DeepForm", 1, 33), ("DeepLabel", 12, 14), ("InnerPanel", 6, 15), ("Items", 22, 31), ("SiblingButton", 16, 18), ("TopPanel", 4, 19)]

2. `end_line = line_num - 1` (off-by-one at the terminator) → **exit 101**,
   1 failed — proof the exact terminator line is pinned, not merely "some larger
   number":

       assertion `left == right` failed: each object must span from its own header line to its own matching end
         left: [("BottomPanel", 20, 31), ("DeepButton", 8, 10), ("DeepForm", 1, 32), ("DeepLabel", 12, 13), ("InnerPanel", 6, 14), ("Items", 22, 30), ("SiblingButton", 16, 17), ("TopPanel", 4, 18)]
        right: [("BottomPanel", 20, 32), ("DeepButton", 8, 11), ("DeepForm", 1, 33), ("DeepLabel", 12, 14), ("InnerPanel", 6, 15), ("Items", 22, 31), ("SiblingButton", 16, 18), ("TopPanel", 4, 19)]

3. Unterminated objects post-patched to reach the file end (the tempting
   "fill in a plausible span" design) → **exit 101**, and it reddens a
   DIFFERENT test, so a partial regression cannot hide:

       assertion `left == right` failed: an unterminated object must not fabricate a span
         left: [("BrokenForm", 1, 8), ("LostButton", 5, 8), ("OrphanPanel", 3, 8)]
        right: [("BrokenForm", 1, 1), ("LostButton", 5, 5), ("OrphanPanel", 3, 3)]

Restored after each mutant by copying back the green file; restored SHA-256 of
`crates/codegraph-extract/src/embedded/dfm.rs`:
`a8c3db6949c395071b5ecabc7c74a80515927d23c84cd2d0b59afbaa4c1a86ab`.
Re-run with the fix restored → exit 0, 9 passed.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, run before every Cargo
  batch; every Cargo command used `--locked`.
- `cargo build --locked -p codegraph-rs` → exit 0 (binary used for the SQLite
  ground-truth measurements above, pre- and post-change).
- `cargo test --locked -p codegraph-extract --test markup_risk_languages` →
  exit 0, 9 passed.
- `cargo test --locked -p codegraph-extract` (whole crate) → exit 0, all suites
  green (382 lib tests + integration suites).
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  commit (code, tests, fixtures, this prose, then `make fmt`); result recorded
  below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

Determinism: the fix adds no map iteration and no new sort — objects are still
emitted in source-line order and the `end_line` write targets an already-fixed
index, so output ordering is unchanged.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path
must be inside request cwd`); locked Cargo clippy/test is the honest fallback and
no LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte
changed. No native Windows/MSVC runtime validation is claimed — everything above
ran on Linux.

## Batch D3 — both mapper dialects are recognized and a qualified `refid` keeps its namespace (2026-07-28)

Ports upstream issues **#1182** (MyBatis/iBatis mapper forms) and **#1209**
(qualified `<include refid>` resolution) as ONE commit: they share the single
extractor `crates/codegraph-extract/src/embedded/mybatis.rs`, and the frozen
manifest lists them as one row (`| #1182/#1209 | PORT | Batch D3 MyBatis/iBatis
forms and qualified refids | none |`).

Investigated first, then MEASURED against real indexed database state. BOTH
halves were real defects — neither was already correct.

### The located logic, quoted

**Dialect detection** — `mybatis.rs:38-53` (pre-change). One literal root tag,
and a hard-coded closing tag independent of it:

```rust
fn find_mapper_root(&self) -> Option<(String, usize, usize)> {
    let open_re = Regex::new(r#"<mapper\b([^>]*)>"#).unwrap();
    let ns_re = Regex::new(r#"\bnamespace\s*=\s*"([^"]+)""#).unwrap();
    let open = open_re.find(self.source)?;
    …
    let body_end = self.source[body_start..]
        .find("</mapper>")
        .map_or(self.source.len(), |idx| body_start + idx);
```

`extract` calls it with `if let Some(…) = self.find_mapper_root()`, so a root
that is not literally `<mapper` yields `None` and the whole mapper body is
skipped — the file node is the only output.

**`refid` matching** — `mybatis.rs:131-150` (pre-change). Any dot at all meant
"rewrite every dot to `::`":

```rust
let ref_qualified = if refid.contains('.') {
    refid.replace('.', "::")
} else {
    format!("{namespace}::{refid}")
};
```

The nodes it must match are built at `mybatis.rs:95` as
`let qualified = format!("{namespace}::{id}");` — the namespace keeps its OWN
dots. So `com.example.UserMapper.baseColumns` became
`com::example::UserMapper::baseColumns`, which no node carries.

### The measured Red (SQLite ground truth, pre-change binary)

Scratch project `/tmp/d3pre` — a MyBatis mapper with five statements (bare
refid, self-qualified refid, foreign-namespace refid, a DECOY refid naming
another namespace's same-`id` fragment, and an unresolvable refid), a second
mapper owning the decoy, and an iBatis `<sqlMap>` file. Built from
`crates/codegraph-extract/src/embedded/mybatis.rs` SHA-256
`725e83497536a315463da4318dbd8417a50a0fde27d9117d32f7549e2fc9f0a4`.

`select kind, name, qualified_name, start_line, end_line, file_path from nodes
order by file_path, start_line;`

```
file|LegacySqlMap.xml|src/LegacySqlMap.xml|1|11|src/LegacySqlMap.xml
file|OrderMapper.xml|src/OrderMapper.xml|1|10|src/OrderMapper.xml
method|baseColumns|com.example.OrderMapper::baseColumns|3|5|src/OrderMapper.xml
method|orderColumns|com.example.OrderMapper::orderColumns|6|8|src/OrderMapper.xml
file|UserMapper.xml|src/UserMapper.xml|1|22|src/UserMapper.xml
method|baseColumns|com.example.UserMapper::baseColumns|3|5|src/UserMapper.xml
method|findLocal|com.example.UserMapper::findLocal|6|8|src/UserMapper.xml
method|findQualified|com.example.UserMapper::findQualified|9|11|src/UserMapper.xml
method|findCross|com.example.UserMapper::findCross|12|14|src/UserMapper.xml
method|findDecoy|com.example.UserMapper::findDecoy|15|17|src/UserMapper.xml
method|findMissing|com.example.UserMapper::findMissing|18|20|src/UserMapper.xml
```

RED #1 (#1182): `src/LegacySqlMap.xml` contributes ONLY a file node. Its
`<sql id="legacyColumns">` and `<select id="legacySelect">` produce nothing —
the iBatis form is invisible.

`select kind, source, target, line from edges where kind!='contains' order by
source;`

```
references|method:bf6ee95a109cb69fa818daa03aa4122e|method:828b1d45f6bc1f7b0562a33ab633f28a|7
```

RED #2 (#1209): exactly ONE include edge — the bare `refid` on line 7. All three
namespace-qualified includes produce no edge at all.

`select reference_name, line, file_path from unresolved_refs order by file_path,
line;`

```
com::example::UserMapper::baseColumns|10|src/UserMapper.xml
com::example::OrderMapper::orderColumns|13|src/UserMapper.xml
com::example::OrderMapper::baseColumns|16|src/UserMapper.xml
com::example::NopeMapper::baseColumns|19|src/UserMapper.xml
```

This is the direct proof of the mechanism: every qualified refid was rewritten to
`com::example::…`, a name that matches no `qualified_name` in the graph, so all
four stayed unresolved — including line 10, which names the fragment sitting three
lines above it in the SAME file.

### The Green (same fixtures, post-change binary)

`crates/codegraph-extract/src/embedded/mybatis.rs` SHA-256
`1550b1da1eaa6476c5b451e0f8b30aa2abe693eeccad70e08b4efa7d96f7f7ae`, scratch
project `/tmp/d3post`, same sources.

```
file|LegacySqlMap.xml|src/LegacySqlMap.xml|1|11|src/LegacySqlMap.xml
method|legacyColumns|Legacy::legacyColumns|4|6|src/LegacySqlMap.xml
method|legacySelect|Legacy::legacySelect|7|9|src/LegacySqlMap.xml
file|OrderMapper.xml|src/OrderMapper.xml|1|10|src/OrderMapper.xml
method|baseColumns|com.example.OrderMapper::baseColumns|3|5|src/OrderMapper.xml
method|orderColumns|com.example.OrderMapper::orderColumns|6|8|src/OrderMapper.xml
file|UserMapper.xml|src/UserMapper.xml|1|22|src/UserMapper.xml
method|baseColumns|com.example.UserMapper::baseColumns|3|5|src/UserMapper.xml
method|findLocal|com.example.UserMapper::findLocal|6|8|src/UserMapper.xml
method|findQualified|com.example.UserMapper::findQualified|9|11|src/UserMapper.xml
method|findCross|com.example.UserMapper::findCross|12|14|src/UserMapper.xml
method|findDecoy|com.example.UserMapper::findDecoy|15|17|src/UserMapper.xml
method|findMissing|com.example.UserMapper::findMissing|18|20|src/UserMapper.xml
```

```
references|method:45d8eacff46c2aeb1379cee54963b101|method:0b3a72ef37c2e01cfe46e661ff8f8df6|13
references|method:bf6ee95a109cb69fa818daa03aa4122e|method:828b1d45f6bc1f7b0562a33ab633f28a|7
references|method:c2ed2a8d2c281cada1850566cff7b94d|method:148f6dfc5ea8f4cef10862224874d23f|16
references|method:d8364a506d91720a65866d24e14d1d03|method:376bcb423ecbaeef2d07eda51acfeb42|8
references|method:d9d21bd0b97b87d2c1eff47088ee900a|method:828b1d45f6bc1f7b0562a33ab633f28a|10
```

```
com.example.NopeMapper::baseColumns|19|src/UserMapper.xml
```

Read against the node table, each edge lands where it must:

- line 7, bare `refid` → `method:828b1d45…` = `com.example.UserMapper::baseColumns`,
  its OWN mapper's fragment, NOT the same-`id` decoy in OrderMapper.
- line 10, self-qualified `refid` → the same `method:828b1d45…`. A bare refid and
  the same fragment spelled out in full now name one node.
- line 13, foreign `refid` → `method:0b3a72ef…` = `com.example.OrderMapper::orderColumns`,
  crossing namespaces correctly.
- line 16, the DECOY → `method:148f6dfc…` = `com.example.OrderMapper::baseColumns`.
  The refid names OrderMapper, so OrderMapper's fragment wins over UserMapper's
  identically-`id`'d one. A naive unqualified match cannot distinguish these two.
- line 19, the unresolvable `refid` → **no edge**, and it stays in
  `unresolved_refs`. It does NOT bind to either existing `baseColumns`. This
  upholds the precedent already set by B1, C2 and A4: when the target cannot be
  determined, emit NOTHING rather than guess among same-named candidates. A wrong
  `include` edge is worse than a missing one.
- `src/LegacySqlMap.xml` line 8 → `method:d8364a…` → `method:376bcb…`, the
  iBatis fragment. The `<sqlMap>` file now contributes two method nodes
  (`Legacy::legacyColumns`, `Legacy::legacySelect`) plus a working include.

### Node-ID safety — explicit statement

Node IDs are `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}` with
`line` = `start_line`.

**No `start_line` and no `name` changes for any node that existed before.** Diff
the pre and post node tables above: every MyBatis node keeps its exact
`start_line` and `name`, and the IDs are byte-identical across the two runs —
`method:828b1d45f6bc1f7b0562a33ab633f28a`, `method:148f6dfc5ea8f4cef10862224874d23f`,
`method:0b3a72ef37c2e01cfe46e661ff8f8df6`, `method:bf6ee95a109cb69fa818daa03aa4122e`,
`method:45d8eacff46c2aeb1379cee54963b101`, `method:c2ed2a8d2c281cada1850566cff7b94d`,
`method:9eb467f1b674a36bdd30b4585a09f0a9` all appear in BOTH runs.

The dialect change only ADDS nodes, for `<sqlMap>` files that previously produced
none, at their own real source lines. The refid change touches
`unresolved_ref.reference_name` only — never a node's `name`, `start_line`, or
kind. So this port is NOT golden-affecting, matching the frozen manifest's
`none`.

### Golden delta

`GIT_MASTER=1 git diff --stat 1e1d259..HEAD -- reference/golden/` → **EMPTY**, as
the manifest requires. No golden fixture was regenerated, and none needed to be:
no golden corpus contains any `.xml` file (`find crates/codegraph-bench/fixtures
-type f` yields extensions `c cfc cfm cfs cpp cu erl ets gd godot h hpp metal nix
py rb sol tf ts tscn uid` — no `xml`), so the MyBatis extractor is not exercised
by the equivalence oracle at all.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0, **26/26**.

### Tests added

`crates/codegraph-extract/tests/embedded_languages.rs` (extraction shape):

- `mybatis_accepts_the_ibatis_sqlmap_root_form` — the `<sqlMap>` fixture yields
  its `<sql>`/`<select>`/`<update>` nodes on their real lines with
  `Legacy.AccountMap::…` qualified names, plus a working bare-refid reference.
- `mybatis_ignores_the_ibatis_sqlmapconfig_root` — `<sqlMapConfig>` (the iBatis
  _config_ file, which declares no statements) must stay a file node only. The
  fixture plants a stray `<select id="strayStatement">` inside it, so a root regex
  without the word boundary would swallow the config as a statement map and
  attribute that statement to it.
- `mybatis_qualified_refid_keeps_its_namespace_and_only_splits_the_fragment_id` —
  a qualified refid produces exactly the `{namespace}::{id}` a node carries; a
  bare and a fully-written refid for the same fragment produce the SAME reference
  name; no reference name starts with `com::example`.

`crates/codegraph-resolve/tests/golden_resolution.rs` (end-to-end resolution
through the real resolver):

- `mybatis_qualified_refid_resolves_across_namespaces_and_rejects_the_decoy` —
  asserts the resolved edge target node ID for each of the four cases: bare stays
  home, foreign crosses over, the DECOY (a second `baseColumns` in another
  namespace, asserted `assert_ne!` to be a genuinely different node) is picked by
  namespace, and the unresolvable refid yields `None`.

The resolve-side test needed `Some("xml") => Language::Xml` added to
`resolve_fixture`'s extension map, which previously panicked on any non-listed
extension.

### Negative control, EXECUTED (three mutants, each reddening a DIFFERENT test)

Each mutant is a change to `mybatis.rs` ONLY; no test was touched.

1. **Dialect recognition reverted** (`<(mapper|sqlMap)\b…>` → `<(mapper)\b…>`,
   i.e. the exact pre-#1182 root) →
   `cargo test --locked -p codegraph-extract --test embedded_languages` →
   **exit 101**, 1 failed —
   `mybatis_accepts_the_ibatis_sqlmap_root_form`, and ONLY it:

       panicked at crates/codegraph-extract/tests/embedded_languages.rs:264:13:
       missing node kind=method name=legacyColumns; nodes=[
           Node {
               id: "file:a591d82f2d7e057a4033c2f6e60926d4",
               kind: File,
               name: "legacy_sqlmap.xml",
               …
           },
       ]

   (a single file node — the iBatis body is skipped entirely, reproducing RED #1).

2. **Qualified-refid matching reverted** to the unqualified whole-string rewrite
   (`refid.replace('.', "::")`, the exact pre-#1209 body) → reddens a DIFFERENT
   test in each of the two suites:

       panicked at crates/codegraph-extract/tests/embedded_languages.rs:278:13:
       missing ref kind=references name=com.example.OrderMapper::orderColumns; refs=[
           …
           reference_name: "com::example::UserMapper::baseColumns",
           line: 11,
           …

       panicked at crates/codegraph-resolve/tests/golden_resolution.rs:757:5:
       assertion `left == right` failed: a qualified refid must cross into the named namespace; edges=[…]
         left: None
        right: Some("method:3165c21458b287c3528cdae44cbc71d4")

   Both suites exit 101 with exactly 1 failure each, and neither is the test that
   mutant 1 reddens — so a partial regression in either half cannot hide behind
   the other.

3. **Word boundary dropped** (`<(mapper|sqlMap)\b…>` → `<(mapper|sqlMap)…>`, the
   tempting "looser is safer" design) → the two tests above still PASS, and it
   reddens a THIRD test instead:

       panicked at crates/codegraph-extract/tests/embedded_languages.rs:163:5:
       assertion `left == right` failed: sqlMapConfig keeps only the file node; nodes=[…]
         left: 2
        right: 1

   The extra node is the stray `<select>` inside `<sqlMapConfig>` — proof the
   boundary is load-bearing, not decoration.

Restored after each mutant by copying back the green file; restored SHA-256 of
`crates/codegraph-extract/src/embedded/mybatis.rs`:
`1550b1da1eaa6476c5b451e0f8b30aa2abe693eeccad70e08b4efa7d96f7f7ae`.
Re-run with the fix restored → `embedded_languages` exit 0, 8 passed;
`golden_resolution` exit 0, 13 passed.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, run before every Cargo
  batch; every Cargo command used `--locked`.
- `cargo build --locked -p codegraph-rs` → exit 0 (the binary used for the
  SQLite ground-truth measurements above, pre- and post-change; note the package
  is `codegraph-rs`, not `codegraph-cli`).
- `cargo test --locked -p codegraph-extract --test embedded_languages` → exit 0,
  8 passed.
- `cargo test --locked -p codegraph-resolve --test golden_resolution` → exit 0,
  13 passed.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 26/26.
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  commit (code, tests, fixtures, this prose, then `make fmt`); result recorded
  below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

Determinism: the change adds no map, no `HashMap` iteration and no new sort.
Root detection is a single leftmost regex match; statement extraction still walks
the body forward by byte offset; `qualify_refid` is a pure string split on the
LAST dot. Cross-file fragment lookup is not performed in the extractor at all —
it is the existing resolver's `match_by_qualified_name`, which already has a
deterministic ordering rule, and the reference name this port hands it is
identical for every run.

The untracked `reference/golden/*/colby.db-{wal,shm}` files the equivalence
oracle leaves behind were removed before staging; they are not gitignored and
must never be committed.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path
must be inside request cwd`); locked Cargo clippy/test is the honest fallback and
no LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte
changed. No native Windows/MSVC runtime validation is claimed — everything above
ran on Linux.

## Batch E3 — release artifacts carry a checksum authority and the installers refuse unverified binaries (2026-07-28)

### The defect, confirmed not assumed

Release artifacts shipped with **no checksums at all**, and both one-liner
installers downloaded an archive and immediately executed the binary out of it.

```
$ grep -rniE 'sha256|checksum|shasum|Get-FileHash' \
    .github/workflows/release-please.yml scripts/install.sh scripts/install.ps1
$ echo $?
1
```

Zero matches across all three files — the workflow published nothing to verify
against, and neither installer had anything to verify with. Read against the
code: `upload-assets` merged every `dist-*` artifact into `dist/` and attached
`dist/*.tar.gz` + `dist/*.zip`, nothing else. `scripts/install.sh` went
`download "$url" "$tmp/$asset"` → `tar -xzf` → `mv` → `chmod +x` →
`"$install_dir/$BIN" --version`, executing whatever bytes arrived. `install.ps1`
went `Invoke-WebRequest -OutFile $zipPath` → `Expand-Archive` → `& $exePath
--version`. A corrupted download, a truncated transfer, or a substituted asset
was indistinguishable from a good one, and the failure mode was _executing it_.

### The fix

Three coordinated changes, one commit:

1. **`.github/workflows/release-please.yml` — a new `Generate SHA256SUMS` step**
   inside `upload-assets`, placed after `Download artifacts` and before the
   attach step, with `working-directory: dist`. It hashes the archives of that
   very merged `dist/` and writes `dist/SHA256SUMS`; the same step's output is
   echoed into the job log. `dist/SHA256SUMS` is then added to the existing
   `softprops/action-gh-release@v2` step's `files:` list, so it lands on the SAME
   still-draft Release as the archives it describes. No new job, no second
   artifact download — the sums provably describe the uploaded bytes because they
   are computed from them.

2. **`scripts/install.sh` — an integrity gate before extraction.** It resolves a
   hashing tool once (`sha256sum`, else `shasum -a 256`), derives
   `sums_url="${release_base}/SHA256SUMS"` from the SAME `release_base` the
   archive URL is built from (so the sums always come from the same tag), matches
   its own asset's line, and compares. The gate sits between `download` and
   `tar -xzf`: an unverified archive is never unpacked and its binary is never
   executed.

3. **`scripts/install.ps1` — the same gate** via `Get-FileHash -Algorithm
SHA256`, placed between `Invoke-WebRequest` and `Expand-Archive`, with a
   `Assert-CanSkipVerification` helper mirroring the shell `cannot_verify`.

### Determinism of `SHA256SUMS` — how it is guaranteed

- **Ordering**: the filename list is piped through **`LC_ALL=C sort`**. `LC_ALL=C`
  forces byte-order collation, so the ordering does not depend on the runner's
  locale (a `en_US.UTF-8` runner and a `C` runner produce the same sequence).
  Shell glob order alone was not trusted for this.
- **Names**: `sha256sum` is invoked with `working-directory: dist`, so every line
  carries the bare asset basename. There is no `dist/` prefix and no absolute
  path, which is exactly what a client that downloaded a single asset can match
  against.
- **Format**: plain `sha256sum` output — `<64-hex><two spaces><name>` — so the
  file is directly consumable by `sha256sum -c SHA256SUMS`.
- **Line endings**: LF only. The file is produced by `sha256sum` on an
  `ubuntu-latest` runner and never round-tripped through a Windows tool.
- **Empty input is an error, not an empty file**: `nullglob` plus an explicit
  `${#archives[@]} -eq 0` check makes a missing-artifacts situation fail the job
  with `::error::no release archives found in dist/`, instead of quietly
  publishing an empty authority that every client would then treat as
  "unverifiable".

Proven locally by replaying the exact step body against a six-archive fake
`dist/` (`/tmp/e3-dist`):

```
e06c0482dc6da332be84c68951e1756bac63cffedc12f5c1b31c6a533d35a386  codegraph-9.9.9-aarch64-apple-darwin.tar.gz
1ff30088da0b1a6741a52e564af76c5bd670ade89785126af08210e4f94f1d81  codegraph-9.9.9-aarch64-pc-windows-msvc.zip
5d8ed155c442b58ab95e0fb2f4bfa136ed1b13b077363a7ff3ae8989780e4e27  codegraph-9.9.9-aarch64-unknown-linux-musl.tar.gz
929d1ee5ff7c3309238ce8c62b39d41bd3f5aad6132fad860a8454ced2949da8  codegraph-9.9.9-x86_64-apple-darwin.tar.gz
d4db99a2ab23ae7a094367d227c56edfde05211e1d52f2295ef0098bfa949bd6  codegraph-9.9.9-x86_64-pc-windows-msvc.zip
b88c456b823f476789e8fa7a8938314872cad3cc7131d4d1efd12021f88e7aa0  codegraph-9.9.9-x86_64-unknown-linux-musl.tar.gz
```

- run under `LC_ALL=C` and again under `LC_ALL=en_US.UTF-8` → `diff` **IDENTICAL**,
  both files hashing to `5488b0cd60d0a592183bbd43f20ad0663a935addbdcd02eafb6353cd8f8d3e8f`.
- `grep -c '/'` → **0** (no path prefix). `od -c | grep -c '\r'` → **0** (LF only).
- `sha256sum -c SHA256SUMS` → 6× `OK`, **exit 0**.
- the same body in an empty directory → **exit 1** with the `::error::` line.

### Fail closed, and what that costs

The gate is **fail-closed on every unverifiable condition**, not just on a
mismatch:

| condition                                                            | behaviour                                           |
| -------------------------------------------------------------------- | --------------------------------------------------- |
| digest matches                                                       | install proceeds                                    |
| digest **differs**                                                   | **hard abort**, always — the opt-out does NOT apply |
| no `sha256sum` and no `shasum` (POSIX) / no `Get-FileHash` (Windows) | refuse, unless opt-out                              |
| `SHA256SUMS` absent (404)                                            | refuse, unless opt-out                              |
| `SHA256SUMS` present but has no line for this asset                  | refuse, unless opt-out                              |

The opt-out is **`CODEGRAPH_SKIP_CHECKSUM`** (any non-empty value), documented in
the header comment of both scripts. It covers only "I cannot verify"; it never
covers "verification failed". A mismatch aborts with the expected and actual
digests printed, whatever the environment says.

**The deliberate compatibility cost**: a release cut BEFORE this change has no
`SHA256SUMS`, so the plain one-liner will now REFUSE to install it and print the
opt-out instruction. That is a real regression in convenience for old tags, and
it is the intended trade. Silently skipping verification when the authority is
missing would mean an attacker who can suppress one small file downgrades every
client back to the pre-E3 behaviour — the check would protect nobody. This
upholds the precedent already set on this branch by B1, C2, A4 and D3: when
correctness cannot be established, refuse rather than guess. Refusing to install
is recoverable in one command; executing a tampered binary is not.

### Tests added — `scripts/tests/install-checksum.test.sh`

An executable, **network-free** harness in the shape of
`scripts/tests/check-workspace-versions.test.sh` (same `PASS`/`FAIL`/`ok`/`bad`
counters, same `mktemp -d` + `trap cleanup EXIT`, same "assert the business
diagnostic, not just the exit code" style). It runs the REAL
`scripts/install.sh` under `env -i` with:

- a **`curl` shim** first on a sandboxed `PATH` that maps any URL to
  `$CG_TEST_RELEASE_DIR/<basename>` and exits **22** (curl's HTTP-error code)
  when the file is absent — so no scenario touches the network, and the script's
  own `download`/`fetch` plumbing is exercised unchanged;
- a **sandboxed `PATH`** built from symlinks to a fixed tool list, so the
  "no hashing tool" scenario is a _genuine absence_ rather than a stubbed
  failure;
- a fake release holding a real `tar.gz` (with an executable `codegraph` stub)
  plus a decoy Windows asset, so `SHA256SUMS` is never a one-line file and
  picking the right line actually matters.

Ten assertions across eight scenarios:

- **A_match** — correct digest ⇒ exit 0, binary present at the install dir,
  `sha256: OK` reported.
- **B_mismatch** — one digest zeroed ⇒ nonzero exit, `checksum MISMATCH`, and
  **the binary is NOT installed**.
- **C_mismatch_optout** — same mismatch WITH `CODEGRAPH_SKIP_CHECKSUM=1` ⇒ still
  aborts, still nothing installed. Proves the opt-out cannot launder a mismatch.
- **D_truncated** — `SHA256SUMS` correct, archive truncated to 64 bytes (the
  real-world corrupt-download shape, a form NOT designed for) ⇒ abort before
  extraction.
- **E_notool_refuse / E_notool_optout** — no `sha256sum` on PATH ⇒ explicit
  refusal naming `CODEGRAPH_SKIP_CHECKSUM` and no install; with the opt-out set,
  exit 0 with an `UNVERIFIED binary` warning.
- **F_nosums_refuse / F_nosums_optout** — no `SHA256SUMS` published at all (the
  pre-E3 release shape) ⇒ refusal; installable only under the opt-out.
- **G_no_entry** — `SHA256SUMS` present and well-formed but with our asset's line
  removed ⇒ refusal. A sums file that simply omits your asset must not read as a
  pass.
- **H_crlf** — correct digests with CRLF line endings ⇒ accepted. Line endings
  must not silently turn a good release into an unverifiable one.

D, G and H are deliberately shapes the implementation was not designed around,
per the lesson that got an earlier commit on this branch rejected: a fixture
holding only the author's intended shape proves nothing.

### Negative control, EXECUTED — four mutants, each reddening a DIFFERENT scenario

Each mutant edits `scripts/install.sh` only; no test file was touched.

1. **Whole verification block deleted** (the exact pre-E3 flow: download then
   `tar -xzf`) → **21 failures**, including the ones that matter most:

       FAIL: B_mismatch: expected a NONZERO exit, got 0
       FAIL: B_mismatch: binary WAS installed despite a failed verification
       FAIL: G_no_entry: binary WAS installed despite a failed verification
       FAIL: E_notool_refuse: binary WAS installed despite a failed verification
       === harness result: 0 passed, 21 failed ===

   `A_match` also loses its `sha256: OK` assertion. This is the defect itself,
   reproduced.

2. **`cannot_verify` downgraded to a silent `return 0`** — i.e. the FORBIDDEN
   "just skip it if you can't check" design → the mismatch tests still pass, and
   it reddens the three _unverifiable_ scenarios instead:

       FAIL: E_notool_refuse: expected a NONZERO exit, got 0
       FAIL: F_nosums_refuse: expected a NONZERO exit, got 0
       FAIL: G_no_entry: expected a NONZERO exit, got 0
       FAIL: G_no_entry: binary WAS installed despite a failed verification
       === harness result: 7 passed, 10 failed ===

3. **Mismatch routed through the opt-out** (a plausible "be lenient" change) →
   the unverifiable scenarios stay green and it reddens a different trio:

       FAIL: B_mismatch: expected diagnostic /checksum MISMATCH/ on stderr
       FAIL: C_mismatch_optout: expected diagnostic /checksum MISMATCH/ on stderr
       FAIL: D_truncated: expected diagnostic /checksum MISMATCH/ on stderr
       === harness result: 7 passed, 3 failed ===

4. **CRLF tolerance dropped** (`tr -d '\r'` removed — the "obviously
   unnecessary" guard) → everything else stays green and exactly one scenario
   goes red:

       FAIL: H_crlf: expected exit 0, got 1
         stderr: … error: cannot verify the download: SHA256SUMS has no entry for
                 codegraph-9.9.9-x86_64-unknown-linux-musl.tar.gz | error: refusing
                 to install an unverified binary. …
       FAIL: H_crlf: expected the binary at …/H_crlf.dest/codegraph, it is absent
       === harness result: 9 passed, 3 failed ===

   Note the failure mode: a CRLF sums file would make a PERFECTLY GOOD release
   look unverifiable and block every install. The `tr -d '\r'` is load-bearing.

Four mutants, four disjoint red sets — no partial regression in this gate can
hide behind another part of it. Restored by copying back the green file; restored
`scripts/install.sh` SHA-256
`5f35d096b95a2f2824cad06f8057b98a397a477988b421cd6a319722672e6418`, and the
harness re-run green: **10 passed, 0 failed, exit 0**.

### Windows — UNVERIFIED AT RUNTIME

`scripts/install.ps1` was changed by **code inspection only**. This host is
Linux; no PowerShell is available, so the `Get-FileHash` path, the
`Invoke-WebRequest` 404 `catch`, and `Assert-CanSkipVerification` were NOT
executed. No native Windows/MSVC runtime validation is claimed. What was checked
by reading: the gate sits between `Invoke-WebRequest` and `Expand-Archive`;
`$ErrorActionPreference = 'Stop'` plus `throw` gives the hard abort;
`-ine` compares the hex case-insensitively (`Get-FileHash` returns UPPERCASE,
`sha256sum` writes lowercase — a case-sensitive `-ne` here would have been a
false mismatch on every single install); `Get-Content` drops the line ending so
CRLF is handled, and `.TrimStart('*')` covers the BSD marker, mirroring the shell
side. Runtime confirmation on Windows is deferred to the next real release run.

### Golden delta

`GIT_MASTER=1 git diff --stat 063604d..HEAD -- reference/golden/` → **EMPTY**.
Nothing in this commit touches extraction, resolution, or any fixture: the diff
is two shell/PowerShell installers, one workflow job step, one new test harness,
and this prose. No golden was regenerated and none needed to be.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0, **26/26**.

### Verification (actual exit statuses)

- `grep -rniE 'sha256|checksum|shasum|Get-FileHash' <workflow> <install.sh> <install.ps1>`
  on the pre-change tree → exit **1**, zero matches (the defect proof above).
- `bash scripts/tests/install-checksum.test.sh` → exit **0**, 10 passed, 0 failed.
- `python3 -c "yaml.safe_load(...)"` on `release-please.yml` → parses; the
  `upload-assets` step list is `Checkout code, Download artifacts, Generate
SHA256SUMS, Generate release notes, Attach assets to GitHub Release` and
  `files:` is `dist/*.tar.gz\ndist/*.zip\ndist/SHA256SUMS\n`.
- `sha256sum -c SHA256SUMS` on the replayed six-archive `dist/` → exit **0**.
- `bash scripts/check-workspace-versions.sh` → run before every Cargo batch;
  result recorded below. Every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-bench --test equivalence` → recorded below.
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  commit (code, tests, this prose, then `make fmt`); recorded below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.
  No dependency and no version byte changed; release-please still owns versions.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path
must be inside request cwd`); no LSP-clean result is claimed. The changed files
are shell, PowerShell and YAML, which the Rust toolchain does not lint anyway —
the honest checks for them are the executable harness, the YAML parse, and the
replayed workflow step above.

## Batch E1 — `KNOWN_DIFFS.md` becomes an executable, fail-closed oracle (2026-07-28)

### The defect, in three layers

`docs/upstream-sync/KNOWN_DIFFS.md` opened by claiming it "is parsed by
`codegraph-bench::oracle::diff::KnownDiffs`". It was not. Three independent
layers, each confirmed by reading the code at `ecd3641`:

1. **The oracle was never wired.** `grep -rn "KnownDiffs::load" crates/` → exit
   **1**, ZERO hits. The only `KNOWN_DIFFS` mention in the crate was a string
   literal in an error message (`diff.rs:131`). `diff_canonical` applies the
   allowlist (`entries.retain(|entry| !known_diffs.allows(entry))`), but EVERY
   call site passed `None`: 18 in `crates/codegraph-bench/tests/equivalence.rs`,
   one in `oracle/mod.rs::assert_equivalent`, plus the sites in
   `crates/codegraph-cli/tests/{batch_m_outdated_migration,sync_incremental,
parallel_index,godot_idfields_determinism}.rs`. The document had never
   influenced a single decision.
2. **`parse_rule` was not fail-closed.** `tier=1` / `tier=2` PARSED fine and were
   discarded only later by `allows()` (`if entry.tier != Tier::Tier3 { return
false }`), so the document's promise that Tier-1/Tier-2 are "never
   allowlisted" held by downstream accident, not at parse time. A token lacking
   `=` was silently dropped, so `RULE garbage tier=3 surface=nodes key=*
justification=x` parsed clean. An unknown field name (`surfce=nodes`) was
   dropped the same way. `fields` is a `BTreeMap`, so a duplicate field silently
   kept the last occurrence.
3. **`diff.rs` had NO `mod tests`.** `grep -c "mod tests"
crates/codegraph-bench/src/oracle/diff.rs` → **0**. Nothing pinned "No Tier-3
   rules are active yet."

### A fourth layer the fail-closed parser exposed

Wiring the parser to the REAL document immediately reddened, before any mutation:

```text
/…/docs/upstream-sync/KNOWN_DIFFS.md must parse: parsing KNOWN_DIFFS.md line 12:
RULE tier=3 surface=<surface> key=<substring-or-*> justification=<short-token>:
unknown surface <surface>; the differ reports ["nodes", "files", "schema", "edges",
"unresolved_refs"]
```

Line 12 is the `RULE` TEMPLATE inside the "Rule format" ``text fence. The old
parser accepted it as an ACTIVE Tier-3 rule with surface `<surface>` — an
allowlist entry nobody wrote on purpose, inert only because that placeholder
surface never matches a real diff. Had anyone ever passed `Some(&known_diffs)`,
the committed document would have carried one bogus rule. Fix: `parse` now
tracks ``-fences and skips their contents, and BAILS on an unterminated fence
(otherwise every RULE after it would be silently skipped — the same
silent-inertness failure in the other direction). A test pins that a rule AFTER
a closed fence is still active, so fence tracking cannot degrade into "ignore
everything".

### What was wired, and where

- `KnownDiffs::repo_doc_path()` resolves `docs/upstream-sync/KNOWN_DIFFS.md` from
  `CARGO_MANIFEST_DIR`; `KnownDiffs::load_repo_doc()` parses it;
  `KnownDiffs::rule_count()` exposes the active-rule count for pinning.
- `oracle::mod::assert_equivalent` now loads the committed document and passes
  `Some(&known_diffs)`. The load happens BEFORE any comparison, so an
  unparseable file FAILS the assertion instead of being ignored. All 14
  `assert_equivalent` golden tests therefore now adjudicate through the real
  document.
- `assert_equivalent_with_known_diffs(rust_db, golden_dir, known_diffs_path)` is
  the injectable seam, used by the negative test that feeds a deliberately
  invalid allowlist.
- The `diff_canonical(..., None)` call sites were left alone on purpose: those
  compare two RUST runs against each other (sync vs `index --force`, run-to-run
  determinism, migration vs rebuild). An upstream-difference allowlist has no
  business softening a self-consistency check, so they stay strict.

### Fail-closed decisions, each justified

| Rejection                                      | Silently-broken rule it prevents                                                                                                                                                                                               |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `tier=1` / `tier=2` rejected **at parse time** | The promise that Tier-1/Tier-2 are never allowlisted was enforced only downstream in `allows()`; a reader of a `tier=1` line had no way to know it was inert.                                                                  |
| token without `=`                              | `RULE garbage tier=3 …` parsed clean; the garbage token was dropped.                                                                                                                                                           |
| unknown field name                             | `surfce=nodes` was dropped, so the rule fell through to "missing surface" or a stale value.                                                                                                                                    |
| duplicate field                                | Two contradictory `key=` values resolved by `BTreeMap` insertion position alone.                                                                                                                                               |
| empty key or value                             | `=nodes` / `surface=` produced a rule with an empty component.                                                                                                                                                                 |
| `surface=` outside the differ's surfaces       | An inert typo'd rule is worse than a loud error: it sits in the document looking like an active, reviewed decision while allowing nothing. A loud error is fixed in one commit; an inert rule can mislead for a release cycle. |
| unterminated ```-fence                         | Every RULE after it would be silently skipped.                                                                                                                                                                                 |

The `DIFF_SURFACES` whitelist `["nodes", "files", "schema", "edges",
"unresolved_refs"]` was ENUMERATED from the actual `compare_*` call sites in
`diff_canonical` (`compare_tier1_rows` for nodes/files, `compare_schema` for
schema, `compare_tier2_rows` for edges/unresolved_refs) — not guessed. A test
asserts every one of the five is accepted, so the whitelist cannot drift out of
sync with the differ without reddening.

`Tier` keeps all three variants: a `DiffEntry` genuinely carries any tier
(`diff_canonical` mints Tier-1 and Tier-2 entries today). The asymmetry — only
Tier-3 may appear on a RULE line, only a Tier-3 entry can be allowed — is now
documented on the enum and enforced in both `parse_rule` and `allows`.

### Two-direction proof

The entire risk of E1 is that wiring a previously-inert allowlist into golden
adjudication waves a real difference through. Both directions are pinned:

- ALLOWS: `tier3_rule_allows_its_matching_tier3_diff` — the Tier-3 rule
  (`surface=nodes key=alpha`) allows a Tier-3 `nodes` entry with key
  `function:alpha`.
- DOES NOT ALLOW, four ways: `..._does_not_allow_a_different_surface` (`edges`),
  `..._does_not_allow_a_non_matching_key` (`function:beta`),
  `..._does_not_allow_the_same_key_at_tier1`, `..._does_not_allow_the_same_key_at_tier2`.
- `wildcard_rule_never_allows_a_tier1_golden_difference` — even `key=*` allows
  only the Tier-3 entry and never the Tier-1 one.
- `tier1_entries_survive_the_allowlist_in_diff_canonical` — end-to-end through
  `diff_canonical` with a `key=*` rule loaded: an injected Tier-1 node drift is
  still reported. This is the assertion that matters most, since it exercises the
  exact retain() the wiring turned on.

### The zero-rules pin

Two tests pin that the committed document has ZERO active Tier-3 rules:
`oracle::diff::tests::committed_known_diffs_doc_parses_and_has_zero_active_rules`
(unit) and `committed_known_diffs_doc_is_parsed_and_allowlists_nothing`
(integration, next to the golden tests it protects). No Tier-3 rule was added to
the document to exercise the mechanism — every rule in this commit lives in test
strings. A future silent allowlist addition now fails CI in both places.

### Unasserted prose — stated plainly

`KNOWN_DIFFS.md` is mostly PROSE, and this commit does NOT make it executable.
The deferred-colby-resolver list, the canonicalized-timestamp note
(`nodes.updated_at`, `files.modified_at`, `files.indexed_at` — stripped in
`canonicalize.rs`, never compared), the Dart/Pascal function_ref notes, and the
Task-22 MCP text-formatting section remain unasserted documentation. That is
acceptable and intentional: they are not allowlist rules and were never claimed
to be. They describe behavior OUTSIDE the five SQLite surfaces the oracle
compares (deferred resolution paths that no golden fixture exercises, timestamps
removed before comparison, MCP text output the oracle does not read). Turning
them into machine-checked assertions would require new fixtures and new
comparison surfaces — out of scope for E1. What E1 makes executable is exactly
the RULE grammar and the active-rule set; the prose is still prose.

### Negative control — three mutants, three disjoint red sets

Green baseline: lib **29 passed**, `--test equivalence` **28 passed**.

1. **Restore `tier=1|2` acceptance** (`parse_rule` maps them back to
   `Tier::Tier1` / `Tier::Tier2` instead of bailing):

   ```text
   ---- oracle::diff::tests::invalid_known_diffs_file_fails_to_load stdout ----
   panicked at crates/codegraph-bench/src/oracle/diff.rs:626:63:
   must fail: KnownDiffs { rules: [KnownDiffRule { tier: Tier1, surface: "nodes",
   key_pattern: "*", justification: "sneaky" }] }

   ---- oracle::diff::tests::tier1_and_tier2_rules_are_rejected_at_parse_time stdout ----
   panicked at crates/codegraph-bench/src/oracle/diff.rs:443:45:
   rule must be rejected: KnownDiffs { rules: [KnownDiffRule { tier: Tier1,
   surface: "nodes", key_pattern: "*", justification: "must-not-be-allowed" }] }

   test result: FAILED. 27 passed; 2 failed        (lib)
   ```

   and in the integration test:

   ```text
   ---- an_unparseable_known_diffs_file_fails_the_equivalence_assertion stdout ----
   panicked at crates/codegraph-bench/tests/equivalence.rs:29:10:
   an invalid allowlist must fail, not be ignored: ()

   test result: FAILED. 27 passed; 1 failed        (equivalence)
   ```

2. **Make `allows()` ignore `surface`** (drop `rule.surface == entry.surface`):

   ```text
   ---- oracle::diff::tests::tier3_rule_does_not_allow_a_different_surface stdout ----
   panicked at crates/codegraph-bench/src/oracle/diff.rs:457:9:
   assertion failed: !known.allows(&tier3_entry("edges", "function:alpha"))

   test result: FAILED. 28 passed; 1 failed        (lib)
   test result: ok. 28 passed                       (equivalence — untouched)
   ```

3. **Remove the load wiring** (`assert_equivalent_with_known_diffs` uses
   `KnownDiffs::default()` and ignores the path):

   ```text
   ---- an_unparseable_known_diffs_file_fails_the_equivalence_assertion stdout ----
   panicked at crates/codegraph-bench/tests/equivalence.rs:29:10:
   an invalid allowlist must fail, not be ignored: ()

   test result: FAILED. 27 passed; 1 failed        (equivalence)
   test result: ok. 29 passed                       (lib — untouched)
   ```

4. **Add a Tier-3 rule to the committed document** (`RULE tier=3 surface=nodes
key=* justification=silently-added` appended to `KNOWN_DIFFS.md`) — the
   scenario the pin exists for:

   ```text
   ---- oracle::diff::tests::committed_known_diffs_doc_parses_and_has_zero_active_rules stdout ----
   panicked at crates/codegraph-bench/src/oracle/diff.rs:600:9:
   assertion `left == right` failed: /…/KNOWN_DIFFS.md must have zero active
   Tier-3 rules; adding one silently widens golden adjudication
     left: 1
    right: 0

   ---- committed_known_diffs_doc_is_parsed_and_allowlists_nothing stdout ----
   panicked at crates/codegraph-bench/tests/equivalence.rs:19:5:
   assertion `left == right` failed: /…/KNOWN_DIFFS.md must stay empty
     left: 1
    right: 0
   ```

Mutant 2 reddens ONLY the lib surface-discrimination test; mutant 3 reddens ONLY
the integration wiring test; mutant 1 reddens the parse-time gate in both; mutant
4 reddens both zero-rules pins. No partial regression in this gate can hide
behind another part of it.

Each mutant was restored by copying back the green file. Restored SHA-256:
`crates/codegraph-bench/src/oracle/diff.rs`
`2b525ddca8af6c26cddc0b01d285b36fa2ca4472bc157fcd3ac5b2765e4a6089`,
`crates/codegraph-bench/src/oracle/mod.rs`
`fd560ea19fdb574b12268ce31d45790e7e07f8f638ff72f441c5da4d76d482f3`,
`crates/codegraph-bench/tests/equivalence.rs`
`a69bd5c0e7d0d207ec3ff92d97ac0a43305d9985cfc2b5780532c3d4d837b2f2`,
`docs/upstream-sync/KNOWN_DIFFS.md`
`2c973b6f1407b409131c20849de44f15dc9989c1b47bb278e836daf3c682c07d` (byte-identical
to `ecd3641` — the document is NOT modified by this commit). Green re-confirmed
after every restore.

### Golden delta

`GIT_MASTER=1 git diff --stat ecd3641..HEAD -- reference/golden/` → **EMPTY**. No
golden was regenerated and none needed to be: this commit changes only the
oracle's rule parser, its wiring, and tests. No extraction, resolution, or
canonicalization byte changed. `KNOWN_DIFFS.md` itself is untouched.

### Verification (actual exit statuses)

- `grep -rn "KnownDiffs::load" crates/` on the pre-change tree → exit **1**, zero
  hits (defect layer 1). `grep -c "mod tests" …/oracle/diff.rs` → **0** (layer 3).
- `bash scripts/check-workspace-versions.sh` → exit **0** (`OK`, workspace
  0.40.4), run before every Cargo batch. Every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-bench --lib` → exit 0, **29 passed, 0 failed**
  (was 14 before; +15 new oracle tests).
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
  **28 passed, 0 failed** — the required floor is 26; the two added tests are
  `committed_known_diffs_doc_is_parsed_and_allowlists_nothing` and
  `an_unparseable_known_diffs_file_fails_the_equivalence_assertion`. All 26
  pre-existing tests still pass, now adjudicating through the real document.
- `cargo clippy --locked -p codegraph-bench --all-targets -- -D warnings` →
  exit **0**.
- `cargo test --locked -p codegraph-rs --test parallel_index` → exit 0,
  **5 passed** (the other `assert_equivalent` consumer).
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  prose; result recorded below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.
  No dependency, version, or lockfile byte changed.

`lsp_diagnostics` was attempted on `crates/codegraph-bench/src/oracle/diff.rs`
and refused this worktree again (`LSP file path must be inside request cwd`); no
LSP-clean result is claimed. The equivalent evidence is the locked `clippy -D
warnings` run above plus `make ci`.

## Batch E2 — the asset-name contract becomes machine-checked (2026-07-28)

### The three-way verification came back CLEAN

Before writing a line of code the three producers of the release archive name were
read side by side and compared literally. They agree today:

| surface                   | name expression                                                                                |
| ------------------------- | ---------------------------------------------------------------------------------------------- |
| workflow tar.gz packaging | `dist/${BINARY_NAME}-${{ needs.release-please.outputs.version }}-${{ matrix.target }}.tar.gz`  |
| workflow zip packaging    | `dist/${env:BINARY_NAME}-${{ needs.release-please.outputs.version }}-${{ matrix.target }}.zip` |
| artifact upload           | `name: dist-${{ matrix.target }}`, `path: dist/*`                                              |
| `upload-assets` download  | `pattern: dist-*`, `merge-multiple: true`                                                      |
| `scripts/install.sh`      | `BIN="codegraph"`, `asset="${BIN}-${version}-${target}.${ext}"`                                |
| `scripts/install.ps1`     | `$Bin = 'codegraph'`, `$asset = "$Bin-$version-$target.$ext"`                                  |

`env.BINARY_NAME` is `codegraph`, identical to `install.sh`'s `BIN` and
`install.ps1`'s `$Bin`. `install.sh` builds `target="${arch_part}-${os_part}"`
from `{x86_64, aarch64} x {unknown-linux-musl, apple-darwin}`, and `install.ps1`
builds `"$archPart-pc-windows-msvc"` from `{x86_64, aarch64}` — together exactly
the six matrix triples, no orphan on either side. The `aarch64-pc-windows-msvc`
question was checked specifically, because it is the one target whose installer
path is easy to get wrong: `install.ps1` matches `^(ARM64|aarch64)$` against
`PROCESSOR_ARCHITEW6432` (falling back to `PROCESSOR_ARCHITECTURE`, then
`RuntimeInformation.OSArchitecture`), so a Windows-on-ARM host does yield
`aarch64-pc-windows-msvc`. **No defect was found there**; nothing in either
installer needed changing, and neither installer's checksum logic was touched.

So this commit fixes no bug. It closes a hole in the _process_.

### The gap: agreement with no mechanism to keep it

The agreement above is a property of the current bytes, not an enforced invariant.
`tar -czf "dist/…"` in the workflow and `asset="…"` in `install.sh` are two
independent string concatenations with nothing linking them. Change either one and
`cargo test`, `clippy`, `fmt`, the guardrail, and all six cross-compiles still
pass — CI has no opinion about it. The failure surfaces only _after_ a release: the
archives are already public, `SHA256SUMS` is already generated over the
differently-named files, `publish-release` has already flipped the Release to
`draft=false`, and the one-liner installers 404. That is a real, foreseeable
failure path with, until now, zero protection. E3 made the _contents_ of a release
verifiable; E2 makes the _names_ verifiable.

### What was added

`scripts/check-asset-names.sh` — a standalone gate that re-derives all three names
**from the real files** on every run (`.github/workflows/release-please.yml` via
`yaml.safe_load`, `scripts/install.sh` and `scripts/install.ps1` via anchored
regexes over their actual source). It hard-codes no expected asset name: the only
literal it owns is the canonical skeleton `<bin>-<version>-<target>.<ext>`, which
is a _shape_, not a copy of any side's value. A hard-coded duplicate of the
expected names would itself drift and turn the gate into decoration.

Seven assertions, each naming the side at fault when it trips:

1. `binary-name` — `env.BINARY_NAME` == `BIN` == `$Bin`.
2. asset skeleton — all four name expressions (workflow tar.gz, workflow zip,
   `install.sh`, `install.ps1`) normalize to the same skeleton.
3. `extension(unix|windows)` — the workflow's packaged extension per family
   equals the extension the matching installer downloads.
4. `target-coverage` — every matrix target is producible by **exactly one**
   installer's platform detection, with that installer's extension; and no
   installer can ask for a target the matrix never builds (both directions).
5. `artifact-plumbing` — the rendered `upload-artifact` `name:` matches the
   `download-artifact` `pattern:`, `merge-multiple` is `true`, and `path:` covers
   the packaged archives.
6. `release-files` — every rendered archive, plus the `SHA256SUMS` name both
   installers fetch, is covered by a release `files:` glob.
7. `archive-member` — the workflow tars/zips the same in-archive binary name the
   installers extract (`codegraph` / `codegraph.exe`).

### Granularity — the deliberate trade-off

Comparison is on the **normalized skeleton**, not on raw source bytes. Each side's
expression is parsed and its interpolations are replaced by canonical placeholders
(`${BINARY_NAME}`/`${env:BINARY_NAME}`/`$Bin`/`${BIN}` → `<bin>`,
`${{ needs.*.outputs.version }}`/`$version`/`${version}` → `<version>`, and so on),
then all four must reduce to `<bin>-<version>-<target>.<ext>`.

- Too strict (exact source-string comparison) would redden on YAML re-indentation,
  a renamed shell variable, or routing the version through a different workflow
  expression — none of which change a single published byte. False alarms train
  people to weaken the gate.
- Too loose (e.g. "does the name contain the version somewhere") would miss field
  reordering, a changed separator, or a stray prefix — the exact mutations that
  break the installers.

The skeleton sits between the two: blind to how a value is spelled, sensitive to
field order, separators, affixes, and extension. Scenario C in the harness pins
the loose end (a comment plus requoting `path: dist/*` → `path: "dist/*"` stays
green); scenarios D and G pin the strict end (a `-` → `_` separator change and a
`<version>`/`<target>` swap both go red). The globs (`pattern:`, `path:`,
`files:`) are additionally matched against _concrete_ rendered names using
`fnmatch`, so the plumbing that moves the archives is checked, not just the
archives' names.

Where the gate cannot parse a side, it **fails** (exit 2) rather than passing —
an unverifiable contract is exactly when drift hides. Scenario J proves it.

### Negative control, EXECUTED — four mutants, each blamed correctly

Each mutant was applied to a throwaway copy of the three real files (the real
repository tree was never modified) and the gate run against that fixture root.
Real output:

```text
##### MUTANT m1 — workflow tar.gz separator `-` → `_` #####
check-asset-names: MISMATCH [workflow(tar.gz)]: asset skeleton is '<bin>_<version>-<target>.<ext>' but the other sides use '<bin>-<version>-<target>.<ext>'
check-asset-names: FAIL: 1 asset-name disagreement(s) between the release workflow and the installers
EXIT=1

##### MUTANT m2 — install.sh BIN="codegraph" → "codegraf" #####
check-asset-names: MISMATCH [binary-name]: workflow env.BINARY_NAME='codegraph' vs install.sh BIN='codegraf' vs install.ps1 $Bin='codegraph'
check-asset-names: FAIL: 1 asset-name disagreement(s) between the release workflow and the installers
EXIT=1

##### MUTANT m3 — upload-assets `pattern: dist-*` → `bins-*` #####
check-asset-names: MISMATCH [artifact-plumbing]: upload-artifact name 'dist-x86_64-unknown-linux-musl' does not match upload-assets pattern 'bins-*'
check-asset-names: MISMATCH [artifact-plumbing]: upload-artifact name 'dist-aarch64-unknown-linux-musl' does not match upload-assets pattern 'bins-*'
check-asset-names: MISMATCH [artifact-plumbing]: upload-artifact name 'dist-x86_64-apple-darwin' does not match upload-assets pattern 'bins-*'
check-asset-names: MISMATCH [artifact-plumbing]: upload-artifact name 'dist-aarch64-apple-darwin' does not match upload-assets pattern 'bins-*'
check-asset-names: MISMATCH [artifact-plumbing]: upload-artifact name 'dist-x86_64-pc-windows-msvc' does not match upload-assets pattern 'bins-*'
check-asset-names: MISMATCH [artifact-plumbing]: upload-artifact name 'dist-aarch64-pc-windows-msvc' does not match upload-assets pattern 'bins-*'
check-asset-names: FAIL: 6 asset-name disagreement(s) between the release workflow and the installers
EXIT=1

##### MUTANT m4 — install.ps1 loses the `^(ARM64|aarch64)$` arm #####
check-asset-names: MISMATCH [target-coverage]: matrix target 'aarch64-pc-windows-msvc' cannot be produced by either installer's platform detection (install.sh yields ['aarch64-apple-darwin', 'aarch64-unknown-linux-musl', 'x86_64-apple-darwin', 'x86_64-unknown-linux-musl']; install.ps1 yields ['x86_64-pc-windows-msvc'])
check-asset-names: FAIL: 1 asset-name disagreement(s) between the release workflow and the installers
EXIT=1
```

Each mutant names a **different** category — `workflow(tar.gz)`, `binary-name`,
`artifact-plumbing`, `target-coverage` — and the harness additionally asserts the
_absence_ of the other categories, so a mutant cannot pass by reddening something
unrelated. The `m4` shape is the ARM64-coverage question from the audit above,
inverted into a test: were the ARM64 arm ever dropped, the gate now says so by
name.

### Tests added — `scripts/tests/asset-name-drift.test.sh`

Ten scenarios, no Cargo, no network, no repository file touched (each copies the
three real files into a `mktemp -d` fixture and mutates one). Mutation anchors are
asserted present before substitution, so a future refactor that moves an anchor
makes the harness fail loudly instead of silently neutering a mutant into a no-op:

| scenario              | mutation                                  | expect                                             |
| --------------------- | ----------------------------------------- | -------------------------------------------------- |
| A `pristine`          | none (verbatim copies)                    | `0`, all six targets listed with their owner       |
| B `repository`        | none, run on the real repo root           | `0`, `check-asset-names: OK`                       |
| C `cosmetic`          | comment + requote `path:`/`pattern:`      | `0` — churn is not drift                           |
| D `workflow_tar_name` | tar.gz separator `-` → `_`                | `1`, blames `workflow(tar.gz)` only                |
| E `install_sh_bin`    | `BIN="codegraf"`                          | `1`, blames `binary-name`, prints all three values |
| F `download_pattern`  | `pattern: bins-*`                         | `1`, blames `artifact-plumbing`                    |
| G `ps1_field_order`   | `$Bin-$target-$version.$ext`              | `1`, blames `install.ps1` only                     |
| H `ps1_drops_arm64`   | delete the `^(ARM64\|aarch64)$` arm       | `1`, names `aarch64-pc-windows-msvc`               |
| I `sums_unpublished`  | drop `dist/SHA256SUMS` from `files:`      | `1`, blames `release-files`                        |
| J `unparsable`        | replace `tar -czf` with `bsdtar --create` | nonzero — fails CLOSED, never a silent pass        |

Scenario I is the E2/E3 seam: E3's `SHA256SUMS` is only useful if it is actually
published under the name both installers fetch, so an unpublished sums file is
asset-name drift and reddens here. E3's own harness and the workflow's
`Generate SHA256SUMS` step were not modified.

### CI wiring — inside `scripts/guardrail.sh`, not a new `make ci` step

The gate runs as a second block of `scripts/guardrail.sh` rather than as its own
Makefile target. Reason: `guardrail` is the single step already invoked by **all
four** enforcement paths — `make ci`, the `.githooks/pre-push` hook, the CI `test`
job, and (transitively) the release workflow's `verify-ci` wait on `CI Success`. A
new `make ci` target would be missed by the pre-push hook and by CI unless three
more files were edited, and a check that only some paths run is not a gate. The
guardrail is also the right conceptual home: both blocks assert repository-level
invariants that no Rust test can express. `ci.yml` gains one idempotent step that
ensures PyYAML is importable, so a missing parser can never degrade the gate into
a skip (and the gate itself exits nonzero if the import fails anyway). The
guardrail resolves the gate by `dirname "$0"`, so it works from any cwd —
verified by running it from `/tmp`.

### Windows — runtime-UNVERIFIED

`install.ps1` is read, not executed: this host is Linux with no PowerShell. The
`aarch64-pc-windows-msvc` conclusion above is a **code-reading argument** about
`PROCESSOR_ARCHITEW6432` / `PROCESSOR_ARCHITECTURE` / `RuntimeInformation`, and the
gate's knowledge of `install.ps1` is likewise static parsing of its source. No
native Windows/MSVC runtime verification was performed or is claimed. The file's
bytes are unchanged by this commit, so no new Windows risk is introduced.

### Golden delta

`GIT_MASTER=1 git diff --stat 5c585ec..HEAD -- reference/golden/` → **EMPTY**. This
commit adds two shell scripts and edits `guardrail.sh`, `Makefile`, `ci.yml`,
`.githooks/pre-push`, and this ledger. No extraction, resolution, or
canonicalization code was touched, so no golden could change and none was
regenerated.

### Verification (actual exit statuses)

- `bash scripts/check-asset-names.sh` → exit **0** (`check-asset-names: OK`, six
  matrix targets, each owned by exactly one installer).
- `bash scripts/tests/asset-name-drift.test.sh` → exit **0**, **10 passed, 0
  failed**.
- Four standalone mutants → exit **1**, **1**, **1**, **1** with the diagnostics
  quoted verbatim above.
- `bash scripts/guardrail.sh` → exit **0** from the repo root and exit **0** from
  `/tmp` (cwd independence).
- `python3 -c 'import yaml'` → exit **0** (PyYAML 6.0.3 on this host);
  `yaml.safe_load` on the mutated `ci.yml` parses and lists the new step between
  `Run tests …` and `Scope guardrail …`.
- `bash scripts/check-workspace-versions.sh` → recorded below; run before the
  Cargo-invoking gates. Every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-bench --test equivalence` → recorded below
  (floor: 28 after E1).
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  prose and `make fmt`; recorded below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.
  No dependency was added, no version byte moved; release-please still owns
  versions.

`lsp_diagnostics` was attempted and refused this worktree again (`LSP file path
must be inside request cwd`); no LSP-clean result is claimed. The changed files are
shell, YAML and Markdown, which the Rust toolchain does not lint — the honest
checks for them are the executable harness, the four mutants, the YAML parse, and
`make ci`.

## Batch F4 — `CI Success` becomes a strict gate: only `success` passes (2026-07-28)

### The defect (found by the F1–F4 review wave, confirmed by reading the file)

`.github/workflows/ci.yml`, job `ci-success` (pre-fix lines **173–187**; the
`run:` body at **182–187** and the offending condition on **line 183**) rejected
exactly ONE result value:

```yaml
- name: Check job status
  run: |
    if [[ "${{ needs.test.result }}" == "failure" || "${{ needs.audit.result }}" == "failure" || "${{ needs.windows.result }}" == "failure" ]]; then
      echo "❌ CI failed"
      exit 1
    fi
    echo "✅ CI passed"
```

`needs.<job>.result` is not boolean. GitHub Actions sets it to `success`,
`failure`, `cancelled`, or `skipped`. The expression above tests only for
`failure`, so **`cancelled` and `skipped` both fell through to
`echo "✅ CI passed"` and exit 0.**

Three facts turn that from a latent typo into a release blocker:

1. `ci.yml` line **13** sets `cancel-in-progress: true`. A second push to the
   same ref cancels the in-flight run — the cancelled state is not exotic, it is
   the routine outcome of pushing twice.
2. `ci-success` runs `if: always()`, so it executes even when its needed jobs are
   cancelled. It then printed `✅ CI passed`.
3. `release-please.yml` line **124** (`Wait for CI Success on the tag commit`,
   job `verify-ci`, lines **100–175**) gates the release on **that job's**
   conclusion, and `upload-assets` (line **287**) lists `verify-ci` in `needs`.

So the reachable path was: push, push again → CI run 1 cancelled → run 1's
`CI Success` concludes **success** → a release cut against run 1's commit finds a
green `CI Success` → `verify-ci` passes → assets upload → `publish-release`
publishes. A release on a run whose tests never finished. That is precisely the
half of the project's contract ("a release only fires on green CI") that this
commit restores.

### Old vs new expression, side by side

**OLD** (the whole gate, one line):

```bash
if [[ "${{ needs.test.result }}" == "failure" || "${{ needs.audit.result }}" == "failure" || "${{ needs.windows.result }}" == "failure" ]]; then
```

**NEW** (the accept test, from the shipped body):

```bash
mapfile -t rows < <(
  printf '%s' "$NEEDS_JSON" \
    | jq -r 'to_entries | sort_by(.key)[] | "\(.key)\t\(.value.result // "<missing>")"'
)
...
  if [ "$result" = "success" ]; then   # ← the ONLY accepting branch
```

Three changes of substance:

- **Allow-list, not deny-list.** `= "success"` accepts one value; every other
  value — `cancelled`, `skipped`, `neutral`, `timed_out`, an empty string, or any
  value GitHub adds in the future — falls to the failing branch. Unknown is
  treated as not-success, never tolerated.
- **Results come from `${{ toJSON(needs) }}`** (env `NEEDS_JSON`), so the body
  hardcodes no job name. Adding a job to `needs:` gates it with no script edit.
  A `needs.<job>.result` interpolation reappearing in the body is now a guard
  failure (scenario E below).
- **Fails closed on a degenerate context.** An empty `NEEDS_JSON`, a non-object,
  a `{}`, or a job whose `result` key is absent all exit 1 with an explicit
  "failing closed" message, instead of iterating zero jobs and declaring success.

### Actionable failure message (the point of the rewrite, not decoration)

A maintainer reading a red `CI Success` now sees, per job, which one was not
`success` and what it actually was — plus a GitHub `::error` annotation that
surfaces in the run summary:

```
Required job results (only 'success' passes):
  ✅ audit        success
  ❌ test         cancelled
  ✅ windows      success
::error title=CI gate::required job 'test' concluded 'cancelled', not 'success'.
❌ CI failed: 1 of 3 required job(s) did not conclude 'success' (see the ❌ rows above).
```

### The required set: `test`, `audit`, `windows` — verified against the file

Enumerated from the real `ci.yml`, not from the audit note. `jobs` contains
exactly five keys: `test` (line 20), `windows` (71), `audit` (112), `coverage`
(145 pre-fix 138), `ci-success` (205 pre-fix 173). The gate requires
`test, audit, windows`; the required set is **unchanged by this commit** — it was
not widened or narrowed, only made strict.

`coverage` remains deliberately EXCLUDED. `codecov.yml` marks both statuses
`informational: true` (baseline ~72% line coverage against an aspirational 95%
target), so coverage is report-only by design and must never redden a merge or
block a release. That reasoning is now written where the exclusion lives
(`ci.yml` `needs:` comment) **and mechanically tied to its premise**:
`scripts/check-ci-gate.sh` fails if `codecov.yml` ever stops marking coverage
informational while it is still excluded, so the exclusion cannot outlive its
justification (harness scenario I).

### "Robust to a job added later" — GitHub Actions cannot express it; the gap is made loud

Stated plainly: **there is no in-workflow way to require every job.** The `needs`
context contains ONLY the jobs listed in `needs:`. `toJSON(needs)` iterates
whatever is there, so a job added to `ci.yml` and never added to
`ci-success`'s `needs:` is invisible to the gate no matter how the expression is
written. There is no `needs: [*]`, and `jobs.<id>.needs` accepts no wildcard.

So the invariant is asserted from OUTSIDE the workflow, and made loud rather than
silent: new script `scripts/check-ci-gate.sh` parses `ci.yml` and fails when
`ci-success.needs` is not exactly `set(jobs) - {ci-success} - INFORMATIONAL`. Add
a job without gating it and the guardrail turns red locally, at pre-push, and in
CI, naming the ungated job (harness scenario H adds a `fuzz` job and gets
`job(s) ['fuzz'] exist in ci.yml but are NOT in ci-success's needs:`). The
allow-list is a single explicit set in that script with a per-entry
justification function — widening it is a visible, reviewable edit, not an
omission.

The guard also asserts the gate's other structural preconditions: the job exists
and is still named `CI Success` (the string branch protection and `verify-ci`
both look for), it still runs `if: always()` (without it the gate is skipped when
a needed job fails, so it never turns red), and it has exactly one `run:` step
reading `${{ toJSON(needs) }}`.

### `verify-ci` — audited, already strict, NOT changed

Checked as required. `release-please.yml` lines **152–175** poll the `CI Success`
job and accept exactly one conclusion:

```bash
if [ "$conclusion" = "success" ]; then
  echo "✅ CI Success passed for ${HEAD_SHA}."
  exit 0
fi
echo "❌ CI Success concluded '${conclusion}' for ${HEAD_SHA}; blocking release (draft preserved)."
exit 1
```

That is already an allow-list: a `cancelled` conclusion on `CI Success` hits the
`exit 1` and blocks the release. There is exactly one `exit 0` in the body and it
is inside the `= "success"` branch. **No fix was needed and none was made** — the
release-side gate was never the hole. The hole was upstream of it: `CI Success`
itself reported `success` for a cancelled run, so `verify-ci` was correctly
believing an incorrect input.

`check-ci-gate.sh` now pins that shape too (single `exit 0`, inside the success
branch, at least one `exit 1`, selects the `CI Success` job, and `upload-assets`
still lists `verify-ci` in `needs` so the gate stays load-bearing) — harness
scenarios J and K.

### Negative control — old ADMITS `cancelled`/`skipped`, new REJECTS

Both columns are produced by executing real bash locally: the OLD column runs the
pre-fix body verbatim with `${{ needs.X.result }}` textually substituted the way
the runner substitutes it; the NEW column extracts the shipped `run:` body
straight out of `.github/workflows/ci.yml` with `yaml.safe_load` and runs it with
a synthetic `NEEDS_JSON`. Actual output of
`bash scripts/tests/ci-gate.test.sh`, scenario D:

```
  truth table — result | OLD exit | NEW exit | OLD verdict | NEW verdict
    success    |    0     |    0     | ADMITS  | ADMITS
    failure    |    1     |    1     | rejects | rejects
    cancelled  |    0     |    1     | ADMITS  | rejects
    skipped    |    0     |    1     | ADMITS  | rejects
```

Rows 3 and 4 ARE the defect and its fix. Rows 1 and 2 show the fix is not a
behavior change for the cases that already worked.

The wider table, run by the guard itself over the shipped body (actual output of
`bash scripts/check-ci-gate.sh`):

```
check-ci-gate: truth table over the shipped gate body (3 required job(s))
  all jobs success                               exit 0   OK
  audit = failure                                exit 1   OK
  audit = cancelled                              exit 1   OK
  audit = skipped                                exit 1   OK
  audit = neutral                                exit 1   OK
  audit = timed_out                              exit 1   OK
  audit = unknown_future_value                   exit 1   OK
  audit = <empty-string>                         exit 1   OK
  audit = cancelled (per-job coverage)           exit 1   OK
  test = cancelled (per-job coverage)            exit 1   OK
  windows = cancelled (per-job coverage)         exit 1   OK
  all jobs cancelled                             exit 1   OK
  empty needs context                            exit 1   OK
  empty JSON object                              exit 1   OK
  malformed JSON                                 exit 1   OK
  result key missing                             exit 1   OK
check-ci-gate: OK (16 truth-table cases; only all-success exits 0)
```

`unknown_future_value` is the forward-compatibility case: a value GitHub does not
document today fails, because the gate allow-lists `success` rather than
enumerating the failures.

### Why a `scripts/tests/` harness — decided deliberately, added

The precedent is exact. `scripts/check-asset-names.sh` (Batch E2) guards a
CI/release contract that no Cargo test can express, ships a
`scripts/tests/asset-name-drift.test.sh` mutation harness, and is wired into
`scripts/guardrail.sh` so it runs in `make ci`, the pre-push hook, and CI. This
defect is the same shape — a gate whose weakening leaves every test, lint, and
build green — so it gets the same treatment rather than a new mechanism.

The argument against (a workflow file is "just config", the fix is one
expression) does not survive the evidence above: the previous one-expression gate
looked correct to reviewers for the whole life of the file, and the mode of
failure is silence. A guard that only asserted shape would also be weak, so the
harness asserts BEHAVIOR: it extracts the shipped step body and executes it. If
someone rewrites the gate in a different but equally strict way, the truth table
still passes; if they rewrite it in a weaker way, it reddens.

`scripts/tests/ci-gate.test.sh` — 12 scenarios, mutating ONE real file per
scenario inside a throwaway fixture (the real tree is never touched):

| scenario                 | mutation                                              | expect                                          |
| ------------------------ | ----------------------------------------------------- | ----------------------------------------------- |
| A `pristine`             | none (verbatim copies)                                | `0`, strict gate + truth table green            |
| B `repository`           | none, run on the real repo root                       | `0`, `check-ci-gate: OK`                        |
| C `cosmetic`             | reflow `needs:` inline list → block list, add comment | `0` — churn is not a gate change                |
| D `negative_control`     | none; evaluates OLD and NEW bodies side by side       | old ADMITS `cancelled`/`skipped`, new rejects   |
| E `legacy_expression`    | restore a `${{ needs.test.result }}` interpolation    | `1`, blames `gate-shape`, quotes the expression |
| F `job_ungated`          | drop `windows` from `needs:`                          | `1`, names `['windows']`                        |
| G `no_always`            | delete `if: always()`                                 | `1`, blames `gate-always`                       |
| H `new_job_ungated`      | add a `fuzz` job, do not gate it                      | `1`, names `['fuzz']`                           |
| I `coverage_blocking`    | `codecov.yml` → `informational: false`                | `1`, blames `gate-exclusion`                    |
| J `release_gate_relaxed` | `verify-ci` → `[ "$conclusion" != "failure" ]`        | `1`, blames `release-gate`                      |
| K `gate_not_wired`       | `upload-assets` drops `verify-ci` from `needs`        | `1`, blames `release-gate`                      |
| L `unparsable`           | `jobs:` becomes a sequence                            | nonzero — fails CLOSED, never a silent pass     |

### CI wiring — a third block of `scripts/guardrail.sh`

Same home and same reasoning as Batch E2's asset-name gate: `guardrail` is the
one step already invoked by `make ci`, `.githooks/pre-push`, and the CI `test`
job, so a check placed there runs on every enforcement path without editing three
more files. `ci.yml` gains one `jq --version` step before the guardrail so a
missing `jq` (needed to evaluate the truth table) fails loudly instead of letting
the gate degrade — the same fail-loud pattern as the existing PyYAML step.

There is a deliberate ordering property worth naming: the guard runs inside the
`test` job, which is itself one of the gated jobs. If someone weakens the gate,
`test` goes red, so `ci-success` sees `test = failure` and — under the new strict
logic — fails. The weakening cannot merge.

### Scope

Touched: `.github/workflows/ci.yml`, `scripts/guardrail.sh`, new
`scripts/check-ci-gate.sh`, new `scripts/tests/ci-gate.test.sh`, and this ledger.
Explicitly NOT touched: any file under `crates/`, the six-platform build matrix,
the `Generate SHA256SUMS` step, the draft-until-all-green `publish-release`
mechanism, `cancel-in-progress: true` (concurrency cancellation is intended —
the bug was that a cancelled run reported success, not that cancellation
happens), `verify-ci`'s already-strict polling logic, and every version /
lockfile byte.

### Honesty — what was and was NOT verified

Validated by **local expression evaluation and YAML parsing only**:

- the shipped `ci-success` step body was extracted from the real `ci.yml` with
  `yaml.safe_load` and EXECUTED under `bash` against synthetic `needs` contexts;
- the pre-fix body was executed the same way with the runner's textual
  interpolation applied by hand;
- both workflow files were parsed with `yaml.safe_load`.

**No GitHub Actions run was triggered, cancelled, or observed.** No claim is made
that a real cancelled run was pushed through this gate on GitHub's
infrastructure. The `${{ toJSON(needs) }}` interpolation itself, and the exact
set of values GitHub may place in `result`, are taken from GitHub's documented
contract, not from an observed run — that is the one link in the chain this
evidence cannot close locally, and it is the same link every workflow edit in
this repository rests on.

`lsp_diagnostics` is **not applicable**: the changed files are YAML, POSIX shell,
Bash, and Markdown. No Rust source was touched, so there is no LSP result to
report — clean or otherwise, and none is claimed.

### Golden delta

`GIT_MASTER=1 git diff --stat 956cf32..HEAD -- reference/golden/` → **EMPTY**.
This commit adds two shell scripts and edits `ci.yml`, `guardrail.sh`, and this
ledger. No extraction, resolution, or canonicalization code exists in the diff,
so no golden could change and none was regenerated.

### Verification (actual exit statuses)

- `bash scripts/check-ci-gate.sh` → exit **0** (structure OK; 16 truth-table
  cases, only all-success exits 0).
- `bash scripts/tests/ci-gate.test.sh` → exit **0**, **12 passed, 0 failed**
  (scenario L exits 2 — the fail-closed `die()` path — which the harness accepts
  as nonzero).
- `bash scripts/guardrail.sh` → exit **0** (all three blocks: forbidden crates,
  asset-name contract, CI gate integrity).
- `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))"`
  → exit **0**.
- `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release-please.yml'))"`
  → exit **0**.
- `bash scripts/check-workspace-versions.sh` → exit **0** (workspace 0.40.4 ==
  `version.txt` == release manifest `.`; 10 source-less packages all at 0.40.4).
- `make ci CARGO='cargo --locked'` → **exit 0** (`✅ All CI checks passed!`), with
  the full chain green: `fmt-check` (rustfmt + oxfmt, "All matched files use the
  correct format"), clippy `-D warnings`, `cargo test --workspace`, and all three
  guardrail blocks.

  **The known flake, reported honestly with BOTH results.** Across the runs on
  that worktree, `make ci` came back green, then once **exit 101** with a single
  failure — `formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home`
  at `crates/codegraph-cli/src/main.rs:6213`,
  `assertion failed: dir.join(".config/fish/completions/codegraph.fish").is_file()`,
  **352 passed, 1 failed** — and then green again on the immediate re-run with no
  code change in between. That is the documented pre-existing `HOME` /
  `XDG_DATA_HOME` in-process race on this branch (~4 failures in 8 runs on a clean
  commit), it lives in `crates/codegraph-cli`, which this commit does not touch,
  and it was NOT weakened, ignored, skipped, or slowed down. Both outcomes are
  recorded above rather than only the convenient one.

  The final `make ci` — the one run after this prose and after `make fmt`, with no
  byte written afterwards — has its actual exit status reported in this task's
  closeout message. It is deliberately not backfilled here, because writing it
  would mean editing the ledger AFTER the final gate, which the closeout order
  forbids.

- `sha256sum Cargo.lock` → `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`,
  **unchanged**. No dependency added, no version byte moved; release-please still
  owns versions.

**Where `make ci` was run, and why that is the honest reading.** This task's
worktree is shared with three sibling F-wave tasks, and at the time of the final
gate a sibling's in-flight `crates/codegraph-resolve` edit left the tree failing
clippy with `dead_code` on its own not-yet-wired helpers — a failure entirely
inside `crates/`, which this task is forbidden to touch. So the final gate was run
on a **clean detached worktree at `956cf32`** carrying EXACTLY this commit's five
files (`ci.yml`, `guardrail.sh`, `check-ci-gate.sh`, `ci-gate.test.sh`, this
ledger) and nothing else. That is the stricter reading, not a weaker one: it
proves these five files alone are green against the recorded parent commit,
without borrowing green from — or inheriting red from — a sibling's work in
progress. The exit-0 above is that run's real status.

## Batch F3 — the data-router route-object walk stops at that object's own `}`, not at 300 fixed bytes (2026-07-28)

Completes upstream issue **#1348**, whose data-router half Batch D1 left untouched
while the manifest already classified the issue as a full `PORT`. This batch fixes
the behavior (Option A) rather than narrowing the claim (Option B), so both
documents now describe a landing that exists.

### The defect, at exact line numbers

`crates/codegraph-resolve/src/frameworks/react.rs:188` (pre-change), inside the
`// React Router data-router (v6.4+)` block that begins at line 184:

```rust
let win = byte_window(content, whole.start(), 300);
let comp = obj_element_attr()
    .captures(win)
    .or_else(|| obj_component_attr().captures(win))
    .map(|c| c.get(1).expect("comp group").as_str().to_string());
```

A fixed 300-byte window scanned forward from every `path:` key and took the FIRST
`element:` / `Component:` it met. Nothing bounded it to the route object that owns
that `path`, so a parent route with no `element` of its own silently borrowed a
CHILD route's component out of `children: […]` — the same borrowing defect
`route_opening_tag_window` (react.rs:552-561, Batch D1) fixes for the `<Route>` JSX
form. The `<Route>` half was fixed; this half was not.

Why Option A and not Option B: the object form is bounded by exactly the mechanism
the JSX form already uses — walk forward tracking depth and quotes, stop at the
construct's own terminator, keep a named byte backstop for malformed input. The JSX
terminator is `>` at brace depth 0; the object terminator is `}` at depth 0. Nothing
about the object form made that disproportionate, so narrowing the claim would have
been the weaker choice.

### Green — `route_object_own_properties` (react.rs:608-664)

`route_object_own_properties(content, path_key_start)` returns the route object's
OWN properties as text. It walks from the `path:` key and:

- returns at the `}` / `]` that closes the object (depth 0) — nothing past it belongs
  to this route;
- **elides** nested `{…}` / `[…]` groups instead of merely tracking them, so a
  `children: […]` array cannot leak its `element` into the parent's text and neither
  can a `handle: {…}` object;
- copies quoted values verbatim, so a `}` or `>` inside `path: 'a>b'` neither ends
  the object nor unbalances the depth;
- stops at the next depth-0 `path:` key (`is_path_key_at`, react.rs:666-672) when the
  literal never closes, so even malformed input cannot borrow a following sibling's
  properties;
- is capped by `ROUTE_OBJECT_SCAN_LIMIT` regardless.

This deliberately mirrors `opening_tag_end`'s invariants rather than inventing a
second unrelated scanner: same brace/quote skipping, same "terminate at the
construct's own end, cap the search anyway" pair. It cannot literally REUSE
`opening_tag_end`, because the two forms differ in three ways — the object's
terminator is `}`/`]` and not `>`; `[` must count toward depth for an array-valued
`children`; and nested groups must be ELIDED from the returned text, not merely
skipped over, which is why this function returns an owned `String` while
`route_opening_tag_window` can return a `&str` slice. Their invariants are kept
visibly parallel in the doc comments.

Not fixed by raising 300: a larger fixed window moves the borrowing further away
without removing it, and would newly break the long-object case in the opposite
direction. The bound is structural first, byte-capped only as a backstop.

### The bound and why that number

`ROUTE_OBJECT_SCAN_LIMIT = 4096`, justified in its doc comment from route objects
actually measured (raw walk distance from the object's `path:` key to its own closing
`}`, nested children included because the walk traverses and elides them):

| Route-object shape                                                                       | Bytes |
| ---------------------------------------------------------------------------------------- | ----- |
| minimal inline `{ path, element }`                                                       | 38    |
| typical v6.4 route: `loader` + `errorElement` + `element`                                | 142   |
| plus `action`, `handle: {…}`, `shouldRevalidate`, `element` LAST                         | 407   |
| wide enterprise route: `hydrateFallbackElement` + nested `handle` object, `element` LAST | 611   |
| parent with 8 loader-bearing children, own `element` declared LAST (worst realistic)     | 1503  |

The 1503-byte case is the one that sizes the constant: every child byte must be
traversed before the parent's own `element` is reached, so the walk length is driven
by children count, not by the parent's own property count. 4096 is ~2.7x that, and
strictly wider than the 300-byte window it replaces, so nothing the old code could
reach is lost. Measured with a brace/quote-aware Python walker over each fixture, not
guessed — the same discipline the D1 bound used after its first 512-byte draft was
falsified by a real 700-byte tag.

### Red — measured, before the fix

The five new tests were written first and run against the pre-change 300-byte window
(`cargo test --locked -p codegraph-resolve --test frameworks` → **exit 101**). Actual
output, unabridged assertions:

    ---- react_data_router_parent_without_element_does_not_borrow_from_children ----
    assertion `left == right` failed: a parent route with no element of its own must emit no route->component pair
      left: [("/dashboard", 3), ("settings", 6)]
     right: [("settings", 6)]

    ---- react_data_router_child_without_element_does_not_borrow_from_parent ----
    assertion `left == right` failed: the element-less child must emit nothing; only the parent owns an element
      left: [("/dashboard", 3), ("settings", 5)]
     right: [("/dashboard", 3)]

    ---- react_data_router_object_stops_at_next_path_key_when_unbraced ----
    assertion `left == right` failed: the first path key has no element between it and the second key
      left: ["/a", "/b"]
     right: ["/b"]

    ---- react_data_router_object_survives_angle_brackets_and_stops_at_sibling ----
    assertion `left == right` failed: a `>` inside a quoted path or a braced expression must not end the object, and an element-less object must not borrow from its next sibling
      left: [("/a>b", 2), ("/c>d", 3), ("/plain", 4)]
     right: [("/a>b", 2), ("/plain", 4)]

    ---- react_data_router_keeps_long_route_object_intact ----
    panicked at crates/codegraph-resolve/tests/frameworks.rs:638:10: route node

The first Red is the defect verbatim: `/dashboard` has no `element` of its own, yet
it emitted a route→component pair by borrowing `DashboardHome` from its pathless
`index` child. The last Red is the opposite failure mode — a legitimate 300+-byte
route object whose own `element` sits past the window is dropped entirely, so no
route node exists at all.

### Tests

Five new tests in `crates/codegraph-resolve/tests/frameworks.rs`, all driving the
real `ReactResolver::extract`:

- `react_data_router_parent_without_element_does_not_borrow_from_children` — the
  #1348 data-router shape: parent `path` with NO `element`, a pathless `index` child
  that HAS one, and a `path: 'settings'` child. Asserts the exact route-node list AND
  the exact reference list: parent emits nothing, `settings` emits exactly its own.
- `react_data_router_child_without_element_does_not_borrow_from_parent` — the mirror,
  and the shape that makes the object's own `}` load-bearing: an element-less child
  inside `children: […]` while the parent declares `element` AFTER the array.
- `react_data_router_object_stops_at_next_path_key_when_unbraced` — two depth-0
  `path:` keys with no brace between them; only the next-`path:` stop prevents the
  first from borrowing the second's element.
- `react_data_router_object_survives_angle_brackets_and_stops_at_sibling` — a `path`
  string containing `>`, an `element` whose nested JSX expression contains `>` inside
  braces (`<Spinner size={2 > 1 ? 8 : 4}/>`), and a following element-less sibling
  that must not borrow forward. Shapes the 300-byte window mishandled.
- `react_data_router_keeps_long_route_object_intact` — a route object with `loader`,
  `action`, `errorElement`, `hydrateFallbackElement`, a nested `handle: {…}` and
  `shouldRevalidate`, its own `element` declared LAST and asserted (in the test) to
  sit past 300 bytes. Asserts the reference list is EXACTLY `["BillingSettingsPage"]`,
  so neither the `errorElement` nor the `hydrateFallbackElement` may be mistaken for
  the route's element. Pins the bound from the outside: tightening
  `ROUTE_OBJECT_SCAN_LIMIT` below the object length reddens it.

`react_data_router_object_walk_is_bounded_for_unterminated_literal` also lands here:
an unterminated `{ path: '/a',` followed by 25 000 filler lines and then a
well-formed `/b` route, asserting `/a` reaches neither end-of-file nor `/b`'s
element. It passes both before and after the fix (the old 300-byte cap also bounded
it), so it is recorded as a guard, NOT as Red evidence.

### Negative control, EXECUTED (three mutants, each reddening a DIFFERENT test)

1. Production line reverted to `byte_window(content, whole.start(), 300)`, tests
   untouched → **exit 101**, 5 failed — the full Red block quoted above.

2. Object-boundary terminator removed (the depth-0 `}`/`]` arm `continue`s instead of
   returning), 4096 backstop and elision kept → **exit 101**, **1 failed**:

       ---- react_data_router_child_without_element_does_not_borrow_from_parent ----
       assertion `left == right` failed: the element-less child must emit nothing; only the parent owns an element
         left: [("/dashboard", 3), ("settings", 5)]
        right: [("/dashboard", 3)]

   — a DIFFERENT test from mutants 1 and 3 in isolation, and proof the terminator is
   load-bearing on its own: the child escaped its own `}` and took the parent's
   `element`.

3. Elision removed (nested `{…}`/`[…]` still counted for depth, but their bytes kept
   in the returned text), terminator and backstop intact → **exit 101**,
   **1 failed**:

       ---- react_data_router_object_survives_angle_brackets_and_stops_at_sibling ----
       assertion `left == right` failed: a `>` inside a quoted path or a braced expression must not end the object, and an element-less object must not borrow from its next sibling
         left: [("/plain", 4)]
        right: [("/a>b", 2), ("/plain", 4)]

   — proof elision, not merely depth tracking, is what keeps a nested group's contents
   out of the owning object's text.

Restored after each mutant by copying back the green file; restored SHA-256 of
`crates/codegraph-resolve/src/frameworks/react.rs`:
`647e4a1e5504c479a937bdfe5ca1a05b402076ffac8528260af38885780fadbd`.
Re-run with the fix restored → exit 0, 37 passed.

### Golden row delta

None. `git diff --stat 956cf32..HEAD -- reference/golden/` → EMPTY (956cf32 is the
pre-task HEAD). No golden fixture contains a `<Route` or `createBrowserRouter`
construct (`grep -rln 'createBrowserRouter\|createHashRouter\|createMemoryRouter\|createRoutesFromElements' reference/golden/ crates/codegraph-bench/fixtures/`
→ exit 1, no matches), so the changed branch is unreachable for them.
`cargo test --locked -p codegraph-bench --test equivalence` → exit 0,
**28 passed, 0 failed**. `git status --short reference/golden/` → empty, so this run
left no `colby.db-{wal,shm}` behind to clean.

### Documentation accuracy

- `V1_5_COMMIT_MANIFEST.md` #1348 row now reads
  `Batch D1 bounded React <Route> opening-tag scan + Batch F3 bounded data-router route-object walk`
  — the issue stays `PORT` because it is now genuinely a full behavioral landing, and
  the row names both halves.
- The Batch D1 section's "Exactly the single correct edge the issue asks for" is now
  scoped to the `<Route>` JSX form and carries an inline scope-correction note stating
  that D1 alone did not close #1348.

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, run before every Cargo batch;
  every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-resolve --test frameworks` → exit 101 pre-fix
  (Red, 5 failed), exit 0 post-fix, **37 passed** (31 before this task + 5 new + the
  bounded-walk guard).
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 28/28.
- Mutants 1/2/3 → exit 101 each, reddening 5 / 1 / 1 tests respectively, with mutants
  2 and 3 each reddening a different single test.
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  commit (code, tests, this prose, then `make fmt`); result recorded below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path must
be inside request cwd`); locked Cargo clippy/test is the honest fallback and no
LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte changed. No
native Windows/MSVC runtime validation is claimed — everything above ran on Linux.

## Batch F2 — SQLite sidecar detection is byte-lossless, so a non-UTF-8 project path can no longer hide a committed WAL (2026-07-28)

Fixes the highest-severity finding of the F1–F4 final review wave. This is silent
data staleness, not a crash: on Unix a project path may carry arbitrary non-UTF-8
bytes, and the sidecar-detection gates rendered that path through
`Path::display()` — a LOSSY conversion. Every invalid byte became U+FFFD, so the
constructed `-wal` / `-shm` name pointed at a file that does not exist while the
REAL `codegraph.db-wal` sat undetected beside the database. The strict `Current`
read gate therefore saw a "sidecar-free" namespace, admitted it, deserialized
only the main-database bytes, and served an index missing every row that lived
only in the log. That is exactly the stale-read risk commit `8c66848` reverted as
unsafe, re-opened through a path-encoding hole.

### The defect, at exact line numbers

`crates/codegraph-store/src/connection.rs`, pre-change:

```rust
// 1010  fn first_existing_database_artifact(paths: &IndexPaths) -> Result<Option<PathBuf>> {
// 1013      artifacts.push(PathBuf::from(format!("{}-wal", db.display())));
// 1014      artifacts.push(PathBuf::from(format!("{}-shm", db.display())));

// 1074  fn first_existing_database_sidecar(paths: &IndexPaths) -> Result<Option<PathBuf>> {
// 1077          PathBuf::from(format!("{}-wal", db.display())),
// 1078          PathBuf::from(format!("{}-shm", db.display())),
```

Traced consequences, all confirmed by reading the file:

- `first_existing_database_sidecar` (1074) returns `Ok(None)` — "no sidecar" —
  because it stat's the lossy lookalike.
- The strict gate at `corroborate_current_database_options` (861) therefore never
  raises `StoreError::CurrentWithDatabaseSidecar` and admits the namespace.
- `open_read_only_without_sidecars` (880-912) then deserializes ONLY
  `std::fs::read(db_path)`, so WAL-only rows are invisible. `Current`, stale, no
  error.
- `first_existing_database_artifact` (1010) feeds
  `reject_missing_database_artifacts` (1087), so the `Missing`-state guard was
  equally blind: an unexplained `-wal` beside a stateless namespace on a
  non-UTF-8 path was accepted as a fresh writable namespace.

The correct idiom was already present in this very crate: `sqlite_sidecar_path`
(`crates/codegraph-store/src/uninit.rs:233`) builds sidecar names with
`OsString::push`, and `connection.rs` itself already had the lossless
`database_sidecar_path` (1057) — used by `remove_checkpointed_sidecars` (1032-1050)
but NOT by either detection function. The removal path was lossless while the
DETECTION path was lossy; that asymmetry is the whole bug.

### The fix — one helper, every site

`database_sidecar_path` is now the single source of every sidecar name in
`connection.rs`. Three call sites changed; the helper's body is unchanged:

- `first_existing_database_artifact` → `database_sidecar_path(&db, "-wal"/"-shm")`.
- `first_existing_database_sidecar` → `database_sidecar_path(&db, "-wal"/"-shm")`.
- the unit-test cleanup loop in `reopening_existing_db_keeps_schema_version`
  (previously `format!("{}{ext}", db_path.display())`) → the same helper, so no
  `Path::display()` path construction remains anywhere in the file.

The helper's doc comment now states the invariant and the exact failure mode, so a
future edit cannot reintroduce a lossy sibling without contradicting it.

Nothing else moved. `Store::open_for_read` still defaults to
`allow_live_sidecar = false`; no sidecar-tolerant read path exists under any name;
the `Current` corroboration order (tombstone → database presence → sidecar
freedom → main-file stamp) is byte-identical; recovery still refuses to delete a
non-empty `-wal` (`StoreError::WalNotFolded`), still revalidates the exclusive
lease before each unlink, still leaves the `-journal` for SQLite to replay, and
still keeps its bounded 500ms lease deadline and its "dead owner proven first"
precondition. No PID signalling was added. Recovery is now merely ABLE to see the
residue it was always supposed to see.

### Workspace-wide audit of the bug class

`grep -rn 'display()' crates/ --include='*.rs' | wc -l` → **501 hits, exit 0**.
Filtering for hits that actually construct a filesystem path
(`PathBuf::from` / `Path::new` / `.push(` / a `format!` bound to a path variable)
leaves eleven. Classification of every one:

| Location                                                                               | Verdict                                                                                                                                                                                                                                                                                                                                                                                           |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `codegraph-store/src/connection.rs:1013,1014` (was)                                    | **BUG — PATH construction.** Fixed: `database_sidecar_path`.                                                                                                                                                                                                                                                                                                                                      |
| `codegraph-store/src/connection.rs:1077,1078` (was)                                    | **BUG — PATH construction.** Fixed: `database_sidecar_path`.                                                                                                                                                                                                                                                                                                                                      |
| `codegraph-store/src/connection.rs:1323` (was)                                         | **BUG (unit-test cleanup only) — PATH construction.** Fixed: same helper. Impact was a leaked temp file, not staleness, but it is the same class.                                                                                                                                                                                                                                                 |
| `codegraph-store/src/uninit.rs:557`                                                    | **CORRECT BY DESIGN.** Inside `non_utf8_database_sidecars_are_removed_without_touching_lossy_lookalikes`; deliberately builds the lossy lookalike as the file that must SURVIVE.                                                                                                                                                                                                                  |
| `codegraph-store/tests/store_state_gates.rs:1163,1192,1250`                            | **BENIGN — test fixtures on ASCII temp paths** (`std::env::temp_dir()` + an ASCII label), where `display()` is a lossless round-trip. Left unchanged so the pre-existing ASCII coverage keeps asserting the exact same bytes; the new non-UTF-8 tests use the lossless `sqlite_sidecar` helper that already existed at line 134.                                                                  |
| `codegraph-store/tests/store_state_gates.rs:1337` (new)                                | **INTENTIONAL.** The new `lossy_sidecar` helper — it exists precisely to name the lookalike that must not be touched.                                                                                                                                                                                                                                                                             |
| `codegraph-cli/tests/batch_m_finalizer.rs:135`                                         | **BENIGN — test fixture on an ASCII temp path.** Same reasoning.                                                                                                                                                                                                                                                                                                                                  |
| `codegraph-watch/src/watcher.rs:502`                                                   | **FINE — message only** (`format!("watch {} failed: {err}", …)`, a human-readable reason string).                                                                                                                                                                                                                                                                                                 |
| `codegraph-cli/src/main.rs:768,6112,6254` and `codegraph-cli/tests/completions.rs:104` | **FINE — shell-snippet TEXT, not a path used by this process.** `format!(". \"{}\"", script.display())` is a line PRINTED for the user to paste into their rc file. It is not opened, stat'd, or unlinked. A non-UTF-8 completion-script path would render imperfectly in that printed hint; no filesystem decision depends on it, so it is a display-quality nit and out of this commit's scope. |

The remaining ~490 `display()` uses are all inside `panic!` / `assert!` /
`expect` / `println!` / `tracing::` / `with_context` / `#[error]` message strings —
formatting for humans, never a path this process then acts on. Two related lossy
conversions were also checked and are NOT path construction that can go wrong the
same way: `codegraph-core/src/index_paths.rs:179` (`to_string_lossy` on a
configured-root file NAME, which is then hashed into a deterministic sibling
directory name — a lossy rendering there produces a stable, still-unique name and
is pre-existing published behavior, out of scope for this blocker), and
`codegraph-cli/src/installer/{targets/kiro.rs:53,targets/qoder.rs:142,mod.rs:491}`
(agent-config JSON values, i.e. text emitted for another program's config file).

### Red — measured, before the fix

`cargo test --locked -p codegraph-store --test store_state_gates` →
**exit 101, 22 passed / 4 failed**. The four new tests, verbatim:

    ---- non_utf8_current_with_committed_wal_fails_closed stdout ----
    panicked at crates/codegraph-store/tests/store_state_gates.rs:1383:14:
    non-UTF-8 Current with committed WAL must fail closed: Store { conn: Connection { path: Some("") }, …
      path: "/tmp/…/proj-\x80\xFF/.codegraph-v2/codegraph.db", access: StateRead, … }

    ---- non_utf8_dead_owner_wal_is_folded_and_then_readable stdout ----
    panicked at crates/codegraph-store/tests/store_state_gates.rs:1423:5:
    a dead owner's committed WAL must be DETECTED on a non-UTF-8 path

    ---- non_utf8_recovery_never_touches_the_lossy_lookalike_sidecars stdout ----
    panicked at crates/codegraph-store/tests/store_state_gates.rs:1502:10:
    strict gate admits the recovered non-UTF-8 namespace: CurrentWithDatabaseSidecar { path: "/tmp/…/proj-��/.codegraph-v2/codegraph.db-wal" }

    ---- missing_state_with_non_utf8_database_sidecar_fails_closed stdout ----
    panicked at crates/codegraph-store/tests/store_state_gates.rs:1532:14:
    Missing plus a DB sidecar is not a fresh writable namespace: FullRebuildRequired(StoreWriteAuthorization { status: Missing, purpose: FullRebuild, … })

The first failure IS the blocker, stated as an assertion: `Store::open_for_read`
RETURNED A READABLE `Store` over a namespace holding a committed, un-checkpointed
write-ahead log. The third failure's message is the smoking gun in reverse — the
error path names the U+FFFD-rendered `proj-��/…/codegraph.db-wal`, i.e. the gate
was reporting a file that does not exist while the real one went unseen.

### Green

Same command after the fix → **exit 0, 26 passed, 0 failed** (22 pre-existing + 4
new).

### Tests added — `crates/codegraph-store/tests/store_state_gates.rs`

All four are `#[cfg(unix)]`. The in-code comment states why: a Unix path is an
arbitrary byte string, whereas Windows paths are UTF-16 and the byte-oriented
constructors these fixtures need (`std::os::unix::ffi::OsStringExt::from_vec`) do
not exist there. The non-UTF-8 project directory is built as
`"<label>-" ++ [0x80, 0xFF]`, and every test asserts
`native_path != lossy_path` FIRST, so a fixture that silently stopped being
non-UTF-8 fails loudly instead of passing vacuously.

1. `non_utf8_current_with_committed_wal_fails_closed` — the blocker's direct
   inverse. Plants a REAL committed WAL by re-invoking the existing
   `run_crashed_current_writer` child (which commits with `wal_autocheckpoint=0`
   and then `exit(91)` without running destructors), asserts the log is non-empty
   and that the probe row is absent from the main file, then asserts BOTH
   `open_for_read` and `open_for_status` refuse with
   `CurrentWithDatabaseSidecar { path }` where `path` is the NATIVE `-wal`, and
   that the tree is byte-unchanged.
2. `non_utf8_dead_owner_wal_is_folded_and_then_readable` — the recovery half.
   `recover_stale_current_sidecars` must return `Ok(true)` (i.e. must SEE the
   residue), both sidecars must be gone, the WAL-only row must now be in the main
   file, and the strict gate must then admit the namespace and return
   `wal_only_probe = "committed"`. It also asserts the permanent lock, the
   published state slot, tombstone absence and `Current` status all survive.
3. `non_utf8_recovery_never_touches_the_lossy_lookalike_sidecars` — the shape the
   fix must NOT break. Plants real residue AND writes decoy files at the LOSSY
   `-wal` / `-shm` names, then asserts recovery removes the native WAL while both
   decoys keep their exact bytes. This is the test that would catch a "fix" that
   merely swapped which wrong file it deletes.
4. `missing_state_with_non_utf8_database_sidecar_fails_closed` — the second gate,
   which the audit found shares the same construction. A `-wal`/`-shm` beside a
   stateless namespace must yield `MissingStateWithDatabase { path }` (native
   path) from write, read AND status opens, non-mutating.

No existing test was weakened, skipped, `#[ignore]`d or deleted, and no sleep was
added; the fixtures are child-process + polling based exactly as before.

### Negative control, EXECUTED (two mutants, each reddening a DIFFERENT test set)

Green SHA-256 of `crates/codegraph-store/src/connection.rs`:
`f7c0884560e033d6858b7622ceb124efac1a8808f99b8b57eb2d1ad4230568b4`.

1. `first_existing_database_sidecar` reverted to `display()` ONLY (the artifact
   function left lossless) → **exit 101, 23 passed / 3 failed**:
   `non_utf8_current_with_committed_wal_fails_closed`,
   `non_utf8_dead_owner_wal_is_folded_and_then_readable`,
   `non_utf8_recovery_never_touches_the_lossy_lookalike_sidecars`.
   `missing_state_with_non_utf8_database_sidecar_fails_closed` stayed GREEN —
   proof the two gates are covered independently.

2. `first_existing_database_artifact` reverted to `display()` ONLY (the sidecar
   function left lossless) → **exit 101, 25 passed / 1 failed**, the complement:

       ---- missing_state_with_non_utf8_database_sidecar_fails_closed stdout ----
       panicked at crates/codegraph-store/tests/store_state_gates.rs:1529:14:
       Missing plus a DB sidecar is not a fresh writable namespace: FullRebuildRequired(StoreWriteAuthorization { status: Missing, purpose: FullRebuild, lease: IndexLease { … db_parent: "/tmp/…/proj-\x80\xFF/.codegraph-v2" … } })

   — and the three `non_utf8_*` tests stayed GREEN. A partial regression of either
   detection site therefore cannot hide behind the other.

Restored after each mutant by copying back the green file; restored SHA-256 of
`crates/codegraph-store/src/connection.rs` re-verified as
`f7c0884560e033d6858b7622ceb124efac1a8808f99b8b57eb2d1ad4230568b4`.

### Golden row delta

None. This is a filesystem-path-construction change in the store's gate layer;
extraction, resolution and serialization are untouched.
`git diff --stat 956cf32..HEAD -- reference/golden/` → EMPTY (956cf32 is the
pre-task HEAD). `cargo test --locked -p codegraph-bench --test equivalence` →
exit 0, **28 passed, 0 failed**. Any untracked
`reference/golden/*/colby.db-{wal,shm}` the oracle leaves behind was removed
before staging (`git status --short reference/golden/` → empty).

### Verification (actual exit statuses)

- `bash scripts/check-workspace-versions.sh` → exit 0, run before every Cargo
  batch; every Cargo command used `--locked`.
- `cargo test --locked -p codegraph-store --test store_state_gates` → exit 101
  pre-fix (Red, 4 failed), exit 0 post-fix, **26 passed**.
- `cargo test --locked -p codegraph-store` (all targets) → recorded below.
- `cargo test --locked -p codegraph-bench --test equivalence` → exit 0, 28/28.
- Mutants 1/2 → exit 101 each, reddening 3 and 1 tests respectively — disjoint sets.
- `make ci CARGO='cargo --locked'` → final gate, run after the last byte of this
  commit (code, tests, this prose, then `make fmt`); result recorded below.
- `sha256sum Cargo.lock` →
  `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`, unchanged.

`lsp_diagnostics` was attempted and again refused this worktree (`LSP file path must
be inside request cwd`); locked Cargo clippy/test is the honest fallback and no
LSP-clean result is claimed. No dependency, version or `Cargo.lock` byte changed. No
native Windows/MSVC runtime validation is claimed — every command above ran on
Linux, and the Windows arm of `database_sidecar_path` is exercised only by
inspection (`OsString::push` on a `PathBuf`'s `OsStr`, which is lossless on both
platforms by construction).

## F4 Blocker B3 — user-facing docs still pointed at the abandoned `.codegraph` root (2026-07-28)

Batch M moved the per-project index to an isolated `.codegraph-v2` root that new
binaries never open, migrate, or write. The shipped documentation was never
updated, so a user following the README got a silent no-op (their `v0.40.4` index
and config are ignored) and the golden regeneration recipe copied from a path that
no longer exists. This is a documentation-only correction: no `.rs`, `Cargo.toml`,
or `Cargo.lock` byte changed, and `scripts/guardrail.sh` changed by one COMMENT
line with no behavior change (`bash -n` exit 0).

### What the code actually says (read before writing any prose)

- `index_paths.rs:46,48` — `DEFAULT_CURRENT_DIR = ".codegraph-v2"`,
  `LEGACY_DIR = ".codegraph"`. `legacy_roots()`'s own doc comment states new
  binaries never open, migrate, or write these.
- EVERY per-file accessor hangs off `current_root`: `current_db`, `config_toml`,
  `extension_config`, `permanent_lock`, `state_slots`, `tombstone`, AND
  `daemon_pid` / `daemon_log` / `daemon_socket` (`index_paths.rs:266-331`).
- `codegraph-daemon/src/paths.rs` — module doc: "Nothing here reconstructs a
  `.codegraph*` path: every value comes from `IndexPaths::resolve`".
  `rendezvous_dir` returns `current_root()`.
- `config.rs:267-297` — `Config::load_for_paths` "never consults a legacy
  `.codegraph/config.toml`".
- `codegraph-extract/src/ext_config.rs:49-55` — `load_for_paths` "Reads exactly
  `paths.extension_config()`". One file; no tree walk; no mtime cache.
- `cmd_sync` (`main.rs:1330-1366`) → `sync_project_once_with_progress`:
  hash-gated per-file skip, per-file delete/reinsert, full re-resolve; reports
  `files_reindexed` / `files_skipped_unchanged` / `files_removed`.
- `codegraph-watch/src/policy.rs:191` — the always-ignore rule skips `.git`,
  `.codegraph`, AND any `.codegraph-*`, so BOTH spellings are correctly named in
  the watcher-pruning prose.

**The B3 brief's premise was wrong and is corrected here.** It stated `.codegraph/`
is still the daemon rendezvous while only index and config moved. It is not — the
rendezvous moved WITH the index. Verified empirically, not just by reading:

```
$ codegraph init /tmp/cg-fixture-godot     # exit 0; creates .codegraph-v2/ ONLY
$ codegraph status /tmp/cg-fixture-godot --json
  indexPath        = /tmp/cg-fixture-godot/.codegraph-v2/
  dbPath           = …/.codegraph-v2/codegraph.db
  daemonPidPath    = …/.codegraph-v2/daemon.pid
  daemonSocketPath = …/.codegraph-v2/daemon.sock
  daemonLogPath    = …/.codegraph-v2/daemon.log
  legacyIndexPaths = []
```

### How index/config/rendezvous paths were distinguished from legitimate `.codegraph`

Not by string replacement. Each site was classified against the accessor that
produces it:

1. **Live path → `.codegraph-v2/`.** Anything naming the index DB, `config.toml`,
   `codegraph.json`, the indexed-project marker, or the daemon pid/socket/log —
   all derived from `current_root`.
2. **Watcher/scanner ignore set → BOTH spellings kept.** `policy.rs:191` skips
   `.codegraph` and `.codegraph-*`, so `docs/cli.md`'s pruning list now names
   `.codegraph` AND `.codegraph-v2`. Dropping the legacy name would have made the
   doc contradict the code.
3. **Deliberate legacy/historical discussion → `.codegraph/` retained.** The new
   upgrade notes, and the "a legacy X is ignored" clauses, must name the old path
   to be meaningful.

### Stale sites found and corrected

| File                          | Correction                                                                                                                                                                                                                                                                                                       |
| ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `README.md`                   | 2× `init` comments, skill blurb, `daemon.sock`, extension+config paths; NEW "Upgrading from v0.40.4" section + TOC entry                                                                                                                                                                                         |
| `docs/readme/README.zh-CN.md` | same five sites mirrored + NEW「从 v0.40.4 升级」section + TOC entry                                                                                                                                                                                                                                             |
| `docs/cli.md`                 | `init`/`uninit` rows; **`sync` row rewritten** (was "currently reuses the safe full-index path"); serve-mode step 2; daemon log/socket/pid; watcher list now names both spellings; extension-config heading+body; **resolution rule corrected** (was "walks up the directory tree … mtime-cached" — wrong twice) |
| `docs/mcp.md`                 | 5 sites: Zed-SSH ×2, indexed-project marker, find-up target, `daemon.log`                                                                                                                                                                                                                                        |
| `skills/codegraph/SKILL.md`   | front-matter trigger, DB blurb, detect-index block + fence, `init` comment, fallback list; NEW legacy-root note                                                                                                                                                                                                  |
| `docs/equivalence.md`         | 10 `cp` paths + 3 comments; NEW reproducibility caveats (below)                                                                                                                                                                                                                                                  |
| `docs/architecture.md`        | subcommand count 13→26; removed `init_config` (now `Config::load_env_or_default`, logger-only); DB path; **`sync` rewritten**; rendezvous → current root                                                                                                                                                         |
| `docs/data-model.md`          | fixture DB path ×2                                                                                                                                                                                                                                                                                               |
| `docs/languages.md`           | extension-config path + resolution rule corrected (was "nearest config up the directory tree wins")                                                                                                                                                                                                              |
| `editors/zed/README.md`       | remote-SSH index path                                                                                                                                                                                                                                                                                            |
| `AGENTS.md`                   | rendezvous path; release surface 4-platform → **6**, naming `x86_64-pc-windows-msvc` (`windows-latest`) and `aarch64-pc-windows-msvc` (`windows-11-arm`) — verified against `release-please.yml:191-214`                                                                                                         |
| `scripts/guardrail.sh`        | "AND both CI jobs" → "AND the CI `test` job (the `windows` job does NOT run it)" — verified: `ci.yml` runs guardrail only at line 76 in `test`                                                                                                                                                                   |

### `docs/equivalence.md` — the recipe was verified end to end, not just path-edited

Ran the corrected Godot recipe with the built binary. `cp` from
`.codegraph-v2/codegraph.db` then `--gen-golden` (exit 0) reproduced
`nodes.json`, `edges.json`, `refs.json`, `files.json` **byte-identically** to the
committed golden. Two things the path fix ALONE would not have caught, both now
documented as explicit non-claims:

- **`colby.db` is NOT byte-reproducible.** A freshly indexed DB differs from the
  committed one at byte 28 (SQLite header change counter) despite identical rows.
- **`schema.sql` records `.schema` statement ORDER, which shifted.** A fresh DB
  puts `idx_edges_identity` at `.schema` line 103; the committed `colby.db` has it
  at line 116. Confirmed this is an artifact of WHICH BINARY created the DB, not of
  the doc edit: re-dumping the COMMITTED `colby.db` reproduces the committed
  `schema.sql` byte-identically (all 5 artifacts IDENTICAL), while a freshly
  indexed DB yields the order-only diff. Steps 3+4 keep the committed pair
  self-consistent because both come from the same database.

`reference/golden/` is unchanged — the verification ran entirely in `/tmp`.

### Out of scope, deliberately left alone

`docs/godot.md` carries three `.codegraph/config.toml` / `codegraph.json` hits
(lines 27, 275, 388). It is owned by a sibling task in this wave and was NOT
touched. Those sites remain stale and need the same correction in a follow-up.
`docs/upstream-sync/UPSTREAM.md` and the historical body of this ledger keep their
`.codegraph` spellings by design: they describe what upstream or an earlier slice
did at the time.

### Commands and their REAL exit statuses

Run individually, never `&&`-joined, so no tail is hidden.

| Command                                         | Exit | Note                                                                        |
| ----------------------------------------------- | ---- | --------------------------------------------------------------------------- |
| `cargo build --locked -p codegraph-rs`          | 0    | binary used for every empirical check below                                 |
| `codegraph init /tmp/cg-fixture-godot`          | 0    | created `.codegraph-v2/` ONLY; 6 files, 15 nodes, 13 edges                  |
| `codegraph status /tmp/cg-fixture-godot --json` | 0    | all five paths under `.codegraph-v2/`; `legacyIndexPaths = []`              |
| `--gen-golden` from a FRESH index               | 0    | 4 JSON artifacts IDENTICAL; `schema.sql` order-only diff; `.db` differs @28 |
| `--gen-golden` from the COMMITTED `colby.db`    | 0    | all 5 artifacts IDENTICAL — isolates the diff to the creating binary        |
| `bash -n scripts/guardrail.sh`                  | 0    | comment-only edit is syntactically inert                                    |
| `make fmt`                                      | 0    | oxfmt, 23 docs files; run BEFORE the gates so `fmt-check` cannot trip       |
| `bash scripts/check-workspace-versions.sh`      | 0    | workspace / `version.txt` / release manifest all `0.40.4`                   |
| `make ci CARGO='cargo --locked'` (run 1)        | 2    | FAILED — see the flake note below                                           |
| `make ci CARGO='cargo --locked'` (run 2)        | 0    | PASSED — `✅ All CI checks passed!`                                         |

**Run 1 failure was the known pre-existing flake, reported honestly rather than
hidden.** Two failures in `codegraph-rs`'s bin tests, 351 passed:

- `install_completions_writes_zsh_fish_elvish_into_home` — `assert!(elv.is_file())`
  at `main.rs:6220`. This is the documented in-process `HOME`/`XDG_DATA_HOME` race
  (~4 failures in 8 runs on a clean commit). NOT weakened, NOT touched.
- `codegraph_dir_and_db_path_default_layout` — `PoisonError` at `main.rs:5740`.
  This is COLLATERAL, not an independent failure: it takes the same
  `ENV_LOCK` (`main.rs:5704`) with a bare `.unwrap()`, so the first test's panic
  while holding that mutex poisons it and fails this one too. The flaky test uses
  `unwrap_or_else(|e| e.into_inner())` and so tolerates poisoning; this one does
  not. Both passed in run 2.

No source change was made to address either — they are pre-existing and outside
this documentation task's scope.

**A SECOND, DIFFERENT pre-existing flake surfaced on a later run** (`make ci` runs
3 and 4, after the ledger prose was appended):

| Command                                  | Exit | Note                                   |
| ---------------------------------------- | ---- | -------------------------------------- |
| `make ci CARGO='cargo --locked'` (run 3) | 2    | FAILED — `Text file busy` flake, below |
| `make ci CARGO='cargo --locked'` (run 4) | 0    | PASSED — `✅ All CI checks passed!`    |

`verify_legacy_binary_rejects_a_wrong_executable_version`
(`batch_m_legacy_extension_override.rs:1804`, 20 passed / 1 failed) failed with
`run configured legacy binary …/stub-legacy --version: Text file busy (os error
26)` — ETXTBSY: the test writes an executable stub and immediately execs it, and
on Linux the exec can race the writer's still-open fd. The assertion is intact and
correct; it simply never reached the version branch. Distinct from the
`install_completions` flake and likewise pre-existing: `git diff --name-only`
matched NO `.rs` file at any point in this task (exit 1), so no test source was
touched, weakened, or skipped. Run 4 passed with the identical tree.

**Golden byte-stability.** `git diff --stat <base>..HEAD -- reference/golden/` is
EMPTY. During `make ci` the test suite left two untracked SQLite sidecars
(`reference/golden/ruby/colby.db-shm`, `-wal`) from opening the golden read-only;
they were deleted, and `git status --porcelain reference/golden/` is empty. Every
committed golden byte is untouched, `colby.db` included.

**Scope proof.** `git diff --name-only <base>..HEAD | grep -E '\.rs$|Cargo\.(toml|lock)'`
returns NOTHING (exit 1). `Cargo.lock` SHA-256 is unchanged at
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

**Not claimed.** No native Windows/MSVC validation — every command above ran on
this Linux host. The six-platform release surface was verified by READING
`release-please.yml:191-214`, not by building the Windows targets.

### Final `.codegraph/` sweep

`grep -rn '\.codegraph/' --include='*.md' .` (excluding `node_modules`,
`reference/golden`, and this ledger's own historical body) leaves only CORRECT
hits, each justified:

- `README.md:69,72,77,78` + `docs/readme/README.zh-CN.md:64,66,71` — the new
  upgrade notes. They MUST name the old root to tell a `v0.40.4` user what is
  being ignored.
- `skills/codegraph/SKILL.md:41` — the legacy-root note ("an index left by
  `v0.40.4` or earlier … never read or migrated").
- `docs/cli.md:658`, `docs/languages.md:112` — "a legacy `.codegraph/codegraph.json`
  is ignored", the corrected resolution rule.
- `docs/godot.md:27,275,388` — sibling-owned file, deliberately NOT touched; still
  stale, tracked above as follow-up.

`docs/cli.md`'s watcher-pruning list intentionally names BOTH `.codegraph` and
`.codegraph-v2` (it is an ignore set covering both spellings, per
`policy.rs:191`), so it does not appear as a `.codegraph/` hit but is correct.

## Batch F1 — the gdext decision no longer rests on a false build-time claim (2026-07-29)

`docs/godot-gdext-decision.md` (added by `956cf32`) argued the right conclusion
from a verifiably wrong premise. An independent audit caught it. The verdict is
unchanged — `gdext` is still rejected, and still unnecessary — but an argument a
reader can falsify in five minutes discredits the whole record, so the premise is
corrected rather than defended.

### The incorrect claim, verbatim

From "What gdext actually is":

> Its type surface (`Node`, `Callable`, `NodePath`, …) is generated from
> `extension_api.json`, which ships with a specific Godot build

and, restating it, from disqualifier 1 ("It requires the engine; CodeGraph
deliberately does not"):

> `gdext` would make Godot analysis conditional on having a matching Godot binary
> and its `extension_api.json` present at build time.

### The official source that refutes it

`https://docs.rs/godot/latest/godot/#cargo-features` (crate `godot`, version
`0.5.4`, the `gdext` binding's public crate). Under _Godot version and
configuration_ it documents `api-4-{minor}`, `api-custom` and `api-custom-json`
together:

> Sets the **API level** to the specified Godot version, or a custom-built local
> binary. You can use at most one `api-*` feature. If absent, the current Godot
> minor version is used, with patch level 0.
>
> `api-custom` feature requires specifying `GDRUST_GODOT_BIN` environment
> variable with a path to your Godot4 binary.
>
> The `api-custom-json` feature requires specifying `GDRUST_GODOT_API_JSON`
> environment variable with a path to your custom-defined `extension_api.json`.

So a plain `gdext` dependency builds against an API description bundled with the
crate and pinned by the crate version; an engine binary is needed only under
`api-custom`, and a hand-supplied `extension_api.json` only under
`api-custom-json`. `https://docs.rs/crate/godot/latest/features` corroborates the
opt-in status: of 22 features exactly one (`__codegen-full`) is on by default —
neither `api-custom` nor `api-custom-json` is.

### What changed

- "What gdext actually is" now says the type surface is generated from Godot's
  `extension_api.json` and is only meaningful once the engine has initialized,
  and adds an explicit paragraph stating that an earlier draft got the build step
  wrong, quoting the docs.rs text and naming `api-custom` / `api-custom-json`
  with their environment variables.
- Disqualifier 1 is reframed from build time to **run** time: `gdext` code only
  executes as a GDExtension loaded by a Godot process, so `codegraph index` would
  stop being a self-contained command. That is the true and sufficient form of
  the objection.
- The verdict paragraph still opens with the rejection, now resting on the
  load-bearing reason alone: no offline API for reading `.gd` / `.tscn` /
  `.tres` / `project.godot`.
- Former disqualifier 3 ("It solves none of the recorded gaps") is renumbered 2.

Untouched and still valid: the four ranked alternatives and their ordering, the
`unverified` marking on whether a newer `tree-sitter-gdscript` than the pinned
`6.1.0` exists, the `godot:dynamic:*` sentinel defence, and the recommendation
(not action) to widen `scripts/guardrail.sh`. No "Conclusion" section was added.

### Why disqualifier 2 does not survive

The dropped disqualifier claimed adopting `gdext` "breaks byte-stable
determinism" because "[b]indings generated from a specific engine version make
output a function of that version, so the same corpus could produce different
goldens on different release runners". With the prebuilt default that premise is
gone: the generated surface is fixed by the crate version, exactly as
reproducible as any other pinned dependency, and it becomes runner-dependent only
under `api-custom` (engine binary) or `api-custom-json` (local JSON) — features
nobody proposed. Kept as written it would be a second false claim; narrowed to
"true only for `api-custom`" it would be an objection to a configuration nobody
suggested, i.e. filler. The verdict does not need three disqualifiers, so it is
**dropped rather than kept for symmetry**. What replaces it is a sharper true
observation folded into the corrected prose: the real determinism risk was never
the bindings but that any output derived from a live engine's state is not a pure
function of the corpus — which is the runtime problem disqualifier 1 already
states.

### Gates — real exit statuses

Run in the mandated order, each separately (no `&&` chaining, so no tail is
hidden):

| Command                                       | Exit                               |
| --------------------------------------------- | ---------------------------------- |
| `make fmt`                                    | **0**                              |
| `bash scripts/check-workspace-versions.sh`    | **0** (`OK`, workspace `0.40.4`)   |
| `make ci CARGO='cargo --locked'` — final gate | **0** (`✅ All CI checks passed!`) |

The final `make ci` above is the one taken after the last byte of this batch, and
it exited 0. It was not the only run: `make ci` was invoked repeatedly here, once
after the first ledger draft and again after each prose correction, and MORE THAN
HALF of those invocations exited non-zero (`101` from `make: *** [Makefile:127:
test] Error 101`, surfacing as `2` from `make ci` itself). Every single red run
failed on the KNOWN pre-existing flake
`formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home`
and on nothing else: 352 passed / 1 failed every time, with the assertion landing
on a different line from run to run (`main.rs:6213`, then `main.rs:6224`), and one
log showing a completion actually written into the real
`/config/.config/fish/completions/` instead of the test's temp `HOME`. That is the
documented in-process `HOME`/`XDG_DATA_HOME` race, not a docs regression — this
batch changes two Markdown files and no Rust at all. The flake is reproducing more
often on this host than the ~4-in-8 previously recorded. The test was NOT
weakened, skipped or ignored, and the red runs are recorded here rather than
quietly discarded.

The equivalence oracle also left untracked
`reference/golden/{erlang,mini}/colby.db-{wal,shm}` behind on several runs; they
are not gitignored and were deleted before staging, so the commit contains no
golden-directory byte.

**Scope.** `git diff --name-only e728937..HEAD` lists only
`docs/godot-gdext-decision.md` and this ledger. `git diff --stat
e728937..HEAD -- reference/golden/` is empty; `Cargo.lock` SHA-256 unchanged at
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

**Not claimed.** No native Windows/MSVC validation — every command ran on this
Linux host. The docs.rs pages were read over the network as cited; no `gdext`
build was attempted, and none is needed to verify a documented feature contract.

## F4 round-two documentation blockers: two false statements corrected

Round two of the F1–F4 review wave rejected F4 on four blockers. Two were false
statements in committed documentation. Both are corrected here; the other two
were adjudicated (a claimed daemon-path contract conflict rested on a
mis-briefing, and the known test flakes predate this branch). Documentation only:
no `.rs`, no `Cargo.*`, no workflow, no `scripts/`, no golden byte.

### Blocker 1 — `docs/equivalence.md` named a file that does not exist

The committed text, verbatim (`docs/equivalence.md:739-740` at `c8968ad`):

> Tier-3 differences are allowlisted by grep-able lines in repo-root
> `KNOWN_DIFFS.md`:

`ls KNOWN_DIFFS.md` → `No such file or directory` (exit 2). The file that does
exist is `docs/upstream-sync/KNOWN_DIFFS.md` (11133 bytes), and it is the ONLY
path the oracle ever reads —
`crates/codegraph-bench/src/oracle/diff.rs:141-147`:

```rust
pub fn repo_doc_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-bench lives under crates/")
        .join("docs/upstream-sync/KNOWN_DIFFS.md")
}
```

The path is now named correctly, attributed to `KnownDiffs::repo_doc_path`.

The surrounding section also still described only the pre-`5c585ec` permissive
contract. Each added claim was read off `diff.rs` before it was written:

- Tier gate on the query side — `allows` returns `false` for any
  `entry.tier != Tier::Tier3` (`diff.rs:190-193`).
- Tier gate at parse time — `tier=1`/`Tier1`/`tier1`/`2`/`Tier2`/`tier2` bail
  with "may not be allowlisted"; any other value bails as "unknown tier"
  (`diff.rs:234-240`).
- Token shape — a token without `=` bails; an empty key or value bails
  (`diff.rs:218-224`).
- Unknown field — rejected against `RULE_FIELDS = ["tier", "surface", "key",
"justification"]` (`diff.rs:21`, `225-227`).
- Duplicate field — rejected (`diff.rs:228-230`).
- Surface — rejected unless it is one of `DIFF_SURFACES = ["nodes", "files",
"schema", "edges", "unresolved_refs"]` (`diff.rs:18`, `242-245`).
- Missing field — all four of `tier`, `surface`, `key`, `justification` are
  `.context("missing …")?` (`diff.rs:233`, `242`, `250-258`).
- Fenced lines are documentation, and an unterminated fence is an error
  (`diff.rs:166-188`).
- Fail-closed loading — `load_repo_doc` propagates a parse error, so an
  unparsable document fails the assertion instead of being ignored
  (`diff.rs:149-154`).

Nothing was asserted that `diff.rs` does not state. The wildcard semantics
(`key_pattern == "*"` or `entry.key.contains(&rule.key_pattern)`,
`diff.rs:194-198`) were left as the existing template already implies them.

### Blocker 2 — `docs/godot.md` still pointed at the retired index root

`ddd6b27` corrected eleven documents and missed this one. The three stale sites,
verbatim (`docs/godot.md` at `c8968ad`):

> in `.codegraph/config.toml`. That list replaces the default set entirely, so (line 27)

> `.codegraph/codegraph.json`. List the `[resource]` property names that should (line 275)

> `indexing.ignore_dirs` list in `.codegraph/config.toml`. See (line 388)

All three describe live paths a user is told to create. The code puts both files
under the current root: `IndexPaths::config_file` is
`self.current_root.join("config.toml")` and `extension_config` is
`self.current_root.join("codegraph.json")`
(`crates/codegraph-core/src/index_paths.rs:304-312`), and `current_root`
defaults to `DEFAULT_CURRENT_DIR = ".codegraph-v2"` (`index_paths.rs:46`).
`.codegraph` survives only as `LEGACY_DIR` (`index_paths.rs:48`), a root that is
ignored. All three now read `.codegraph-v2/…`.

The daemon rendezvous is the same root, not a surviving `.codegraph/` role:
`crates/codegraph-daemon/src/paths.rs:31-33` is
`Ok(index_paths(project_root)?.current_root().to_path_buf())`, and that file's
own test asserts the result `starts_with(project.join(".codegraph-v2"))`
(`paths.rs:145-152`). `daemon_pid`, `daemon_log` and `daemon_socket` are all
`current_root.join(…)` (`index_paths.rs:315-330`). An earlier review round was
mis-briefed that the rendezvous stayed at `.codegraph/`; it does not, and no
`.codegraph/` role was introduced here.

`docs/godot.md` now has zero `.codegraph` hits that are not `.codegraph-v2`, so
no legacy mention was kept in that file.

### Final grep sweeps

`grep -rn '\.codegraph/' --include='*.md' . | grep -v node_modules` — 35 hits
remain, none of them a live-path instruction:

- `README.md:69,72,77,78` and `docs/readme/README.zh-CN.md:64,66,71` — the
  migration note. Every mention is explicitly the OLD root being ignored and not
  carried over.
- `skills/codegraph/SKILL.md:41` — "A `.codegraph/` directory is an index left by
  `v0.40.4` or earlier", explicitly labelled legacy.
- `docs/cli.md:658`, `docs/languages.md:112` — "A legacy
  `.codegraph/codegraph.json` is ignored", explicitly labelled legacy.
- `docs/upstream-sync/UPSTREAM.md:688,712,749,800,808` — the upstream porting
  ledger, quoting upstream TypeScript behavior and this project's pre-migration
  state. Historical record; rewriting it would falsify what was ported when.
- `docs/upstream-sync/V1_5_PORTABLE_FIXES.md` (remaining hits) — this ledger's
  own account of the migration and of the earlier sweep, including the sites it
  admitted were stale. Historical by construction.

`grep -rn 'repo-root .KNOWN_DIFFS' --include='*.md' .` — no output, exit 1. The
false claim is gone and not reintroduced anywhere.

### Commands and their real exit status

Recorded in the section below, after the gates ran. Gates were run separately,
never joined with `&&`, so no tail is hidden.

## F2 round-two finding, recorded as DEFERRED: the stale-heal path deletes the pid record before the socket (2026-07-29)

**Status: NOT fixed in this release.** This section is the record, not a repair.
The defect predates this branch, so it was adjudicated out of scope for the
v1.5-portable-fixes work; recording it here keeps the evidence ledger the single
place a maintainer looking at this release finds it. `KNOWN_DIFFS.md` was
considered and rejected as the home: that file is the parsed Tier-3 oracle input
plus prose about colby EQUIVALENCE differences, and this is neither an
equivalence difference nor an allowlistable diff. No Tier-3 `RULE` line was
added; that file still has zero active rules.

### The function and the current order

`clear_stale_daemon_socket` (`crates/codegraph-daemon/src/lock.rs:165-178`),
verbatim at `26cca84`:

```rust
    // Liveness gate: only proceed once the owning pid is proven dead/absent.
    if !clear_stale_daemon_lock(&pid_path, None) {
        return false;
    }
    let _ = fs::remove_file(&socket_path);
    true
```

The pid record goes first, the socket second.

### Why the order matters

The published pid record IS the single-instance exclusion token:
`try_acquire_daemon_lock` claims it with `create_new`, so while a record exists a
competing start reports `Taken` and never binds the recorded socket path. Once
this cleaner has removed the record, a replacement daemon may legitimately claim
it and bind the same path before the `remove_file(&socket_path)` line runs — and
that unlink then destroys the LIVE replacement's socket. Observable end state: a
pid record naming a live daemon with no socket. Later clients cannot self-heal
out of it, because the liveness gate in `clear_stale_daemon_lock`
(`lock.rs:141-143`) refuses to touch a record whose pid is alive.

`clear_stale_daemon_lock` also returns `true` for a MISSING pidfile
(`lock.rs:130`), so "cleared" does not imply this caller ever held exclusion —
which widens the interleaving rather than narrowing it.

### Production reachability

Not test-only. `heal_stale_daemon_if_dead` (`crates/codegraph-cli/src/main.rs:2680`)
calls it, and that helper is reached from three sites, including the failed warm
attach inside `proxy_to_running_daemon` (`main.rs:2622-2627`, socket missing) and
its post-`run_proxy` failure path (`main.rs:2647`). Two concurrent
`serve --mcp` starts on one project can therefore run a cleaner against a
replacement daemon's namespace.

### It predates `aba4079`

`git show aba4079:crates/codegraph-daemon/src/lock.rs` already contains the same
function with the same order at its line 161:

```rust
pub fn clear_stale_daemon_socket(project_root: &Path) -> bool {
    let pid_path = daemon_pid_path(project_root);
    let socket_path = recorded_socket_path(project_root);
    // Liveness gate: only proceed once the owning pid is proven dead/absent.
    if !clear_stale_daemon_lock(&pid_path, None) {
        return false;
    }
    let _ = fs::remove_file(&socket_path);
    true
}
```

This branch changed only its error handling, because `daemon_pid_path` and
`recorded_socket_path` began returning `Result`. The deletion order is untouched.

### Contrast with `f691415`

`f691415` ("fix(daemon): remove the rendezvous socket before releasing its
ownership record") fixed exactly this class of race in a DIFFERENT function,
`cleanup_owned_rendezvous` (`lock.rs:281-312`): it removes the socket first while
its own record still excludes competitors, then deletes the record only after
re-corroborating ownership through `cleanup_owned_lock`. That function's doc
comment states the reasoning ("ORDER IS LOAD-BEARING"). The stale-heal path never
received the same treatment.

### Why a naive order swap is not a fix

Swapping the two deletions in `clear_stale_daemon_socket` is insufficient. Unlike
`cleanup_owned_rendezvous`, this cleaner does not own the record it is deleting,
so it has no ownership token to re-corroborate and nothing stops several
concurrent cleaners from performing a delayed unlink after a replacement daemon
has already bound the path. A correct fix needs either a serialization protocol
shared by starters and cleaners, or the stale cleanup moved inside
`start_or_attach_gated` (`crates/codegraph-daemon/src/lib.rs:291`) so it runs only
once the new pid record is held. Neither is a documentation change, and neither
was attempted here.

## Release blocker — the two `install_completions` / `removed_ignored_directory` failures were NOT unfixable flakes; they were one lock defect, now fixed (2026-07-29)

**This section supersedes every earlier characterisation of these two failures in
this ledger as "pre-existing", "known", "in-process `HOME`/`XDG_DATA_HOME` race",
or otherwise unfixable environment flakes** (see the notes near the F1 and F4 gate
tables above, and the run-1 note at `make ci` run 1 of the F1 batch). That
characterisation was wrong. Both failures had a determinate root cause — test code
that mutated process-global environment variables while holding mutexes that did
not exclude each other — and a determinate fix. No production code was involved in
either failure, and none was changed to fix them.

### The defect: two DIFFERENT `ENV_LOCK` statics in one crate's test code

`cargo test` runs every unit test of one binary/crate as THREADS OF A SINGLE
PROCESS, so `HOME`, `XDG_DATA_HOME`, `USERPROFILE`, `LOCALAPPDATA`,
`CODEGRAPH_NO_WATCH`, `CODEGRAPH_FORCE_WATCH` are shared mutable state. The CLI
binary's test code declared THREE independent mutexes for that one piece of shared
state (line numbers are the pre-fix `bb8097b` tree):

| Static                                  | Declared at                                     | Used by tests at                             |
| --------------------------------------- | ----------------------------------------------- | -------------------------------------------- |
| `static ENV_LOCK: Mutex<()>`            | `crates/codegraph-cli/src/main.rs:2753`         | `main.rs:2757, 2779, 2877, 2908`             |
| `static ENV_LOCK: std::sync::Mutex<()>` | `crates/codegraph-cli/src/main.rs:5704`         | `main.rs:5740, 5992, 6010, 6061, 6202, 6242` |
| `static ENV_LOCK: Mutex<()>`            | `crates/codegraph-cli/src/installer/mod.rs:503` | `installer/mod.rs:592, 630, 780, 808`        |

Distinct statics do not exclude each other AT ALL. A test in one group could set
`HOME` to its temp dir while a test in another group was mid-way through its own
`HOME` swap. Nothing in the code said "there is one env"; three separate comments
each claimed their mutex serialized "tests that mutate process-global env".

**And three env-mutating tests took NO lock at all**, including the `HOME` removal
near the old `main.rs:6078`:

- `main.rs:6117` `http_log_path_lands_under_registry_dir` — `CODEGRAPH_HTTP_REGISTRY_DIR`
- `main.rs:6147` `print_http_conflict_and_note_others_isolated` — `CODEGRAPH_HTTP_REGISTRY_DIR`
- `main.rs:6171` `is_http_detach_internal_reads_env_marker` — `CODEGRAPH_HTTP_DETACH_INTERNAL`

(The unlocked `remove_var("HOME")` the triage flagged at `main.rs:6078` is inside
`powershell_profile_path_override_then_userprofile_then_error`, which DID hold the
`5704` lock at its line 6061 — but that lock did not exclude the `2753` group, so
the effect was the same as being unlocked with respect to those four tests.)

### The escape: the suite wrote into the developer's REAL home directory

The failure is not merely a wrong assertion. Reproduced here on the untouched
`bb8097b` tree, `cargo test --locked -q -p codegraph-rs --bin codegraph`, 12
consecutive runs at default parallelism: **6 failed (exit 101), 6 passed** — runs
2, 3, 6, 9, 10, 11 red, all at the same assertion:

```
thread 'formatter_and_env_tests::install_completions_writes_zsh_fish_elvish_into_home' panicked at crates/codegraph-cli/src/main.rs:6213:9:
assertion failed: dir.join(".config/fish/completions/codegraph.fish").is_file()
```

The captured stdout of that test proves where the write actually landed:

```
Installed zsh completions to /tmp/cg-cli-fmt-install-comp-1713440-1785291188072/.zfunc/_codegraph
Installed fish completions to /tmp/codegraph-installer-run-skill-1713440-1785291188076856474/home/.config/fish/completions/codegraph.fish
```

The zsh step wrote into ITS OWN temp `HOME`; by the fish step, `HOME` had been
replaced by the `installer::tests` `run-skill` temp dir — a test holding a
DIFFERENT mutex. So `install_completions` wrote a real file into whatever `HOME`
happened to be at that instant. The same mechanism explains the earlier report of
`Installed fish completions to /config/.config/fish/completions/codegraph.fish` —
the REAL `$HOME` — when the interleaving landed between another test's restore and
the next test's set.

**Pollution outside the repository, recorded not deleted.**
`/config/.config/fish/completions/codegraph.fish` exists on this host, 34177 bytes,
mtime `2026-07-29 10:01:47 +0800`. It was created by an earlier run of this test
suite escaping its temp `HOME`. It is OUTSIDE the repository, so it was left in
place and is recorded here rather than removed.

### The sibling defect in `codegraph-watch` — same root cause, fixed together

`watcher::tests::removed_ignored_directory_does_not_schedule_a_full_sync` panicked
at `watcher.rs:1499` unwrapping the `Option` from `ProjectWatcher::start`. Causal
chain, all within one test process:

1. `policy.rs`'s tests mutate `HOME`, `CODEGRAPH_NO_WATCH`, `CODEGRAPH_FORCE_WATCH`
   under the `policy.rs:490` `ENV_LOCK`.
2. `ProjectWatcher::start` (`watcher.rs:419-423`) calls
   `watch_disabled_reason(&project_root, options.no_watch)`, which READS
   `CODEGRAPH_NO_WATCH` (`policy.rs:282`) and `HOME` (`policy.rs:390`), returning
   `Ok(None)` — no watcher — when they say "don't watch".
3. The `watcher.rs` tests took NO lock, because they never write env. But they READ
   it transitively, and `.unwrap()` on the returned `Option` turns "another test's
   `CODEGRAPH_NO_WATCH=1` was visible" into a panic.

Proof it is this and not something else: a positive control that widens the window
without changing any assertion. Inserting a 6-second sleep after
`unsafe { set_var(CODEGRAPH_NO_WATCH, "1") }` in
`policy::tests::watch_disabled_when_no_watch_env_is_set`, then running only that
test plus the watcher test:

```
cargo test --locked -q -p codegraph-watch --lib -- \
  policy::tests::watch_disabled_when_no_watch_env_is_set \
  watcher::tests::removed_ignored_directory_does_not_schedule_a_full_sync
→ watcher::tests::removed_ignored_directory_does_not_schedule_a_full_sync --- FAILED
  panicked at crates/codegraph-watch/src/watcher.rs:1499:10:
  called `Option::unwrap()` on a `None` value
  test result: FAILED. 1 passed; 1 failed
```

The sleep was a throwaway probe: `policy.rs` was restored from a pre-probe copy and
verified byte-identical to `bb8097b` (`sha256
a0cf200be0223631458d2f9fd2c7b2208f37f5a154f9eb03bd1bbc419d461274`, equal to
`git show bb8097b:crates/codegraph-watch/src/policy.rs | sha256sum`). No sleep
exists in the committed fix.

### The fix — one lock per crate, an RAII guard, and an ACTIVE escape detector

Test-only. `install_completions`, `completion_target`, `home_dir`,
`powershell_profile_path`, `watch_disabled_reason`, `ProjectWatcher::start` and
every other production path are byte-for-byte unchanged, as are the paths
completions install to.

- **One lock per test process.** `crates/codegraph-cli/src/main.rs` gains a
  `#[cfg(test)] pub(crate) mod test_env` holding THE single `ENV_LOCK` for the
  whole `codegraph` binary, reachable from `main.rs`'s five test modules AND from
  `installer::tests` (a child module of the same binary). The three former statics
  (`main.rs:2753`, `main.rs:5704`, `installer/mod.rs:503`) are deleted.
  `crates/codegraph-watch/src/lib.rs` gains the same module for that crate's test
  process; `policy.rs:490`'s static is deleted. Two processes, two locks — a lock
  cannot span processes, so per-crate is the correct granularity, and each crate's
  lock lives at the crate root where every test module can see it.
- **RAII guard.** `env_guard()` returns an `EnvGuard` that holds the mutex for its
  whole lifetime, records each variable's prior value on first touch, and restores
  every one of them **in reverse order** on `Drop` — so a failing assertion cannot
  leak a temp `HOME` into the rest of the suite. Poisoning is recovered
  (`unwrap_or_else(|e| e.into_inner())`) so one failure does not cascade; that
  removes the collateral `PoisonError` at the old `main.rs:5740` documented earlier
  in this ledger.
- **`assert_intact()` — the escape detector.** Every `set`/`remove` re-reads the
  variable and asserts it equals what this guard wrote; the completions tests also
  call it immediately BEFORE each `install_completions`. If any future test mutates
  `HOME` without holding the lock, the guarded test fails LOUDLY with
  `env var HOME changed underneath this guard: another test mutated process-global
env without holding the shared lock` instead of silently writing into the real
  home directory. This is what makes the escape impossible rather than unlikely.
- **`EnvGuard::home_key()`** centralises the `USERPROFILE`-on-Windows /
  `HOME`-elsewhere split that was duplicated at every call site. **This host is
  Linux; the Windows branch is validated by code inspection only — no native
  Windows/MSVC run was performed.**
- **Read-only tests take the lock too.** `watcher.rs`'s 17 tests that call
  `ProjectWatcher::start` / `start_serve_watcher`, and `main.rs`'s
  `db_path_is_under_codegraph_dir` (reads `CODEGRAPH_DIR`), now hold the guard even
  though they never write — reading shared state concurrently with a writer is the
  same race.

### Enumeration — every env-mutating test, before and after

Produced by a script that walks each `#[test]`, collects `set_var`/`remove_var`
calls and checks for a lock acquisition in the same body. All 28 pre-fix
env-mutating tests, plus the read-only tests brought under the lock:

| File:line (pre-fix)           | Test                                                                     | Keys mutated                                          | Before                   | After        |
| ----------------------------- | ------------------------------------------------------------------------ | ----------------------------------------------------- | ------------------------ | ------------ |
| `cli/main.rs:2755`            | `debug_enabled_honors_truthy_values_only`                                | `CODEGRAPH_DEBUG`                                     | lock A (`2753`)          | shared guard |
| `cli/main.rs:2777`            | `effective_log_level_translates_codegraph_debug_and_defers_to_rust_log`  | `CODEGRAPH_DEBUG`, `RUST_LOG`                         | lock A                   | shared guard |
| `cli/main.rs:2875`            | `daemon_services_disabled_at_home_and_root_enabled_for_nested_project`   | `HOME`/`USERPROFILE`                                  | lock A                   | shared guard |
| `cli/main.rs:2906`            | `guard_indexable_root_rejects_home_and_root_allows_nested_project`       | `HOME`/`USERPROFILE`                                  | lock A                   | shared guard |
| `cli/main.rs:5676`            | `env_path_none_for_empty_or_unset_some_for_value`                        | test-private key                                      | **none**                 | shared guard |
| `cli/main.rs:5738`            | `codegraph_dir_and_db_path_default_layout`                               | `CODEGRAPH_DIR`                                       | lock B (`5704`)          | shared guard |
| `cli/main.rs:5990`            | `home_dir_resolves_from_home_then_userprofile_then_errors`               | `HOME`, `USERPROFILE`                                 | lock B                   | shared guard |
| `cli/main.rs:6008`            | `completion_target_paths_per_shell`                                      | `HOME`, `XDG_DATA_HOME`, `LOCALAPPDATA`               | lock B                   | shared guard |
| `cli/main.rs:6059`            | `powershell_profile_path_override_then_userprofile_then_error`           | `CODEGRAPH_PS_PROFILE`, `USERPROFILE`, `HOME`         | lock B                   | shared guard |
| `cli/main.rs:6117`            | `http_log_path_lands_under_registry_dir`                                 | `CODEGRAPH_HTTP_REGISTRY_DIR`                         | **none**                 | shared guard |
| `cli/main.rs:6147`            | `print_http_conflict_and_note_others_isolated`                           | `CODEGRAPH_HTTP_REGISTRY_DIR`                         | **none**                 | shared guard |
| `cli/main.rs:6171`            | `is_http_detach_internal_reads_env_marker`                               | `CODEGRAPH_HTTP_DETACH_INTERNAL`                      | **none**                 | shared guard |
| `cli/main.rs:6200`            | `install_completions_writes_zsh_fish_elvish_into_home`                   | `HOME`, `XDG_DATA_HOME`                               | lock B                   | shared guard |
| `cli/main.rs:6240`            | `install_completions_powershell_writes_script_and_dot_sources_profile`   | `CODEGRAPH_PS_PROFILE`, `LOCALAPPDATA`                | lock B                   | shared guard |
| `cli/installer/mod.rs:589`    | `init_target_kiro_writes_project_local_mcp_with_concrete_path`           | `HOME`/`USERPROFILE`                                  | lock C (`503`)           | shared guard |
| `cli/installer/mod.rs:627`    | `init_target_none_writes_nothing`                                        | `HOME`/`USERPROFILE`                                  | lock C                   | shared guard |
| `cli/installer/mod.rs:778`    | `run_uninstall_with_ctx_via_public_paths`                                | `HOME`/`USERPROFILE`, `XDG_CONFIG_HOME`               | lock C                   | shared guard |
| `cli/installer/mod.rs:806`    | `run_skill_install_and_uninstall_and_status_via_ctx`                     | `HOME`/`USERPROFILE`                                  | lock C                   | shared guard |
| `watch/policy.rs:524`         | `watch_disabled_when_root_is_home`                                       | `HOME`, `CODEGRAPH_FORCE_WATCH`, `CODEGRAPH_NO_WATCH` | lock D (`policy.rs:490`) | shared guard |
| `watch/policy.rs:539`         | `watch_disabled_for_home_even_with_force_watch`                          | same three                                            | lock D                   | shared guard |
| `watch/policy.rs:555`         | `watch_disabled_when_root_is_filesystem_root`                            | `CODEGRAPH_FORCE_WATCH`, `CODEGRAPH_NO_WATCH`         | lock D                   | shared guard |
| `watch/policy.rs:567`         | `watch_allowed_for_normal_project_subdir`                                | same three                                            | lock D                   | shared guard |
| `watch/policy.rs:626`         | `too_broad_reason_flags_home_and_filesystem_root_but_not_nested_project` | `HOME`                                                | lock D                   | shared guard |
| `watch/policy.rs:651`         | `too_broad_reason_normalizes_trailing_dot_to_home`                       | `HOME`                                                | lock D                   | shared guard |
| `watch/policy.rs:666`         | `watch_disabled_when_root_is_home_with_trailing_dot`                     | same three                                            | lock D                   | shared guard |
| `watch/policy.rs:683`         | `watch_disabled_when_no_watch_flag_is_set`                               | `CODEGRAPH_NO_WATCH`                                  | lock D                   | shared guard |
| `watch/policy.rs:695`         | `watch_disabled_when_no_watch_env_is_set`                                | `CODEGRAPH_NO_WATCH`                                  | lock D                   | shared guard |
| `watch/policy.rs:844`         | `force_watch_re_enables_a_wsl_drive_mount_path`                          | same three                                            | lock D                   | shared guard |
| `watch/watcher.rs` (17 tests) | every test calling `ProjectWatcher::start` / `start_serve_watcher`       | reads `CODEGRAPH_NO_WATCH`, `HOME` transitively       | **none (reader)**        | shared guard |
| `cli/main.rs:5553`            | `db_path_is_under_codegraph_dir`                                         | reads `CODEGRAPH_DIR`                                 | **none (reader)**        | shared guard |

Post-fix grep confirms **zero** `set_var` / `remove_var` outside the two
`test_env` modules in these files, and exactly one `ENV_LOCK` static per crate.

Out of scope and deliberately untouched: the separate `ENV_LOCK`s in
`codegraph-core`, `codegraph-daemon`, `codegraph-mcp`, `codegraph-store` and the
integration-test binaries. Each is a DIFFERENT test process, so a lock there
cannot interact with these two; none of them mutates `HOME`.

### Negative control — the defect returns when the unification is reverted

Re-introducing the two-lock defect (adding a second `ENV_LOCK_B` static and
pointing the completions test at it, leaving everything else fixed), 4 runs at
default parallelism: **run 1 RED, runs 2-4 green**:

```
assertion `left == right` failed: env var HOME changed underneath this guard:
another test mutated process-global env without holding the shared lock
test result: FAILED. 351 passed; 2 failed
```

Two things this proves. First, the defect is genuinely the two-lock split — undo
only that and the failure returns. Second, it is PROBABILISTIC (1 red in 4), which
is precisely why every earlier single green run "proved" nothing and why the
failure was mis-filed as an environment flake for so long. `main.rs` was restored
afterwards and verified (`7146875e689cedc4…`).

Note also WHICH assertion reddened: not the old `is_file()` at `main.rs:6213`, but
`assert_intact`'s message. The detector fires at the moment the variable is stolen,
before any file is written — so even in the reverted state the suite no longer
writes outside its temp dir silently.

### Verification — every command with its REAL exit status

| Command                                                                    | Exit         | Result                                                                                           |
| -------------------------------------------------------------------------- | ------------ | ------------------------------------------------------------------------------------------------ |
| `make ci CARGO='cargo --locked'` (pre-change reproduction)                 | 0            | passed on that attempt — the flake is probabilistic, hence the loops below                       |
| `cargo test --locked -q -p codegraph-rs --bin codegraph` × 12 (pre-change) | 6× 0, 6× 101 | **defect reproduced**: runs 2,3,6,9,10,11 red at `main.rs:6213`                                  |
| `cargo test --locked -q -p codegraph-watch --lib` × 15 (pre-change)        | 15× 0        | watcher defect did not surface unaided — hence the positive control                              |
| positive control (6 s sleep probe, 2 tests only, pre-change)               | 101          | `watcher.rs:1499` `Option::unwrap()` on `None` — chain confirmed                                 |
| `policy.rs` restored from pre-probe copy                                   | 0            | sha256 equals `git show bb8097b:…/policy.rs` — probe left no trace                               |
| `cargo test --locked -q -p codegraph-rs --bin codegraph` × 8 (post-fix)    | 8× 0         | 353 passed each run                                                                              |
| `cargo test --locked -q -p codegraph-watch --lib` (post-fix)               | 0            | 100 passed                                                                                       |
| `cargo check --locked --workspace --all-targets`                           | 0            | clean                                                                                            |
| 3 parallel post-fix rounds (`cli` + `watch`)                               | 0            | `cli 353/353`, `watch 100/100`, every round green                                                |
| negative control (two-lock defect re-introduced) × 4                       | 1× 101, 3× 0 | **reddens**: `351 passed; 2 failed`, `assert_intact` message                                     |
| `lsp_diagnostics`                                                          | n/a          | REFUSED in this worktree (`LSP file path must be inside request cwd`); fell back to locked Cargo |

### Closeout — the first 5-run CI batch failed on a lint, not a test

Reported rather than hidden. The FIRST batch of five `make ci CARGO='cargo --locked'`
runs all exited **2**, every one at the `lint` target before any test ran:

```
error: items after a test module
   --> crates/codegraph-watch/src/lib.rs:18:1
    = note: `-D clippy::items-after-test-module` implied by `-D warnings`
```

`crates/codegraph-watch/src/lib.rs` had the new `#[cfg(test)] mod test_env` placed
directly after the `mod` declarations and BEFORE the crate's `pub use` re-exports,
which `clippy::items_after_test_module` forbids. Fixed by moving the module to the
END of the file, after the last `pub use`. `codegraph-cli`'s `test_env` needed no
move — `main.rs` has no trailing re-exports. Pure relocation: no logic, no test, no
production line changed.

| Command                                                             | Exit | Result                                            |
| ------------------------------------------------------------------- | ---- | ------------------------------------------------- |
| `make ci CARGO='cargo --locked'` (batch 1, runs 1-5)                | 5× 2 | FAILED at `lint` — `items_after_test_module`      |
| `cargo clippy --locked -q --workspace --all-targets -- -D warnings` | 0    | clean after the relocation                        |
| `cargo fmt --all --check`                                           | 0    | clean                                             |
| `cargo test --locked -q -p codegraph-watch --lib`                   | 0    | 100 passed                                        |
| `cargo test --locked -p codegraph-bench --test equivalence`         | 0    | **28 passed** — golden oracle unchanged           |
| `make fmt`                                                          | 0    | oxfmt, 23 docs files, run BEFORE every CI batch   |
| `bash scripts/check-workspace-versions.sh`                          | 0    | workspace / `version.txt` / manifest all `0.40.4` |
| `make ci CARGO='cargo --locked'` (batch 2, run 1)                   | 0    | `✅ All CI checks passed!`                        |
| `make ci CARGO='cargo --locked'` (batch 2, run 2)                   | 0    | `✅ All CI checks passed!`                        |
| `make ci CARGO='cargo --locked'` (batch 2, run 3)                   | 0    | `✅ All CI checks passed!`                        |
| `make ci CARGO='cargo --locked'` (batch 2, run 4)                   | 0    | `✅ All CI checks passed!`                        |
| `make ci CARGO='cargo --locked'` (batch 2, run 5)                   | 0    | `✅ All CI checks passed!`                        |

Five consecutive green runs at DEFAULT parallelism. `--test-threads=1` was never
used, and neither `Makefile` nor `.cargo/config.toml` was touched — serializing the
suite would have hidden the defect instead of fixing it. The `ETXTBSY`
(`Text file busy`) flake noted earlier in this ledger did NOT recur in any of these
runs.

**Golden byte-stability.** `git diff --stat bb8097b..HEAD -- reference/golden/` is
EMPTY; `git status --porcelain` showed no untracked
`reference/golden/*/colby.db-{wal,shm}` after the batch. `Cargo.lock` SHA-256
unchanged at `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.
Six files changed: the five test-side sources plus this ledger.

## Correction to the F2 DEFERRED record — the stale-heal rendezvous ownership race is now FIXED (2026-07-29)

**This section supersedes the status line of "F2 round-two finding, recorded as
DEFERRED: the stale-heal path deletes the pid record before the socket" above.**
That section's technical account remains accurate and is deliberately left
in place as the historical record; only its verdict changed. It said
"**Status: NOT fixed in this release.**" The user has since explicitly
authorized the repair, so the defect it describes is now fixed on this branch.
Its "out of scope because it predates the branch" adjudication no longer holds.

### The defect, re-confirmed against the pre-fix tree

`clear_stale_daemon_socket` at `4f910f2`, `crates/codegraph-daemon/src/lock.rs:165-178`:

```rust
pub fn clear_stale_daemon_socket(project_root: &Path) -> bool {
    let Ok(pid_path) = daemon_pid_path(project_root) else { return false; };
    let Ok(socket_path) = recorded_socket_path(project_root) else { return false; };
    // Liveness gate: only proceed once the owning pid is proven dead/absent.
    if !clear_stale_daemon_lock(&pid_path, None) {
        return false;
    }
    let _ = fs::remove_file(&socket_path);
    true
}
```

The published pid record IS the single-instance exclusion token:
`try_acquire_daemon_lock` (`lock.rs:83-101`) claims it with `create_new`, and a
daemon binds its socket only after that claim succeeds. `clear_stale_daemon_lock`
(`lock.rs:126-146`) DELETES that record as its side effect. Between
`lock.rs:173` and `lock.rs:176` the namespace is therefore unowned: a replacement
daemon can legitimately claim the record and bind the same recorded socket path,
and line 176's unlink then destroys the LIVE replacement's socket. End state — a
pid record naming a live daemon with no socket — is unhealable, because the
liveness gate at `lock.rs:141-143` refuses to touch a record whose pid is alive.

`clear_stale_daemon_lock` also returns `true` for a MISSING pidfile
(`lock.rs:130`), so "cleared" never implied this caller held exclusion at all.

Production-reachable: `heal_stale_daemon_if_dead`
(`crates/codegraph-cli/src/main.rs:2784-2788`) is the only caller, reached from the
socket-missing branch of `proxy_to_running_daemon` (`main.rs:2730`) and from its
post-`run_proxy` failure branch (`main.rs:2751`). Two concurrent `serve --mcp`
starts on one project can run a cleaner against a replacement daemon's namespace.

It predates the branch: `git show aba4079:crates/codegraph-daemon/src/lock.rs`
line 161 has the identical order.

### The option chosen: (b) a serialization protocol every starter and cleaner already obeys

Reviewer option **(a)** — drop proactive stale cleanup from the attach-failure path
and let `start_or_attach_gated` clear the leftover socket once it holds the new pid
record — was **rejected**, for two reasons found by reading the code rather than by
preference:

1. `start_or_attach_gated` (`crates/codegraph-daemon/src/lib.rs:291-334`) is NOT on
   the path that needs healing. The CLI's warm/cold decision
   (`serve_spawn_or_proxy`, `main.rs:2688-2718`) reaches
   `heal_stale_daemon_if_dead` on a WARM start whose attach failed, and then serves
   DIRECT; it never calls `start_or_attach_gated` in that flow. Moving the cleanup
   there would leave the stale socket in place for exactly the failing case Fix A
   was written for (the dead-daemon-with-leftover-socket hang), i.e. it would
   silently delete a shipped behavior, not relocate it.
2. `start_or_attach_gated`'s own successful path already binds through
   `bind_with_fallback` (`lib.rs:451-473`), which unlinks a stale entry at each
   candidate BEFORE binding it (`lib.rs:457-462`) — under its already-held pid
   record. Option (a)'s "new owner clears the leftover socket" is therefore already
   implemented there. The residual defect is specifically the CLEANER that owns
   nothing, so that is where the fix belongs.

Option **(b)** was taken, using the token that already exists instead of inventing
a second one: `clear_stale_daemon_socket` now BECOMES the namespace owner before it
deletes anything in the namespace.

```
1. read the RECORDED socket (the record is the only place a bind-fallback path is
   written down), then clear the stale record — still liveness-gated, so a LIVE
   owner's namespace is never entered;
2. re-claim the record via try_acquire_daemon_lock — the very same `create_new`
   token a starter uses. A LOST claim returns false and touches NOTHING;
3. unlink the stale socket while the claim excludes every starter;
4. release the claim last, re-corroborated against our own pid via
   cleanup_owned_lock.
```

This is the same idiom `f691415` established for `cleanup_owned_rendezvous`
(`lock.rs:281-312`) — mutate the namespace only while holding the record that
excludes competitors, and re-corroborate ownership at the final boundary — adapted
to a cleaner that must ACQUIRE ownership rather than already have it.
`cleanup_owned_rendezvous` and its tests were not modified.

### Why no residual window remains

The claim is a mutual-exclusion token with an atomic acquire: `create_new` on the
pid path is a single filesystem operation that exactly one of N concurrent
claimants can win. Every party that can bind the recorded socket path — a daemon
start via `start_or_attach_gated` → `try_acquire_daemon_lock` → `bind_with_fallback`
— must hold that same token first. So the unlink at step 3 executes only inside an
interval in which:

- no starter can hold the token (we do), therefore no process can have bound the
  path since we acquired it; and
- no other cleaner can hold it either (they lose at step 2 and return `false`
  having touched nothing), so the "several concurrent cleaners performing a
  delayed unlink" shape the reviewer named is eliminated rather than shrunk — a
  losing cleaner performs NO deletion at all, at any later time.

A mere order swap would not achieve this: unlink-then-clear still unlinks while
owning nothing, so a daemon that bound between the liveness read and the unlink
still loses its socket. The property that closes the window is ownership, not
order — and the order within the owned interval (socket first, record last) is what
keeps a mid-heal crash fail-closed.

Crash behavior: a crash between steps 2 and 4 leaves a record naming this
now-dead process and no socket — precisely the self-healing residue the NEXT heal
clears through its own liveness gate. It cannot leave a live daemon socketless,
because a live daemon cannot be the record's subject while we hold the claim.

Windows: the socket layer is cfg-split. On Windows the recorded "socket path" is a
bare namespaced pipe name (`crates/codegraph-daemon/src/transport.rs:36-38,59-68`),
not a filesystem entry, and `Rendezvous::cleanup_path` exists only under
`#[cfg(unix)]` (`transport.rs:70-73`), so step 3's `remove_file` simply fails and
is ignored there (a named pipe disappears with its owning process). Steps 1, 2 and
4 act on the real pid file, which is what carries the exclusion on Windows. **This
is by code inspection only — no native Windows/MSVC run was performed; the gates
below all ran on Linux.**

Callers: `clear_stale_daemon_socket` has exactly one production caller
(`heal_stale_daemon_if_dead`, `main.rs:2785`) and its signature and `bool` contract
are unchanged, so no call site needed adjusting. `git grep` confirmed the full set:
that one caller, the `codegraph-daemon` unit tests, and
`crates/codegraph-daemon/tests/stale_socket.rs`. The four pre-existing
`stale_socket.rs` integration tests (incl. `..._preserves_live_pid_lock` and
`..._removes_recorded_fallback_socket`) pass unchanged.

### Red — the interleaving, failing on the pre-fix order

`cargo test --locked -q -p codegraph-daemon --lib` with the ORIGINAL
`clear_stale_daemon_lock`-then-`remove_file` body restored (checkpoints emitted so
the new tests can run at all), exit **101**, 3 failed / 101 passed:

```
---- lock::tests::a_replacement_start_during_the_stale_heal_keeps_its_own_socket stdout ----
thread '...' panicked at crates/codegraph-daemon/src/lock.rs:800:9:
the LIVE replacement's socket must survive the stale heal

---- lock::tests::the_stale_heal_holds_its_claim_across_the_unlink_and_releases_it_last stdout ----
thread '...' panicked at crates/codegraph-daemon/src/lock.rs:838:64:
the heal claim is published

---- lock::tests::an_absent_record_is_claimed_before_its_leftover_socket_is_removed stdout ----
thread '...' panicked at crates/codegraph-daemon/src/lock.rs:924:9:
the leftover socket must still be present at the moment the claim is won, proving
the removal happens UNDER the claim and not before it
```

`a_replacement_start_during_the_stale_heal_keeps_its_own_socket` is the exact
interleaving: the cleaner passes its liveness gate, and at the
`StaleRecordCleared` checkpoint a replacement publishes a record naming a LIVE pid
(this test process) and writes its own socket bytes at the same path. On the
pre-fix code the unconditional unlink destroys it. Ordering is by the checkpoint
callback — the mutation happens inside the call — so there is **no sleep and no
timing assumption**. `StaleHealCheckpoint` mirrors the `RendezvousCleanupCheckpoint`
seam `f691415` introduced.

### Green

`cargo test --locked -q -p codegraph-daemon` — exit **0**, `104 passed` (lib) plus
every integration target green (`6, 4, 3, 4, 5, 1, 1, 2, 5, 0` passed).

### Negative controls — three mutants, each reddening a different assertion

Each was applied to the green tree, run, then reverted; after every revert
`sha256sum crates/codegraph-daemon/src/lock.rs` printed
`c8c002ff6ab847c53cd70b43f3fa75ca4e86acd3cb6061b35ce481d51ec0ec19`.

**NC1 — restore the original pid-record-then-socket order (no claim at all).**
`cargo test --locked -q -p codegraph-daemon --lib`, exit **101**, 3 failed:
the three assertions quoted under "Red" above (`lock.rs:800`, `838`, `924`).
Primary reddened test: `a_replacement_start_during_the_stale_heal_keeps_its_own_socket`.

**NC2 — release the claim BEFORE the unlink** (`cleanup_owned_lock` moved above
`remove_file`, everything else intact). `cargo test --locked -q -p codegraph-daemon
--lib`, exit **101**, 1 failed:

```
---- lock::tests::the_stale_heal_holds_its_claim_across_the_unlink_and_releases_it_last stdout ----
thread '...' panicked at crates/codegraph-daemon/src/lock.rs:876:17:
the heal claim must STILL be published while the socket is unlinked
```

A DIFFERENT test from NC1's primary, and it pins the load-bearing half of the fix:
the claim must span the unlink, not merely precede it.

**NC3 — let an ABSENT record short-circuit the claim** (absent record ⇒ unlink
immediately and return `true`, the widest form of the original defect).
`cargo test --locked -q -p codegraph-daemon --lib`, exit **101**, 1 failed:

```
---- lock::tests::an_absent_record_is_claimed_before_its_leftover_socket_is_removed stdout ----
thread '...' panicked at crates/codegraph-daemon/src/lock.rs:957:9:
the leftover socket must still be present at the moment the claim is won, proving
the removal happens UNDER the claim and not before it
```

A third distinct test. This is the deliberate decision on the absent-record case:
an absent record IS treated as an unowned namespace whose leftover socket may be
removed — `clear_stale_daemon_lock` already reports it cleared — but the removal is
gated on WINNING the claim exactly as for a dead-owner record. Refusing to heal an
absent record would strand orphaned sockets that nothing else removes (nothing
records them), while unlinking without the claim is the widest interleaving of all:
an absent record names no pid that could be proven dead, so a daemon starting
concurrently could bind the path between the two syscalls. Claiming first makes the
two cases one code path with one safety argument.

### Gates and their REAL exit status

Every command below was run separately, never joined with `&&`.

| Command                                                               | Exit | Result                                                                         |
| --------------------------------------------------------------------- | ---- | ------------------------------------------------------------------------------ |
| `bash scripts/check-workspace-versions.sh` (before every Cargo batch) | 0    | workspace / `version.txt` / manifest all `0.40.4`                              |
| `cargo test --locked -q -p codegraph-daemon --lib` (NC1 mutant)       | 101  | 3 failed — the Red interleaving                                                |
| `cargo test --locked -q -p codegraph-daemon` (green)                  | 0    | 104 lib + all integration targets passed                                       |
| `cargo test --locked -q -p codegraph-daemon --lib` (NC2 mutant)       | 101  | 1 failed — claim released before the unlink                                    |
| `cargo test --locked -q -p codegraph-daemon --lib` (NC3 mutant)       | 101  | 1 failed — absent record short-circuited the claim                             |
| `cargo test --locked -p codegraph-bench --test equivalence`           | 0    | **28 passed** — golden oracle unchanged                                        |
| `lsp_diagnostics` on `crates/codegraph-daemon/src/lock.rs`            | n/a  | REFUSED: `LSP file path must be inside request cwd`; fell back to locked Cargo |
| `make fmt`                                                            | 0    | oxfmt, run AFTER this prose and BEFORE CI                                      |
| `make ci CARGO='cargo --locked'` run 1                                | 0    | recorded below                                                                 |
| `make ci CARGO='cargo --locked'` run 2                                | 0    | recorded below                                                                 |
| `make ci CARGO='cargo --locked'` run 3                                | 0    | recorded below                                                                 |

**Golden byte-stability.** `git diff --stat 4f910f2..HEAD -- reference/golden/` is
EMPTY. The equivalence oracle left untracked
`reference/golden/{cpp,nix,terraform}/colby.db-{wal,shm}`; they are not gitignored
and were removed before staging. `Cargo.lock` SHA-256 unchanged at
`750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1`.

Two files changed: `crates/codegraph-daemon/src/lock.rs` and this ledger. No
`reference/golden/`, `Cargo.toml`, `Cargo.lock`, `.github/workflows/` or `scripts/`
change. Test parallelism untouched — `--test-threads` was never passed, and no test
was skipped, `#[ignore]`d, weakened, or given a sleep.

## PR #173 CI blockers — three failures, two root causes (2026-07-29)

Three test failures on PR #173 at HEAD `706d36a`. All three were treated as
hypotheses needing proof, and each mechanism was established from code plus a
runtime observation before anything was changed.

| #   | Job            | Failure                                                                             | Root cause                     |
| --- | -------------- | ----------------------------------------------------------------------------------- | ------------------------------ |
| A   | `Test` (LINUX) | `an_exhausted_drain_budget_replies_incomplete_instead_of_success`                   | PRODUCTION defect              |
| B   | `Windows`      | `lock::tests::a_crash_between_cleanup_boundaries_leaves_only_self_healing_residue`  | test premise absent on windows |
| C   | `Windows`      | `lock::tests::a_replacement_start_at_the_cleanup_midpoint_keeps_its_own_rendezvous` | test premise absent on windows |

### Failure A — the daemon reported a drain it never performed

CI location: `Test` job, run `30430353283`,
`panicked at crates/codegraph-daemon/tests/control_shutdown.rs:218:5`,
`3 passed; 1 failed`.

The dismissed hypothesis was "the silent client's session is not yet registered
when the shutdown frame arrives". Instrumentation refutes it. Three probes were
added temporarily to `run_accept_loop_async` — `active_count` immediately before
`close_all_sessions`, immediately after it, and at the entry to
`drain_watcher_loops_and_sessions` — and the failing runs report:

```
DBG before=1 after=0 entry=0 drained=true     <- every FAILING run
DBG before=1 after=1 entry=1 drained=false    <- every PASSING run
```

`before=1` on the failures proves the session WAS registered. The registration
race does not exist: `connect_silent_data_client` reads the daemon hello, and that
hello is written by `serve_session_async` at line 384 — AFTER
`registry.start_session()` at line 362. Completing the hello read is already
positive proof of registration. (A `handle.active_sessions() >= 1` probe inserted
into the test before the control frame never once fired across 40 runs, including
runs that then failed the drain assertion in the same binary.)

The actual defect is in production, in the shutdown path itself:

1. `registry.close_all_sessions()` signals every session to stop;
2. sessions retire on OTHER tokio worker threads;
3. `drain_watcher_loops_and_sessions` then reads `active_count()` for the FIRST
   time and returns `true` the instant it sees zero — even for
   `drain_budget: ZERO`, which permits no waiting at all.

So with a zero budget and one outstanding session, the answer was decided by
whether a worker thread got scheduled between steps 1 and 3. When it did, the
daemon answered `drained: true` for a drain it never waited for. `uninit --force`
consumes that ack as permission to delete children. This is exactly the class of
bug the test exists to prevent, and the test was right.

**Fix** (`crates/codegraph-daemon/src/lib.rs`): new `close_sessions_and_drain`
samples `watcher_done() && active_syncs() == 0 && active_count() == 0` BEFORE
signalling, and a zero budget answers from that sample. A nonzero budget is
byte-identical to before — it delegates to the unchanged
`drain_watcher_loops_and_sessions`, whose own test still passes untouched.

It is deliberately NOT an `async fn`: an `async fn` body does not execute until
first poll, which would put the sample back on the scheduler's timeline. Sampling
in a plain fn that returns the drain future pins the sample to the CALL. The new
unit test `a_zero_budget_answers_from_the_state_at_the_shutdown_instant` asserts
this by retiring the session BEFORE awaiting the returned future — the exact
interleaving that used to answer `true`.

No sleep, no budget change, no retry, no relaxed assertion. The integration test
is unmodified.

### Failure A reproduction rate, before and after

Reproduced locally, and the constraint that reproduces it is CPU count.

| Condition                              | Before fix         | After fix          |
| -------------------------------------- | ------------------ | ------------------ |
| unconstrained (30 cores), 50 runs      | **10 failed / 50** | 0 failed / 50      |
| unconstrained, second 60-run batch     | 0 failed / 60      | —                  |
| `taskset -c 0,1` (2 cores), 30 runs    | 1 failed / 30      | —                  |
| `taskset -c 0,1` + 8 spinners, 50 runs | 3 failed / 50      | —                  |
| `taskset -c 0` (1 core), 40 runs       | **5 failed / 40**  | **0 failed / 40**  |
| `taskset -c 0` (1 core), 50 runs       | —                  | **50 passed / 50** |

The 10/50 first batch and the 0/60 second on the same binary is itself the
signature: the outcome tracks machine load, not the code path. `taskset -c 0`
reproduces it reliably at ~12%, which is why the fix was verified there and not
only on an idle 30-core host.

### Per-test audit of `control_shutdown.rs`

Every test was checked for the "assume the server already did something" shape.

| Test                                                                    | Precondition it needs                                       | Established?                                                                                                    |
| ----------------------------------------------------------------------- | ----------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `authorized_shutdown_closes_a_long_lived_session_before_acknowledging`  | session registered before the frame                         | YES — the hello read (written after `start_session`) proves it                                                  |
| `an_unauthorized_frame_is_refused_without_shutting_the_daemon_down`     | daemon serving; each refusal answered on its own connection | YES — each `send_control_frame` completes a hello + reply round trip; the `is_finished` check follows the reply |
| `an_exhausted_drain_budget_replies_incomplete_instead_of_success`       | session registered before the frame                         | YES — same hello proof; the failure was NOT a missing precondition                                              |
| `an_incomplete_drain_reply_is_unresponsive_and_never_signals_the_owner` | stub listener bound before the request                      | YES — `ListenerOptions::create_sync()` returns after bind, on the test thread, before the thread is spawned     |

`3 passed` was not luck. No test in this file waits on an unobserved server
action, and none was changed.

### Failures B and C — VERDICT: test premise, not a production defect

CI location: `Windows` job, step `Run tests`,
`panicked at crates\codegraph-daemon\src\lock.rs:761:17` and
`crates\codegraph-daemon\src\lock.rs:714:9`, `93 passed; 2 failed`.

The recorded hypothesis is confirmed by the code:

- `paths::daemon_socket_path` under `#[cfg(windows)]` returns
  `PathBuf::from(format!("{V2_RENDEZVOUS_PREFIX}{}", rendezvous_name(&paths)))` —
  a BARE name like `codegraph-v2-1a2b3c4d5e6f7788`, no directory, no filesystem
  entry (interprocess's `GenericNamespaced` prepends `\\.\pipe\` itself).
- `Rendezvous::cleanup_path` exists only under `#[cfg(unix)]`, and
  `cleanup_owned_rendezvous` unlinks only under `#[cfg(unix)]` (`lock.rs:390-396`).

Both failing tests did `fs::write(&socket, b"old")` — creating a REAL file whose
name is the pipe name, in the process CWD — and then asserted
`!socket.exists()`. On windows nothing removes it, so the assertion fails. It is
asserting a filesystem contract windows does not have.

Production is CORRECT on windows, and deliberately so. A named pipe is a kernel
object reclaimed when its last handle closes; there is no residue to unlink, and
attempting one would be meaningless. The pid record — a real file — is what carries
the single-instance exclusion, and `cleanup_owned_rendezvous` /
`clear_stale_daemon_socket` operate on it identically on both platforms. Gating a
test green here does not hide a broken production path; there is no windows path
to break.

**Fix** (`crates/codegraph-daemon/src/lock.rs`, test module only): three helpers
— `publish_socket_file`, `assert_socket_file_present`, `assert_socket_file_absent`
— perform the socket-FILE operations under `#[cfg(unix)]` and are no-ops
elsewhere. The two tests keep their names, their structure, their checkpoint
callbacks, and every ORDERING assertion on all platforms:

- the pid record is STILL PUBLISHED at the `SocketRemoved` boundary (`f691415`'s
  ordering invariant) — asserted on windows too;
- a competing `try_acquire_daemon_lock` at `OwnershipCorroborated` is REFUSED and
  still reads the departing owner's pid — asserted on windows too;
- `cleanup_owned_rendezvous` returns `false` for a departed owner and `true` for
  the true owner — asserted on windows too;
- `clear_stale_daemon_socket` clears the crash residue — asserted on windows too.

Only the `socket.exists()` / `fs::write(socket)` claims are unix-scoped. On unix
the tests are byte-equivalent in behavior to before.

**Windows coverage now unverified (known gap).** That the socket FILE is removed,
and that it is removed BEFORE the pid record, is asserted on unix only. This is
not a loss of real coverage — there is no socket file on windows — but it is
recorded so no one later reads the gate as "windows cleanup is tested". The three
stale-heal tests (`a_replacement_start_during_the_stale_heal_keeps_its_own_socket`,
`the_stale_heal_holds_its_claim_across_the_unlink_and_releases_it_last`,
`an_absent_record_is_claimed_before_its_leftover_socket_is_removed`) were NOT
gated: they were not in the windows failure list, and `clear_stale_daemon_socket`
calls `fs::remove_file(&socket_path)` unconditionally (`lock.rs:263`, no `cfg`), so
a file they themselves created IS removed on windows. They are left exactly as
`6b9861b` wrote them.

### Windows was NOT validated locally — and cannot be here

No windows verification is claimed. Both cross-compilation routes were retried and
both fail, as previously recorded: `cargo clippy --target x86_64-pc-windows-msvc`
exits 101 (23 `tree-sitter-*` crates fail their `cc-rs` build scripts), and
`cargo xwin clippy` exits 101 (`failed to find tool "clang-cl"`). No clang /
lld-link / zig / cargo-zigbuild is present, no package manager is available, and
the process runs as uid 1000. `clang-cl` cannot be installed here. The B/C verdict
therefore rests on reading `paths.rs`, `transport.rs` and `lock.rs`, not on a
windows run. Real windows validation is the CodeBuild `codegraph-rs-windows`
project, which was NOT touched.

### Gates and their REAL exit status

Each command was run separately; none joined with `&&`.

| Command                                                                               | Exit      | Result                                                   |
| ------------------------------------------------------------------------------------- | --------- | -------------------------------------------------------- |
| `cargo test --locked -p codegraph-daemon` (before any fix, drain test alone, 50 runs) | 101 on 10 | the Red observation                                      |
| `cargo build --locked -p codegraph-daemon --tests` (instrumented)                     | 0         | probe binary for the diagnosis                           |
| `cargo clippy --locked -p codegraph-daemon --all-targets`                             | 0         | no warnings                                              |
| `cargo test --locked -p codegraph-daemon --lib` (mid-fix, `async fn` form)            | 101       | 1 failed — proved the sample must precede the first poll |
| `cargo test --locked -p codegraph-daemon`                                             | 0         | 105 lib + every integration target, 11 targets all `ok`  |
| drain-budget test × 50, `taskset -c 0`                                                | 0         | **50 passed / 50**                                       |
| drain-budget test × 50, unconstrained                                                 | 0         | **50 passed / 50**                                       |
| the other three `control_shutdown` tests × 25 each, `taskset -c 0`                    | 0         | 0 failed / 75                                            |

The remaining gates (`bash scripts/check-workspace-versions.sh`, `make fmt`,
`make ci CARGO='cargo --locked'` twice, the golden-diff and `Cargo.lock` checks)
are recorded immediately below with their real exit statuses.

### PR #173 blockers — final gate statuses

| Command                                              | Exit  | Result                                                                         |
| ---------------------------------------------------- | ----- | ------------------------------------------------------------------------------ |
| `bash scripts/check-workspace-versions.sh`           | **0** | workspace / `version.txt` / manifest all `0.40.4`                              |
| `make fmt`                                           | **0** | run AFTER this prose, BEFORE CI                                                |
| `make ci CARGO='cargo --locked'` run 1               | **0** | `✅ All CI checks passed!`                                                     |
| `make ci CARGO='cargo --locked'` run 2               | **0** | `✅ All CI checks passed!`                                                     |
| `git diff --stat 706d36a..HEAD -- reference/golden/` | **0** | EMPTY — no golden byte changed                                                 |
| `sha256sum Cargo.lock`                               | **0** | `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` — unchanged |

The equivalence oracle left untracked
`reference/golden/{cfml,cpp,solidity}/colby.db-{wal,shm}` after run 1; they are not
gitignored and were removed before run 2 and before staging. Three files changed:
`crates/codegraph-daemon/src/lib.rs`, `crates/codegraph-daemon/src/lock.rs`, and
this ledger. No `reference/golden/`, `Cargo.toml`, `Cargo.lock`,
`.github/workflows/`, `scripts/` or `.aws/` change. No test was skipped,
`#[ignore]`d, weakened, given a sleep, or given a widened timeout.

## PR #173 CI round 4 — the four `codegraph-mcp` windows test failures

Run `30444938158`, job `Windows`, step `Run tests` (`cargo test --workspace
--exclude codegraph-bench`). Its `codegraph-daemon` suite now passes `96/96`, so
`a131aee` and `eb4b516` held. The next layer surfaced in `codegraph-mcp --lib`:
`254 passed; 4 failed`, `error: test failed, to rerun pass -p codegraph-mcp --lib`.

| CI location                                        | Test                                                                                           |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `crates\codegraph-mcp\src\engine.rs:4044:78`       | `engine::tests::ext_open_reads_store_from_project_dir`                                         |
| `crates\codegraph-mcp\src\rmcp_handler.rs:1142:78` | `rmcp_handler::handler_tests::call_tool_bad_project_path_returns_error_result`                 |
| `crates\codegraph-mcp\src\rmcp_handler.rs:1142:78` | `rmcp_handler::handler_tests::call_tool_timeout_disabled_runs_unbounded_path`                  |
| `crates\codegraph-mcp\src\rmcp_handler.rs:1142:78` | `rmcp_handler::handler_tests::execute_owned_success_opens_request_scoped_engine_and_runs_tool` |

### What is unwrapped at each location

Both are the SAME call, and it is neither a path nor a `Store`:

- `engine.rs:4044` — inside `ext_open_reads_store_from_project_dir`, directly:
  `codegraph_store::test_support::finalize_current_test_fixture(&paths).unwrap()`.
- `rmcp_handler.rs:1142` — the LAST statement of the shared test helper
  `real_indexed_project(tag: &str) -> TempDir`, which copies
  `reference/golden/mini/colby.db` plus the two `fixtures/mini` sources into a
  temp project and then calls the identical
  `finalize_current_test_fixture(&paths).unwrap()`.

`real_indexed_project` is the shared helper behind three of the four failures;
`ext_open_reads_store_from_project_dir` calls the same store API inline. So all
four are ONE cause, and the failing code is in **production**
(`codegraph-store`), not in the tests.

### Root cause — `FlushFileBuffers` on a DIRECTORY handle returns ERROR_ACCESS_DENIED

The full CI error, recovered from the run log, is identical in all four:

```
called `Result::unwrap()` on an `Err` value: publish test fixture Building state
Caused by:
    0: cannot synchronize state-slot parent directory at \\?\C:\Users\runneradmin\AppData\Local\Temp\cg-mcp-open-8848-53\.codegraph-v2: Access is denied. (os error 5)
    1: Access is denied. (os error 5)
```

with backtrace frames `codegraph_store::test_support::finalize_current_test_fixture`
(`test_support.rs:63`, the `publish_index_state(..., StatePhase::Building)` line)
→ `anyhow::context` → `codegraph_store::index_state_publisher::StatePublishError`.

`publish_index_state` ends with a parent-directory durability barrier,
`sync_parent` (`index_state_publisher.rs:480-495`): open the state-slot parent
directory (`FILE_FLAG_BACKUP_SEMANTICS`, read-only access) and call
`File::sync_all()` on it. `sync_all` is `FlushFileBuffers(handle)`.

The behavioural difference: on unix `fsync(dirfd)` on a read-only directory
descriptor is the documented way to make a rename durable and returns `0`. On
windows `FlushFileBuffers` requires a handle carrying **write** access
(`GENERIC_WRITE`/`FILE_WRITE_DATA`); a DIRECTORY handle obtained by an
unprivileged process never carries it, so the call returns
`ERROR_ACCESS_DENIED` (5). It is a property of the platform's directory API — it
happens on every call on every windows machine, not on some machines or some
paths. Note the temp path is a `\\?\` verbatim path, which is a red herring: the
`open` succeeded, only the flush failed.

`directory_sync_unsupported` downgraded only `1 | 6 | 50 | 87`
(ERROR_INVALID_FUNCTION / INVALID_HANDLE / NOT_SUPPORTED / INVALID_PARAMETER) to
`ParentSyncStatus::Unsupported`, with a comment stating "Access errors are NOT
downgraded to unsupported". That policy is right for a FILE and wrong for a
DIRECTORY: it turned the one windows-universal outcome into a hard
`StatePublishError::Io`, so **every** `publish_index_state` call fails on windows.
This is not a test defect. Production `codegraph-store` cannot publish index
state on windows at all: `rebuild.rs:317`/`:496`/`:885` and `uninit.rs:188` take
the same path, so `codegraph index`, its `Building -> Current` promotion, and
`codegraph uninit` were all broken on windows. The four tests were the messengers.

### The fix — one production change

`ERROR_ACCESS_DENIED` (5) joins the windows "no directory flush primitive" set.
The justification is narrow and is written into the code: this branch is reached
only AFTER the directory open already succeeded, so the denial cannot mean the
namespace is unwritable, and the writes the barrier protects (temp write, temp
`sync_all` on a real FILE handle, and the rename) have all already completed and
are unaffected. Access errors from OPENING the directory remain hard failures.
Losing the barrier means a crash within the window after the rename could lose
the slot rename on power loss — exactly the risk `ParentSyncStatus::Unsupported`
already exists to report, and strictly better than the previous behaviour of
never publishing at all.

Two mechanical changes make it testable from linux: the per-platform code sets
became named constants (`UNIX_DIRECTORY_SYNC_UNSUPPORTED`,
`WINDOWS_DIRECTORY_SYNC_UNSUPPORTED`, both `cfg(any(<platform>, test))`), and the
matching moved into a pure `directory_sync_unsupported_in(&io::Error, &[i32])`.
`directory_sync_unsupported` is now a thin per-platform wrapper choosing its set.
No behaviour changed on unix: the set is still `22 | 95`.

Three unit tests were added, all runnable on linux:
`windows_directory_flush_denial_is_classified_unsupported_not_a_hard_failure`
(asserts win32 `5` classifies as unsupported, and every code in each platform
set does), `directory_flush_codes_outside_the_platform_set_stay_hard_failures`
(win32 `2`/`112` stay failures; the unix set does not accept `5` or `1`, so the
sets cannot leak into each other), and
`publication_reports_a_parent_sync_status_instead_of_failing`.

**Negative control**: removing `5` from `WINDOWS_DIRECTORY_SYNC_UNSUPPORTED` and
re-running reddens exactly the intended test —
`assertion failed: directory_sync_unsupported_in(&io::Error::from_raw_os_error(5), WINDOWS_DIRECTORY_SYNC_UNSUPPORTED)`,
`test result: FAILED. 5 passed; 1 failed`. The constant was restored and the file
verified byte-identical afterwards.

### No test was gated, and no coverage was lost

All four failures were production defects, so nothing was `#[cfg]`-gated,
`#[ignore]`d, weakened or slept. `crates/codegraph-mcp` is unchanged by this fix;
so are all four test bodies.

### Workspace-wide audit — the same defect class elsewhere

Class: POSIX filesystem-durability / directory-handle / open-file semantics that
differ on windows. Searched all of `crates/` for directory-handle `sync_all`,
hard-coded `raw_os_error()` sets, `rename` over an existing or open destination,
`remove_*` on an open handle, `/`-separator and non-canonicalised path
assumptions, and exact `ErrorKind`/OS-message assertions.

| file:line                                                                                        | assumes                                                                                            | fails on windows?        | reasoning                                                                                                                                                                                                                                                                     |
| ------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------- | ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/codegraph-store/src/index_state_publisher.rs:490,519`                                    | a directory handle can `sync_all`, and only `1/6/50/87` mean unsupported                           | **yes — FIXED here**     | `FlushFileBuffers` on a directory returns `5`; set was missing it                                                                                                                                                                                                             |
| `crates/codegraph-store/src/index_state_publisher.rs:473,511`                                    | unix directory `fsync`, unsupported errnos `22/95`                                                 | no                       | correct on the supported unix targets; `cfg(unix)`-only                                                                                                                                                                                                                       |
| `crates/codegraph-watch/src/watcher.rs:81`                                                       | errno `23/24/28` classify watcher exhaustion                                                       | no                       | explicitly `cfg(unix)`; windows falls through to `Other`, a conservative downgrade                                                                                                                                                                                            |
| `crates/codegraph-daemon/src/lock.rs:89`                                                         | `rename(tmp, pid_path)` over a just-`create_new`ed placeholder whose handle is still open in scope | no                       | rust's `fs::rename` uses `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, and std opens files with `FILE_SHARE_DELETE`, so replacing an open target is permitted; CI corroborates — the whole `codegraph-daemon` suite passes `96/96` on windows and this is its normal acquire path |
| `crates/codegraph-daemon/src/http_registry.rs:151`                                               | `rename(tmp, path)` overwrites an existing registry entry                                          | no                       | same `MOVEFILE_REPLACE_EXISTING` reasoning; no open handle on the target at all                                                                                                                                                                                               |
| `crates/codegraph-cli/src/installer/shared.rs:376`                                               | `rename(tmp, path)` overwrites an existing agent config                                            | no                       | same; and windows installer tests pass in CI                                                                                                                                                                                                                                  |
| `crates/codegraph-store/src/index_lease.rs:632,680`                                              | a locked, still-open lock file can be `rename`d away mid-acquisition (fault-injection tests)       | no                       | `MOVEFILE_REPLACE_EXISTING` + `FILE_SHARE_DELETE`; and these unit tests are in `codegraph-store --lib`, which passed on windows in this very run                                                                                                                              |
| `crates/codegraph-store/tests/index_state_publisher.rs:385,415,439`                              | same, in integration tests                                                                         | uncertain — not observed | the `codegraph-store` integration targets did NOT run on windows in run `30444938158`: the job aborted at the first failing target (`codegraph-mcp --lib`), and `Running` lines for them never appear in the log. Unverified either way; NOT changed                          |
| `crates/codegraph-store/tests/store_state_gates.rs:1051,1109`                                    | rename a retained-lease lock file                                                                  | uncertain — not observed | same: target never ran on windows                                                                                                                                                                                                                                             |
| `crates/codegraph-cli/tests/batch_m_legacy_extension_override.rs:1625,1728`                      | rename over an open snapshot-read handle / move an open directory                                  | uncertain — not observed | `codegraph-cli` integration targets also never ran on windows in this run                                                                                                                                                                                                     |
| `crates/codegraph-mcp/tests/golden_mcp.rs:51`, `crates/codegraph-mcp/tests/support/parity.rs:44` | transient windows file locking appears ONLY as raw os error `32` (`ERROR_SHARING_VIOLATION`)       | uncertain                | `32` is the dominant case, but AV/filter-driver interference can also surface as `5` or `33`. Widening it would be speculative and would weaken a retry predicate against evidence I do not have, so it is left alone and recorded                                            |

Fixed: the one high-confidence entry, which is also the one CI actually proved.
Not fixed, with reasoning: the `rename`-over-open-handle entries — the three
"no" verdicts rest on rust's `MOVEFILE_REPLACE_EXISTING` + `FILE_SHARE_DELETE`
implementation (read from `library/std/src/sys/fs/windows.rs`) plus positive CI
evidence, and the "uncertain" ones sit in targets windows never reached because
the job aborted earlier. Changing them now would be guessing. **Expect further
layers**: once `codegraph-mcp --lib` is green, windows will run
`codegraph-store`'s and `codegraph-cli`'s integration targets for the first time,
and those uncertain rows are where the next failures would appear.

### Gates and their REAL exit status

Each run separately; none joined with `&&`.

| Command                                                                         | Exit | Result                                                   |
| ------------------------------------------------------------------------------- | ---- | -------------------------------------------------------- |
| `gh run view 30444938158 --log` (recover the real windows message)              | 0    | all four `Caused by: ... Access is denied. (os error 5)` |
| `cargo test --locked -p codegraph-store --lib index_state_publisher`            | 0    | 6 passed (3 new)                                         |
| negative control: same, with `5` removed from the windows set                   | 101  | **1 failed** — the intended test only                    |
| `cargo test --locked -p codegraph-store --lib index_state_publisher` (restored) | 0    | 6 passed                                                 |
| `cargo test --locked -p codegraph-mcp`                                          | 0    | **258** lib + 65 across 9 integration targets, all `ok`  |
| `cargo test --locked -p codegraph-store`                                        | 0    | **100** lib + 91 across 8 integration targets, all `ok`  |

The remaining gates (`check-workspace-versions.sh`, `make fmt`, `make ci` twice,
golden diff, `Cargo.lock` SHA) are recorded immediately below.

### CI round 4 — final gate statuses

| Command                                              | Exit  | Result                                                                         |
| ---------------------------------------------------- | ----- | ------------------------------------------------------------------------------ |
| `bash scripts/check-workspace-versions.sh`           | **0** | workspace / `version.txt` / manifest all `0.40.4`                              |
| `make fmt`                                           | **0** | run AFTER this prose, BEFORE CI                                                |
| `make ci CARGO='cargo --locked'` run 1               | **0** | `✅ All CI checks passed!`                                                     |
| `make ci CARGO='cargo --locked'` run 2               | **0** | `✅ All CI checks passed!`                                                     |
| `git diff --stat c7800ca..HEAD -- reference/golden/` | **0** | EMPTY — no golden byte changed                                                 |
| `sha256sum Cargo.lock`                               | **0** | `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` — unchanged |

The equivalence oracle again left untracked
`reference/golden/{cuda,solidity}/colby.db-{wal,shm}`; removed before run 2 and
before staging. Two files changed:
`crates/codegraph-store/src/index_state_publisher.rs` and this ledger. Windows was
NOT validated locally — the two cross-compilation routes still fail exactly as
recorded above and no C toolchain can be installed here.

## PR #173 CI round 5 — the legacy fixture path crossed a shell boundary it could not survive (2026-07-29)

Sixth consecutive windows-only layer, and the first one that is neither a lint
nor a code defect: it is a CI-wiring defect between two processes that disagree
about what an absolute path means.

### The failure, with its exact CI location

Run `30449004902`, job `Windows`, step `Run tests`:

```
test legacy_scan_with_runtime_extension_override_does_not_weaken_storage_safety ... FAILED
panicked at crates\codegraph-cli\tests\batch_m_legacy_extension_override.rs:330:13:
CODEGRAPH_LEGACY_BIN failed frozen fixture verification: inspect configured legacy binary
/tmp/codegraph-legacy-fixture/e52703f3a3d5bef90997ce23d9a3b49c980e6bcc1a078fdb1245ad1305a5bc09/codegraph.exe:
The system cannot find the path specified. (os error 3)
test result: FAILED. 19 passed; 1 failed
```

Every other test in that binary passed. That is the shape of the bug: the fixture
was downloaded, digest-verified and version-verified SUCCESSFULLY by the setup
step — the failing verification is the SECOND one, performed by the test process
against the path the setup step handed it.

### The mechanism — a path-domain mismatch, confirmed

The orchestrator's trace is correct. Confirmed against the real files:

1. `.github/workflows/ci.yml:101-103` — the `windows` job's fixture step is the
   only step in that job with `shell: bash`, and it runs
   `echo "CODEGRAPH_LEGACY_BIN=$(bash scripts/setup-legacy-fixture.sh)" >> "$GITHUB_ENV"`.
   On `windows-latest`, `shell: bash` is **Git Bash** — an MSYS2 runtime.
2. `scripts/setup-legacy-fixture.sh:57` computes
   `CACHE_ROOT="${CODEGRAPH_LEGACY_FIXTURE_CACHE:-${TMPDIR:-/tmp}/codegraph-legacy-fixture}"`.
   `CODEGRAPH_LEGACY_FIXTURE_CACHE` is not set by CI, and `TMPDIR` is not set in
   Git Bash on the runner, so `CACHE_ROOT` becomes the literal string
   `/tmp/codegraph-legacy-fixture`.
3. `/tmp` under MSYS is a **virtual mount**, not a filesystem path. The MSYS
   runtime maps it to something like
   `C:\Users\runneradmin\AppData\Local\Temp`. Every filesystem call the SCRIPT
   makes goes through that runtime, so the download, the `mkdir -p`, the digest
   check and `"$EXE" --version` all succeed — the script's own view is coherent.
4. The script's final line printed that MSYS path verbatim, and the workflow put
   it in `$GITHUB_ENV`. `$GITHUB_ENV` is a plain text file; nothing translates it.
5. `cargo test` is a **native Win32 process**. It reads the variable as an opaque
   string and resolves it through the Win32 API, which has no `/tmp` mount and no
   MSYS runtime. `\tmp\codegraph-legacy-fixture\...` does not exist relative to
   any drive → `ERROR_PATH_NOT_FOUND`, surfaced by Rust as `os error 3`, "The
   system cannot find the path specified".

So the file EXISTS and is the correct, digest-verified v0.40.4 binary. Two
processes simply disagree about the namespace the string belongs to. The
error message even proves the file was fine when the script saw it: the script's
own `--version` check ran against it and passed, or the script would have exited
nonzero and the workflow step would have failed instead.

Why this is a fixture-WIRING defect and not a fixture-CONTENT defect: nothing
about the frozen binary, the manifest, the digests, or the test's property
changed. The property under test — an unmodified published v0.40.4 scanner
cannot weaken v2 storage safety — is intact and must keep being proven on
Windows.

### The fix — option (a), the emitter owns its output contract

`scripts/setup-legacy-fixture.sh` now translates the path it PRINTS into the
consumer's domain, via a single new `emit_consumer_path` used by both exit routes
(the revalidated-cache-hit route at what is now line 354 and the
fresh-download route at the end).

On a Windows shell (`uname -s` matching `MINGW*`/`MSYS*`/`CYGWIN*`/`Windows_NT`)
it runs `cygpath -m -- "$path"`. Everywhere else it prints the path byte-for-byte
and never invokes `cygpath` — which matters, because `cygpath` does not exist on
Linux or macOS.

Three deliberate choices:

- **`-m`, not `-w`.** Both give a native absolute path; `-m` uses forward slashes
  (`C:/Users/...`) and `-w` uses backslashes (`C:\Users\...`). Win32 accepts
  both, and Rust's `Path` handles both. Forward slashes are chosen because the
  string travels through `$GITHUB_ENV` and then through Rust string handling, and
  a backslash path invites an escaping bug in any future consumer that does not
  treat it as opaque. Forward slashes have no such failure mode.
- **A missing or failing `cygpath` is FATAL, not a silent passthrough.** On a
  Windows host, printing the untranslated path is exactly the bug being fixed;
  degrading to it would reintroduce a failure 40 lines later with a much worse
  error message. `cygpath` ships with Git Bash and with Cygwin, so its absence on
  a Windows host is a broken environment, and a fixture that cannot be handed to
  its consumer is a setup FAILURE — the same fail-closed stance the rest of this
  script takes.
- **Only the OUTPUT is translated.** `$EXE` keeps its shell-native form for every
  internal use (`sha256_of`, `chmod`, `"$EXE" --version`, `rm -f`), because those
  all run through the MSYS runtime and would BREAK if handed a `C:/...` path in
  some contexts. The translation happens at the process boundary, which is where
  the domain actually changes.

Why not the alternatives:

- **(b) convert in the workflow.** Works, but puts the knowledge in the caller.
  The script has two callers already — the `test` job, the `windows` job — and a
  third that is not a workflow at all (`legacy_bin()` in
  `batch_m_legacy_extension_override.rs:335` shells out to the script directly
  when `CODEGRAPH_LEGACY_BIN` is unset). That third caller is the decisive
  argument: on a Windows developer machine running `cargo test` outside CI, the
  fallback path spawns `bash scripts/setup-legacy-fixture.sh` from a NATIVE
  process and parses its stdout. Fixing only the workflow leaves that route
  broken. Fixing the script fixes every consumer at once.
- **(c) make the test tolerate MSYS paths.** Rejected. It would teach production
  test code about a CI shell's private namespace, and it would leave the same
  trap armed for every other consumer of this script — including the local
  Windows developer route above.

### Cross-domain audit — every bash→native boundary in CI

Enumerated mechanically (`yaml.safe_load` over both workflow files, listing each
`run` step with its effective shell and its job's `runs-on`, including the
release matrix's per-`os` entries). Neither workflow sets a workflow-level or
job-level `defaults.shell`, so a step with no `shell:` key uses the runner
default: `bash` on ubuntu/macos, **PowerShell** on windows.

| location                                                                             | emits                            | consumed by                                 | domains match?                                                       | verdict                                                                                                        |
| ------------------------------------------------------------------------------------ | -------------------------------- | ------------------------------------------- | -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `ci.yml:103` windows fixture step (`shell: bash`)                                    | fixture exe path → `$GITHUB_ENV` | native `cargo test` (2 later steps)         | **NO — MSYS vs Win32**                                               | **the bug; FIXED** in the script                                                                               |
| `ci.yml:48` linux fixture step (default bash)                                        | fixture exe path → `$GITHUB_ENV` | native `cargo test`                         | yes — one POSIX domain on ubuntu                                     | correct; output byte-identical after the fix                                                                   |
| `batch_m_legacy_extension_override.rs:335` fallback                                  | script stdout → `PathBuf`        | the test process itself (native on windows) | **would NOT have matched on windows**                                | **also FIXED** by the same change — the reason the fix belongs in the script                                   |
| `ci.yml` windows `Clippy lint`, `Run tests`, `Run Batch M item 16`                   | nothing; no paths emitted        | —                                           | n/a — PowerShell steps, cargo-relative                               | no boundary                                                                                                    |
| `release-please.yml:255-260` `Package (unix tar.gz)` (`shell: bash`)                 | writes `dist/*.tar.gz`           | `upload-artifact` (`path: dist/*`)          | yes — gated `if: matrix.archive == 'tar.gz'`, i.e. ubuntu/macos only | no windows involvement                                                                                         |
| `release-please.yml:263-269` `Package (windows zip)` (`shell: pwsh`)                 | writes `dist/*.zip`              | `upload-artifact`                           | yes — pwsh is native; repo-relative paths                            | correct by construction                                                                                        |
| `release-please.yml:311-327` `Generate SHA256SUMS` (`shell: bash`)                   | `SHA256SUMS` in `dist/`          | `action-gh-release`                         | yes — `runs-on: ubuntu-latest`                                       | no windows shell in this job                                                                                   |
| `release-please.yml:122` `sha=$(git rev-list ...)` → `$GITHUB_OUTPUT`                | a commit SHA, not a path         | later `run` in same ubuntu job              | n/a — not a path                                                     | no boundary                                                                                                    |
| `release-please.yml:68-74` `build_artifacts`/`tag_name`/`version` → `$GITHUB_OUTPUT` | non-path scalars                 | matrix jobs incl. windows                   | n/a — not paths                                                      | no boundary                                                                                                    |
| `ci.yml:236` `mapfile`/`jq` gate body                                                | job names, not paths             | same step                                   | n/a                                                                  | no boundary                                                                                                    |
| `scripts/check-workspace-versions.sh:94-106` `/tmp/cwv_meta_err.$$`                  | a POSIX temp file                | itself only                                 | yes — same shell reads and deletes it                                | safe; and this gate runs only in `make ci`/pre-push/`test` job, never on windows                               |
| `scripts/install.sh:116` `mktemp -d`                                                 | temp dir                         | itself only                                 | yes                                                                  | POSIX installer; the Windows installer is `install.ps1`, which uses `[System.IO.Path]::GetTempPath()` natively |
| `scripts/tests/*.test.sh`                                                            | fixture roots                    | themselves                                  | yes                                                                  | never invoked by any workflow; local POSIX harnesses                                                           |

Result: **exactly one** bash→native path boundary existed, plus the same defect
on the script's non-CI fallback route. Both are closed by the one change. No
other emitter in either workflow hands a path across a shell-domain boundary; the
remaining `/tmp` uses are single-domain (a shell writing and reading its own temp
file) or POSIX-only by construction.

`GITHUB_PATH` is never written by either workflow, so there is no PATH-domain
variant of this bug.

Not fixed, with reasoning: nothing. Unlike round 4, this audit found no
"uncertain" rows — a step's shell and its runner OS are both statically
declared, so every boundary is decidable from the files without needing a CI
observation.

### Regression proof — the Windows branch is tested on Linux

The translation only executes on Windows, which no local run can reach. That is
precisely the code that rots. `scripts/tests/legacy-fixture-path-domain.test.sh`
therefore extracts `emit_consumer_path` **verbatim** from the shipped script
(never re-implementing it, so the harness cannot drift) and drives it with a
stubbed `uname` and a stubbed `cygpath`, under the same `set -euo pipefail` and
the same `die` shape as the real script.

Ten assertions: Linux and Darwin pass the path through byte-for-byte and never
consult `cygpath`; all four Windows `uname` spellings translate; `cygpath` is
asserted to be invoked with `-m` and the result asserted to contain no
backslash; a missing `cygpath`, a nonzero `cygpath`, and an empty `cygpath`
result each fail loudly and emit NOTHING on stdout; and a static check asserts
the shipped script has zero remaining raw `$EXE` emissions and exactly two
translated ones, so regressing only ONE of the two exit routes — the way this
class of bug hides — is caught.

Two negative controls were run:

- restoring `b71a5f1`'s pre-fix script → the harness exits **2** and reports it
  cannot extract `emit_consumer_path`, i.e. the harness genuinely depends on the
  fix existing;
- keeping the translator but reverting ONLY the cache-hit emission to
  `printf '%s\n' "$EXE"` → **9 passed, 2 failed**, both failures on the static
  route check, exit **1**. The nine behavioral scenarios still pass, which is
  exactly why the static check is there.

### Linux/macOS output is unchanged — proven, not asserted

The script was run before and after the change on this host, capturing stdout and
stderr separately:

| check                              | result                                                                                                   |
| ---------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `uname -s` / `TMPDIR` on this host | `Linux` / **unset** (so `CACHE_ROOT` is `/tmp/codegraph-legacy-fixture`, same as the CI linux job)       |
| `command -v cygpath`               | **absent** (exit 1) — confirming the guard must be, and is, a real platform check                        |
| stdout before the change           | `/tmp/codegraph-legacy-fixture/1a14d195…/codegraph`                                                      |
| stdout after the change            | identical; `diff` clean; both SHA-256 `63cca31136bc556a423eccb874f36d4ec20ed848c27ee8e31ec37392d2ffcdac` |
| stderr before vs after             | `diff` clean                                                                                             |
| `--print` mode after the change    | exit **0**, same path                                                                                    |

macOS is not runnable here, but it takes the identical branch: `uname -s` returns
`Darwin`, which falls into the non-Windows arm and prints verbatim without ever
looking for `cygpath`. Harness scenario `B_darwin` exercises exactly that arm
with a stubbed `Darwin` uname.

### Gates and their REAL exit status

Each run separately; none joined with `&&`.

| Command                                                       | Exit  | Result                                                        |
| ------------------------------------------------------------- | ----- | ------------------------------------------------------------- |
| `bash scripts/setup-legacy-fixture.sh` (baseline, pre-change) | **0** | `/tmp/codegraph-legacy-fixture/1a14d195…/codegraph`           |
| `bash scripts/setup-legacy-fixture.sh` (post-change)          | **0** | byte-identical stdout AND stderr                              |
| `bash scripts/setup-legacy-fixture.sh --print` (post-change)  | **0** | same path                                                     |
| `bash scripts/tests/legacy-fixture-path-domain.test.sh`       | **0** | **10 passed, 0 failed**                                       |
| negative control: pre-fix script                              | **2** | harness cannot find `emit_consumer_path` — depends on the fix |
| negative control: one emission site regressed                 | **1** | **9 passed, 2 failed**, both on the static route check        |

The harness is a manual POSIX fixture harness, run the same way as
`scripts/tests/install-checksum.test.sh`, `asset-name-drift.test.sh`,
`ci-gate.test.sh` and `check-workspace-versions.test.sh` — none of the four are
wired into `guardrail.sh`, `make ci` or any workflow, and this one follows that
existing convention rather than changing it.

### CI round 5 — final gate statuses

| Command                                                                        | Exit  | Result                                                                         |
| ------------------------------------------------------------------------------ | ----- | ------------------------------------------------------------------------------ |
| `bash scripts/check-workspace-versions.sh`                                     | **0** | workspace / `version.txt` / manifest all `0.40.4`                              |
| `cargo test --locked -p codegraph-rs --test batch_m_legacy_extension_override` | **0** | **21 passed, 0 failed** — incl. the test that fails on windows                 |
| same, with `CODEGRAPH_LEGACY_BIN` exported from the fixed script               | **0** | **21 passed, 0 failed** — proves the emitted path is consumable                |
| `make fmt`                                                                     | **0** | run AFTER this prose, BEFORE CI                                                |
| re-ran the harness + the script after `make fmt`                               | **0** | 10 passed; emitted path still byte-identical to the baseline                   |
| `make ci CARGO='cargo --locked'` run 1                                         | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`              |
| `make ci CARGO='cargo --locked'` run 2                                         | **0** | `✅ All CI checks passed!`                                                     |
| `git diff --stat b71a5f1..HEAD -- reference/golden/`                           | **0** | EMPTY — no golden byte changed                                                 |
| `sha256sum Cargo.lock`                                                         | **0** | `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` — unchanged |

The equivalence oracle left NO untracked `colby.db-{wal,shm}` sidecars this round;
`git status --short` after both CI runs showed only the three intended files.
Three files changed: `scripts/setup-legacy-fixture.sh`, the new
`scripts/tests/legacy-fixture-path-domain.test.sh`, and this ledger. **Windows
was NOT validated locally** — the whole point of this defect is that it only
exists across a Windows shell boundary; only the `Windows` CI job can confirm it.

## PR #173 CI round 6 — two failures, two unrelated root causes; the gate itself was innocent (2026-07-29)

Run [30451206766](https://github.com/sunerpy/codegraph-rust/actions/runs/30451206766)
on `5845f11`. `Test` (Linux) and `Security Audit` PASS. Two jobs red:

| Job                                                                                              | Step                     | Failure                                                                       |
| ------------------------------------------------------------------------------------------------ | ------------------------ | ----------------------------------------------------------------------------- |
| `Windows`                                                                                        | Run tests                | `lock_only_parent_is_not_a_status_discovery_marker` — `batch_m_uninit.rs:558` |
| [`Coverage`](https://github.com/sunerpy/codegraph-rust/actions/runs/30451206766/job/90573437980) | Generate coverage (lcov) | `daemon_idle_exits_after_last_client` — `daemon_idle.rs:218`, exit status 101 |
| `CI Success`                                                                                     | Check job status         | consequence only: `windows` was the sole ❌ row                               |

### The gate-membership question, answered first

`Coverage` did NOT leak into the gate. `ci-success` in `.github/workflows/ci.yml`
declares `needs: [test, audit, windows]` — three jobs, `coverage` deliberately
absent. Two independent proofs:

1. The failing run's own `NEEDS_JSON`, echoed by the step, contains exactly
   `test`, `audit`, `windows` and nothing else; the printed table was
   `✅ audit`, `✅ test`, `❌ windows`, and `❌ CI failed: 1 of 3 required job(s)`.
   So `CI Success` failed because of `Windows` ALONE.
2. `bash scripts/check-ci-gate.sh` exits **0** locally and prints
   `required jobs : audit, test, windows` /
   `excluded (informational, justified): coverage`, then runs 16 truth-table
   cases over the shipped step body.

The gate added in `ec93ef6` is intact and needs no change. `codecov.yml` still
carries `informational: true`; `Coverage` stays non-blocking by design. Its
redness is a REAL test failure worth fixing on its own merits — it is simply not
what turned `CI Success` red.

### Failure 1 — a TEST-side path-separator assumption, not a production defect

`crates/codegraph-cli/tests/batch_m_uninit.rs:548` built its nested directory
from a slash-containing literal:

```rust
let nested = project.join("a/b");
```

`Path::join` does not translate an embedded `/`. On Windows the resulting
`PathBuf` therefore stringifies with MIXED separators — `…\project\a/b`. The CLI
does not: `cmd_status` takes its start path through
`absolute_path` → `normalize_lexical`, which rebuilds the path from
`Path::components()`, and the Windows path parser treats `/` as a separator and
re-emits it as `\`. So the reported `projectPath` is correctly `…\project\a\b`
and the EXPECTATION was the malformed value. The CI panic shows exactly that
asymmetry — `left` is the CLI's (correct) output, `right` the test's literal:

```
 left: String("C:\\Users\\RUNNER~1\\...\\project\\a\\b")
right: "C:\\Users\\RUNNER~1\\...\\project\\a/b"
```

Why only this one test of the six: the sibling
`nested_status_discovers_authenticated_non_current_parent_states` (line 490) uses
the same `join("a/b")` but asserts `projectPath` against `project`, never against
`nested`, so it never stringifies the mixed path.

**Verdict: the test's platform assumption is the defect.** Production is right on
Windows, so nothing was gated and no coverage was lost — the invariant (a
lock-only parent is NOT a discovery marker, so status reports the INVOCATION dir
and `extractionStatus: missing`) is asserted exactly as before, against a path
built component-by-component in the native separator domain:

```rust
let nested = project.join("a").join("b");
```

### Failure 2 — a REAL race on BOTH platforms: a sibling's `remove_var` steals the idle window

The `Coverage` job is not instrumentation-sensitive and the failure is not a
`cargo llvm-cov`, lcov-write or Codecov-upload problem: the failing step is
`Generate coverage (lcov)`, and it failed because
`cargo test --tests … --workspace` under it exited 101 on a genuine assertion.

`daemon_idle.rs` holds TWO `#[test]`s that both run in parallel in one binary,
and both spawn a detached daemon through:

```rust
unsafe { std::env::set_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", idle_ms) };
spawn_detached_daemon(&bin(), project, false)?;   // Command snapshots env HERE
unsafe { std::env::remove_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS") };
```

`std::env` is process-global and `Command` snapshots the environment INSIDE
`spawn_detached_daemon`. When the sibling's `remove_var` lands between this
test's `set_var` and that snapshot, the daemon inherits NO idle var, so
`resolve_idle_timeout_ms` falls back to `DEFAULT_IDLE_TIMEOUT_MS` = **300 000 ms**
and the process cannot possibly be gone inside `wait_until_gone(…, 10s)`.

The CI log confirms the interleave: both projects were initialized 2 ms apart
(`12:23:04.4153` / `12:23:04.4177`) so both tests were in flight, and
`daemon_stays_alive_with_active_client` (which spawns with `"500"`) finished at
`12:23:05.64` — squarely inside the other test's spawn window.

Proof by direct reconstruction, spawning the daemon with the idle var ABSENT and
everything else identical (`/tmp/opencode/repro3.py`, three runs):

```
daemon STILL ALIVE 12.0s after last client disconnected -> reproduces the CI assertion failure
```

and with the var present, both the plain and the instrumented binary idle-exit
identically, so instrumentation is exonerated:

```
plain          rc=0  exit_after_disconnect=2.22s  loop_break->stopped=0.01s
instrumented   rc=0  exit_after_disconnect=2.22s  loop_break->stopped=0.01s
```

Why it is newly red rather than always red: it is a scheduling race, not a
deterministic break. 15 plain runs, 12 CPU-pinned runs, 3 instrumented runs and a
full instrumented `--workspace` pass all went green here; the six earlier
`Coverage` runs simply won the race. Nothing about coverage causes it — the same
binary under plain `cargo test` is equally exposed.

**Fix: the same `ENV_LOCK` shape already used by `daemon_single_watcher.rs`** —
one `OnceLock<Mutex<()>>` per test binary, held across the whole
set → spawn → restore region so the spawned daemon always inherits the window its
own test chose. No sleep, no widened timeout, no `#[ignore]`, no weakened
assertion; the 10 s ceiling and both assertions are untouched.
`daemon_sweep.rs` carries the identical shape (two parallel tests, same
set → spawn → remove sequence, `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS` +
`CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`, defaults 30 000 / 300 000 ms) and was fixed
in the same commit — it is the same bug, unfired.

### Audit — the class behind each failure, swept across the workspace

Failure 1's class: a test-built path string compared against a path the CLI
printed. There are exactly THREE such comparisons in the whole workspace, all in
`batch_m_uninit.rs`:

| file:line               | assumption                                                     | Windows verdict                                                  |
| ----------------------- | -------------------------------------------------------------- | ---------------------------------------------------------------- |
| `batch_m_uninit.rs:503` | `projectPath == project` where `project = dir.join("project")` | SAFE — single component, no embedded `/`                         |
| `batch_m_uninit.rs:539` | `projectPath == project` where `project = dir.join("mini")`    | SAFE — single component                                          |
| `batch_m_uninit.rs:563` | `projectPath == nested` where `nested` came from `join("a/b")` | **BROKEN — the reported failure. FIXED** (`join("a").join("b")`) |

Other `join("<x>/<y>")` sites exist but none is stringified for comparison — they
are only used as filesystem paths, where the mixed form resolves identically on
Windows:

| file:line                                                                   | use of the slash-joined path                       | Windows verdict                                            |
| --------------------------------------------------------------------------- | -------------------------------------------------- | ---------------------------------------------------------- |
| `batch_m_uninit.rs:388`, `:490`, `:514`                                     | `create_dir_all` + `current_dir` only              | SAFE — never stringified for an equality assertion         |
| `batch_m_v2_namespace.rs:246/247/250`                                       | project roots passed as CLI args                   | SAFE — consumed as paths, compared only via `is_file()`    |
| `catch_up.rs:180`, `sync_incremental.rs:*`, `live_update_direct.rs:196/244` | `fs::write` targets                                | SAFE — filesystem-only                                     |
| `installer.rs:*`, `skill_install.rs:*`, `completions.rs:70/97`              | config file paths, `exists()` / content assertions | SAFE — no path-string equality                             |
| `global_http_project_scoped_config.rs:*`                                    | `format!("{}", …display())` into JSON args         | unix-only target (`#![cfg(unix)]`) — never runs on Windows |
| `batch_m_legacy_extension_override.rs:335`                                  | `script.display()` in a panic message              | SAFE — diagnostic text, not an assertion                   |

Failure 2's class: a `set_var` → spawn sequence in a multi-test binary. Four test
binaries mutate env and spawn a child; two were already guarded, two were not:

| file                       | env sites | guard before             | Windows/Linux verdict                         |
| -------------------------- | --------- | ------------------------ | --------------------------------------------- |
| `daemon_single_watcher.rs` | 3         | `env_guard()` ✅         | SAFE — this is the shape copied               |
| `lease_lifetime.rs`        | 4         | `test_serial_guard()` ✅ | SAFE — restores under the guard               |
| `daemon_idle.rs`           | 3         | **none**                 | **BROKEN on both platforms. FIXED**           |
| `daemon_sweep.rs`          | 3         | **none**                 | **BROKEN on both platforms (unfired). FIXED** |

Filesystem-residue / removability / `io::ErrorKind` assertions, the class behind
layers 4-5, were re-swept: every test-side `ErrorKind` match is `NotFound`
(portable), plus `WouldBlock`/`TimedOut`/`Interrupted`/`InvalidData` in
socket/parse paths that carry no platform assumption. Four `remove_dir_all` calls
assert success — `lease_lifetime.rs:871`, `reopen.rs:152`,
`typescript_engine.rs:178/207`. All are Windows-visible and all PASSED on the
Windows runner in this very run (`reopen` 3 passed, and the store/extract suites
green), so no Windows handle-retention problem is latent there; left unchanged
rather than speculatively touched.

Coverage note on what the Windows job still has NOT proven: `cargo test` stops at
the first failing target, so every CLI target alphabetically after
`batch_m_uninit` never executed on Windows this round — 24 of them are
Windows-visible (`batch_m_v2_namespace` … `unicode_search_cli`). This fix is what
lets them run; further layers may surface behind it, exactly as layers 1-6 did.

### Gates and their REAL exit status

Run separately, never joined with `&&`.

| Command                                                               | Exit  | Result                                                    |
| --------------------------------------------------------------------- | ----- | --------------------------------------------------------- |
| `gh run view 30451206766 --log-failed`                                | **0** | 6090 lines; only `Windows`, `Coverage`, `CI Success`      |
| `bash scripts/check-ci-gate.sh`                                       | **0** | required = audit, test, windows; coverage excluded        |
| `cargo test --locked -p codegraph-rs --test batch_m_uninit` (pre-fix) | **0** | 6 passed — the defect is Windows-only, invisible on Linux |
| `python3 /tmp/opencode/repro3.py` ×3 (idle var absent)                | —     | daemon alive at 12.0s each time — Failure 2 reproduced    |

### Round 6 — final gate statuses

Prose written FIRST, then `make fmt`, then the gates, so `fmt-check` sees the
formatted ledger. Each command run separately; the exit status below is the one
the shell actually reported.

| Command                                                                | Exit  | Result                                                                         |
| ---------------------------------------------------------------------- | ----- | ------------------------------------------------------------------------------ |
| `cargo test --locked -p codegraph-rs --test batch_m_uninit` (post-fix) | **0** | **6 passed, 0 failed** (twice)                                                 |
| `cargo test --locked -p codegraph-rs --test daemon_idle`               | **0** | **2 passed, 0 failed**                                                         |
| `cargo test --locked -p codegraph-rs --test daemon_sweep`              | **0** | **2 passed, 0 failed**                                                         |
| `make fmt`                                                             | **0** | run after this prose, before the gates                                         |
| `make ci CARGO='cargo --locked'` run 1                                 | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`              |
| `make ci CARGO='cargo --locked'` run 2                                 | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`              |
| `make ci CARGO='cargo --locked'` run 3                                 | **0** | `✅ All CI checks passed!` — final, with this ledger in the tree               |
| `bash scripts/check-workspace-versions.sh`                             | **0** | workspace / `version.txt` / manifest all `0.40.4`                              |
| `bash scripts/check-ci-gate.sh`                                        | **0** | 16 truth-table cases; required = audit, test, windows                          |
| `git diff --stat 5845f11..HEAD -- reference/golden/`                   | **0** | EMPTY — no golden byte changed                                                 |
| `sha256sum Cargo.lock`                                                 | **0** | `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` — unchanged |

`git status --short` after every CI run showed only the four intended files and no
untracked `reference/golden/*/colby.db-{wal,shm}` sidecars. **Windows was NOT
validated locally** — no MSVC toolchain exists on this host (`cargo xwin` fails on
a missing `clang-cl`), so only the `Windows` CI job can confirm Failure 1's fix.
Failure 2's mechanism, by contrast, WAS reproduced and re-verified locally.

## PR #173 CI round 7 — the proxy's teardown waited for a peer close that Windows named pipes can never deliver (2026-07-29)

### The failure and where CI reported it

GitHub Actions run `30460073159`, job `Windows`, the EIGHTH Windows-only failure
on PR #173 and the only one left at commit `a0315da`:

```
test reader_lease_spans_stamp_check_through_last_row ... FAILED
panicked at crates\codegraph-cli\tests\lease_lifetime.rs:820:14:
proxy completed before deadline: Timeout
test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.91s
```

The `expect` at `:820` is on the OUTER `Result` of
`done_rx.recv_timeout(WAIT)` — a `mpsc::RecvTimeoutError::Timeout`. So
`run_proxy` never sent a result at all within the 10s `WAIT`: it did not fail,
it did not return; it never returned.

### Diagnosis — `run_proxy`'s teardown blocked on a UNIX-only event

`run_proxy` (`crates/codegraph-daemon/src/proxy.rs`) runs two pumps and then
tears down in this order:

```rust
half_close_write(write_fd);
let _ = down.join();          // <- the unbounded wait
```

`down` is the daemon->host pump. `pump_daemon_to_host` loops
`for line in daemon_recv.lines()` and returns ONLY on EOF or a read error, i.e.
only once the DAEMON closes its end of the connection. The daemon is made to
close by `half_close_write`, which issues `shutdown(SHUT_WR)` on the socket fd so
the daemon's session reader EOFs, flushes its last reply, and closes.

Every step of that chain is unix-only:

- `half_close_write` is `#[cfg(unix)] … rustix::net::shutdown(fd, Shutdown::Write)`,
  and the `#[cfg(not(unix))]` arm is `fn half_close_write(_write_fd: Option<WriteFd>) {}` —
  a no-op whose own doc comment already stated the fact: _"Windows named pipes
  have no half-close; the proxy relies on the full-stream drop + the daemon's own
  idle/sweep lifecycle instead."_
- `write_raw_fd` likewise returns `None` on `#[cfg(not(unix))]`, so there is no
  handle to act on even in principle.
- Dropping the send half does not help either. interprocess's `split()` gives
  both halves a refcount over the SAME kernel object
  (`interprocess-2.4.2/src/os/windows/named_pipe/stream/impl.rs:35-41`:
  `let (raw_ac, raw_a) = (self.raw.refclone(), self.raw)`, i.e. one
  `Arc<RawPipeStream>` shared by recv and send). While the proxy's recv half
  lives, the pipe handle is open, so the daemon never sees a disconnect.
- The daemon side, meanwhile, is parked. `serve_session_async`
  (`session.rs:434`) races the rmcp serve loop against its closing watch
  channel; with neither an EOF nor a shutdown signal it simply awaits the next
  frame forever.

So on Windows: the proxy writes its frames, the daemon answers them, the proxy
delivers the answers — and then `down.join()` waits for an EOF that nothing in
the system will ever produce. `run_proxy` never returns, the test's channel
times out at 10s, and the reported 11.91s is the test binary's own total, not a
slow round trip.

This also disproves the "the deadline is too short" hypothesis: the work the
test measures had already completed. The proxy had received the `"id":2` reply
before it wedged — the failure is strictly in teardown, and no `WAIT` value
would fix an unbounded wait. `WAIT` was not touched; no sleep, retry, or
conditional assertion was added.

Why only this test caught it: `proxy_e2e.rs` is `#![cfg(unix)]`, so the only
other test that drives `run_proxy` over a real rendezvous never runs on Windows.
`lease_lifetime.rs` carries no cfg gate and is the sole Windows-visible caller.

### Verdict: PRODUCTION defect, not a test premise

The test's premise holds on Windows exactly as on Unix — a reader lease must
span the stamp check through the last row, and the proxy did produce the last
row. What is broken is `run_proxy`'s own liveness: a real MCP host on Windows
whose stdin closes would leave the launcher process hung instead of exiting,
holding its daemon session open. Nothing was gated; no Windows coverage was
given up.

### The fix — wait on what is OWED, not on the peer closing

`run_proxy` now bounds teardown with a `ReplyLedger`: the set of forwarded
request ids the host is still owed a reply for.

- `pump_host_to_daemon` records an id when, and only when, it FORWARDS a request
  carrying one. Notifications (no `id`) are never recorded; `initialize` and
  `tools/list` are answered locally and are not owed by the daemon.
- `pump_daemon_to_host` retires an id AFTER writing that reply to the host, so
  the ledger settles only once the answer is genuinely in the host's hands.
- Teardown replaces `down.join()` with `ledger.wait_until_settled(REPLY_DRAIN_BUDGET)`.
  This is a `Condvar` wait that returns the instant nothing is owed, so the
  common path is not slower than before on either platform.
- When the daemon stream ends with replies still outstanding they can never be
  answered, so the down pump calls `ledger.abandon()` on exit, waking the wait
  immediately rather than burning the budget.
- `down.join()` is now conditional on `HALF_CLOSE_EOFS_DAEMON` (`true` on unix,
  `false` elsewhere) — joined where the pump is known to terminate, detached
  where it is not. The detached thread holds only clones and ends when the daemon
  closes the pipe via its own idle-exit/sweep/shutdown lifecycle.
- `REPLY_DRAIN_BUDGET` (20s) is a last-resort bound against a daemon that
  accepted the connection and then stalled forever; it is deliberately LONGER
  than callers' own deadlines so a genuine stall still surfaces as the caller's
  failure. Expiry returns `Err`, never a false `Proxied`.

Byte semantics of both pumps are unchanged: the same lines are forwarded, the
same initialize reply is suppressed, and the last reply is still delivered before
teardown — that guarantee is now enforced by the ledger rather than inferred from
a peer close.

### Audit — every test that drives `run_proxy` or the rendezvous

| test file                                                                     | platform gate              | drives                                      | same platform assumption?                          |
| ----------------------------------------------------------------------------- | -------------------------- | ------------------------------------------- | -------------------------------------------------- |
| `lease_lifetime.rs`                                                           | **none — runs on Windows** | `run_proxy` + `recorded_socket_path`        | **YES — this is the failure. FIXED in production** |
| `proxy_e2e.rs`                                                                | `#![cfg(unix)]`            | `run_proxy` (11 sites)                      | never runs on Windows — no exposure                |
| `daemon-crate` `proxy.rs`                                                     | `#![cfg(unix)]`            | proxy attach                                | never runs on Windows — no exposure                |
| `proxy.rs` unit `run_proxy_falls_back_on_version_mismatch_over_a_real_socket` | `#[cfg(unix)]`             | `run_proxy` (mismatch arm)                  | returns BEFORE the split/teardown — unaffected     |
| `daemon_idle.rs`                                                              | `#![cfg(unix)]`            | `recorded_socket_path`                      | never runs on Windows                              |
| `daemon_single_watcher.rs`                                                    | `#![cfg(unix)]`            | `recorded_socket_path`                      | never runs on Windows                              |
| `daemon_spawn.rs`                                                             | `#![cfg(unix)]`            | `daemon_socket_path`                        | never runs on Windows                              |
| `daemon_stale_wal_recovery.rs`                                                | `#![cfg(unix)]`            | `recorded_socket_path`                      | never runs on Windows                              |
| `daemon_sweep.rs`                                                             | `#![cfg(unix)]`            | `recorded_socket_path`                      | never runs on Windows                              |
| `stale_socket.rs`                                                             | `#![cfg(unix)]`            | `daemon_socket_path` (9 sites)              | never runs on Windows                              |
| `lifecycle.rs`                                                                | **none — runs on Windows** | `daemon_socket_path` for a lock record only | NO — writes a pid-lock file, never connects        |

Only two Windows-visible files touch the rendezvous at all, and only
`lease_lifetime.rs` connects through it. `lifecycle.rs` uses
`daemon_socket_path` purely as a value inside a `DaemonLockInfo` it writes to
disk, so it carries no transport assumption. Nothing else needed changing, and
nothing was changed speculatively.

### Regression proof — four new unit tests, all platform-independent

The defect is a Windows-only SYMPTOM of a platform-independent teardown
condition, so the ledger that replaces it is testable on Linux. All four tests
live in `crates/codegraph-daemon/src/proxy.rs`'s existing `#[cfg(test)] mod tests`
and exercise `ReplyLedger` directly — no socket, no daemon, no timing luck.

| test                                                                 | what it pins down                                                                                                                                                                                                                                             |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `reply_ledger_settles_on_the_last_delivered_reply_not_on_peer_close` | Two owed ids keep the ledger unsettled; a thread parked on a 10s budget is woken by the retirement of the last one and asserts `elapsed < 5s` — proving the wake is the RETIREMENT, not the budget expiring, which is exactly the substitution the fix makes. |
| `reply_ledger_abandon_settles_an_unanswerable_wait`                  | A daemon stream that ends still owing id 4 can never answer it; `abandon()` settles a 10s wait in `< 500ms`.                                                                                                                                                  |
| `reply_ledger_ignores_an_unrecorded_retirement`                      | Retiring an id that was never recorded (a daemon reply to something the proxy did not forward, e.g. the primed `initialize`) neither underflows the ledger nor falsely settles a wait that still owes id 1.                                                   |
| `reply_ledger_reports_an_unsettled_budget_expiry`                    | A genuinely stalled daemon still yields `false` at the budget and takes at least the budget to do so — teardown is BOUNDED, and the caller sees `Err`, never a false `Proxied`.                                                                               |

Three pre-existing pump tests were extended rather than replaced, so the ledger
is also covered through its real call sites:
`pump_host_to_daemon_answers_initialize_and_tools_list_locally` now asserts that
ONLY the forwarded `tools/call` (id 3) is owed — the locally answered
`initialize` (id 1) and `tools/list` (id 2) are deliberately not recorded, since
recording either would leave the ledger permanently unsettled — and
`pump_daemon_to_host_suppresses_the_recorded_initialize_reply` now asserts the
ledger settles once the owed reply has been written to the host.

The Windows path itself remains UNVERIFIED LOCALLY: no MSVC toolchain exists on
this host (`cargo xwin` fails on a missing `clang-cl`, there is no package
manager, and uid is 1000), so only the `Windows` CI job and the CodeBuild
`codegraph-rs-windows` project can confirm that `run_proxy` now returns there.
What IS proven locally is the mechanism the fix depends on, and that Linux
behaviour is unchanged.

### Round 7 — final gate statuses

Prose written FIRST, then `make fmt`, then the gates, so `fmt-check` sees the
formatted ledger. Each command run separately, never joined with `&&`; the exit
status below is the one the shell actually reported.

| Command                                                     | Exit  | Result                                                                           |
| ----------------------------------------------------------- | ----- | -------------------------------------------------------------------------------- |
| `make fmt`                                                  | **0** | run after this prose, before the gates                                           |
| `bash scripts/check-workspace-versions.sh`                  | **0** | workspace / `version.txt` / manifest all `0.40.4`; 10 packages                   |
| `bash scripts/check-ci-gate.sh`                             | **0** | 16 truth-table cases; required = audit, test, windows; coverage excluded         |
| `cargo test --locked -p codegraph-daemon`                   | **0** | **140 passed, 0 failed** over 11 targets; all four `reply_ledger_*` tests `ok`   |
| `cargo test --locked -p codegraph-rs --test lease_lifetime` | **0** | **5 passed, 0 failed** — incl. `reader_lease_spans_stamp_check_through_last_row` |
| `make ci CARGO='cargo --locked'` run 1                      | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`                |
| `make ci CARGO='cargo --locked'` run 2                      | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`                |
| `git diff --stat a0315da..HEAD -- reference/golden/`        | **0** | EMPTY — no golden byte changed                                                   |
| `sha256sum Cargo.lock`                                      | **0** | `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` — unchanged   |

The equivalence oracle left untracked `reference/golden/godot/colby.db-{wal,shm}`
sidecars after each `make ci`; they are not gitignored and were removed before
staging, so the working tree carried only the intended files at commit time.

**Windows was NOT validated locally** and this ledger claims no such result — no
MSVC toolchain exists on this host. Confirmation comes from the `Windows` GitHub
Actions job and from the CodeBuild `codegraph-rs-windows` project, which runs
`cargo test --workspace` without stopping at the first failing crate and so
reports the full remaining picture in one pass.

### CodeBuild verification on a REAL Windows host — the fix is CONFIRMED

Windows cannot be compiled on this host (`cargo xwin` fails on a missing
`clang-cl`; no package manager; uid 1000), so the fix was validated on AWS
CodeBuild (`us-east-2`, project `codegraph-rs-windows`), which runs
`cargo test --workspace` and — unlike the GitHub `Windows` job's early exit —
yields the COMPLETE remaining failure list in one pass.

| Step                                                                  | Exit / Result                                                    |
| --------------------------------------------------------------------- | ---------------------------------------------------------------- |
| `GIT_MASTER=1 git push cc feat/upstream-v1.5-portable-fixes`          | **0** — `5845f11..47ee705`, pre-push hook `✅ all checks passed` |
| `aws codepipeline start-pipeline-execution --name codegraph-rust-ci`  | **0** — execution `f55f1270-9e07-43a6-a62f-df1666b1e009`         |
| CodeBuild `codegraph-rs-windows:97aa74b8-d9b6-4759-aa9b-bd55f1311f13` | `FAILED` — but NOT on this fix; see below                        |

The target under diagnosis PASSED:

```
Running tests\lease_lifetime.rs (target\debug\deps\lease_lifetime-7b5c80ba6d9b7683.exe)
running 4 tests
test lease_lifetime_child_process ... ok
test writer_blocks_new_reads_without_sqlite_open ... ok
test sqlite_snapshot_rejects_non_regular_artifact ... ok
test reader_lease_spans_stamp_check_through_last_row ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.99s
```

2.99s for the whole binary, and 4s of wall clock between the target starting and
finishing — against a 10s `WAIT`. That is the measured refutation of the
"deadline too short" hypothesis: with the unbounded join removed, the Windows
round trip fits inside the EXISTING deadline with room to spare. `WAIT` remains
10s, untouched.

This run also finally executed the whole workspace on Windows: **96 test targets
ran, 105 `test result: ok`**, versus the GitHub job's early exit after the first
failing crate. Everything the previous seven rounds could only hypothesize about
is now measured.

### The NINTH failure this unblocked (out of scope here, recorded for the next round)

With the proxy no longer hanging, `cargo test --workspace` reached
`-p codegraph-store --lib` and surfaced a NEW, unrelated Windows-only failure:

```
test connection::tests::current_writer_revalidates_lock_at_the_last_pre_open_checkpoint ... FAILED
panicked at crates\codegraph-store\src\connection.rs:1218:25:
snapshot read …\.codegraph-v2\displaced-at-write-open.lock:
  The process cannot access the file because another process has locked a portion of the file. (os error 33)
test result: FAILED. 93 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.07s
```

Mechanism, from the code: the test renames the STILL-LEASED permanent lock to
`displaced-at-write-open.lock` (`connection.rs:1505`) and then snapshots the
whole tree; `snapshot_tree`'s walk does `std::fs::read` on every regular file
(`connection.rs:1217-1219`). The lease holds that file via
`file.try_lock()` (`index_lease.rs:383`), which is `flock` on unix — ADVISORY,
so a concurrent `read` succeeds — but `LockFileEx` on Windows, which is
MANDATORY over a byte range, so the same `read` fails with
`ERROR_LOCK_VIOLATION` (os error 33). The failure is in the test's snapshot
oracle, not in the lease revalidation it is asserting (that assertion is never
reached). This is a distinct root cause and is NOT addressed by this commit.

### Round 7 — gate statuses, as the shell actually reported them

Prose written FIRST, then `make fmt`, then the gates, so `fmt-check` sees the
formatted ledger. Each command run separately, never joined with `&&`.

| Command                                                     | Exit  | Result                                                                         |
| ----------------------------------------------------------- | ----- | ------------------------------------------------------------------------------ |
| `cargo build --locked -p codegraph-daemon`                  | **0** | clean                                                                          |
| `cargo clippy --locked -p codegraph-daemon --all-targets`   | **0** | zero warnings                                                                  |
| `cargo test --locked -p codegraph-daemon --lib`             | **0** | **109 passed, 0 failed** — incl. 4 new `reply_ledger_*` tests                  |
| `cargo test --locked -p codegraph-rs --test lease_lifetime` | **0** | **5 passed, 0 failed** on Linux                                                |
| `make fmt`                                                  | **0** | run after this prose, before the gates                                         |
| `cargo fmt --all --check`                                   | **0** | clean                                                                          |
| `make ci CARGO='cargo --locked'` run 1                      | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`              |
| `make ci CARGO='cargo --locked'` run 2                      | **0** | `✅ All CI checks passed!` — 125 `test result: ok`, zero `FAILED`              |
| `bash scripts/check-workspace-versions.sh`                  | **0** | workspace / `version.txt` / manifest all `0.40.4`                              |
| `bash scripts/check-ci-gate.sh`                             | **0** | 16 truth-table cases; required = audit, test, windows                          |
| `git diff --stat a0315da..HEAD -- reference/golden/`        | **0** | EMPTY — no golden byte changed                                                 |
| `sha256sum Cargo.lock`                                      | **0** | `750ee84b48ef1fc988bf9efd1a75828d243734f9bc516e8671c4294183de9bb1` — unchanged |

`git status --short` was clean before the work started (no foreign edits from
another session) and after each CI run showed only the intended files, with no
untracked `reference/golden/*/colby.db-{wal,shm}` sidecars. **Windows was NOT
validated locally** — only the CodeBuild run above can and does confirm it.
