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
- First-ever Cargo invocation of the task = the gate run below. A logging Cargo
  shim placed first on `PATH` captured exactly one Cargo call during a gate run,
  and it was the metadata command:

  ```text
  CARGO_INVOCATION: cargo metadata --locked --no-deps --format-version 1 --manifest-path /tmp/opencode/codegraph-rust-v15-impl/Cargo.toml
  ```

  That is the ONLY Cargo subprocess the gate spawns, and it is non-mutating.

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

### Fixture harness — five scenarios, all as expected

`scripts/tests/check-workspace-versions.test.sh` builds dependency-free temporary
workspaces so their locks are trivially consistent (`cargo metadata --locked`
accepts them), then drives the gate. Result: `5 passed, 0 failed`. Every failure
scenario exits nonzero on a precise business assertion (never a
compile/setup/environment failure), and every scenario leaves the fixture
`Cargo.lock` byte-for-byte unchanged.

| Scenario                   | Description                                                       | Expected | Observed exit | Diagnostic (business assertion)                                                                                              | Lock unchanged |
| -------------------------- | ----------------------------------------------------------------- | -------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------- | -------------- |
| A `manifest_lock_drift`    | manifest `0.40.4` / lock members `0.40.3`                         | nonzero  | `1`           | `Cargo.lock package 'fixture-pa' = '0.40.3' != [workspace.package] version = '0.40.4'` (and `fixture-pb`)                    | yes            |
| B `package_set_mismatch`   | extra source-less lock entry (`vendored` excluded from workspace) | nonzero  | `1`           | `workspace package set differs between cargo metadata and Cargo.lock (source-less)` … only in Cargo.lock: `fixture-vendored` | yes            |
| C `stale_version_txt`      | `version.txt` = `0.40.3`, all else `0.40.4`                       | nonzero  | `1`           | `version.txt = '0.40.3' != [workspace.package] version = '0.40.4'`                                                           | yes            |
| D `stale_release_manifest` | manifest `"."` = `0.40.3`, all else `0.40.4`                      | nonzero  | `1`           | `.release-please-manifest.json "." = '0.40.3' != [workspace.package] version = '0.40.4'`                                     | yes            |
| E `repository_green`       | the real repository lock                                          | zero     | `0`           | `check-workspace-versions: OK`                                                                                               | yes            |

### Scope note

This task adds only the gate and its fixture harness. It does NOT begin Batch M,
does NOT touch the release-please `GenericToml`/`codegraph*` selector, the pinned
Action commit, any GitHub/AWS workflow, the `Makefile`, hooks, or CI wiring — all
of which remain E2 follow-up work. No version value in `Cargo.toml`, `Cargo.lock`,
`version.txt`, or `.release-please-manifest.json` was modified. No third-party
dependency was added; the gate uses only `bash`, `awk`, `sed`, `jq`, and
`sha256sum` already present in the repository's toolchain.
