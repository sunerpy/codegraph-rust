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
