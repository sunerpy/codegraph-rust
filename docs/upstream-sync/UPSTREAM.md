# Upstream sync ledger — codegraph-rs ⇄ colbymchenry/codegraph

> Single source of truth for which upstream (colby) version this Rust port is
> faithful to, and what has been ported / evaluated. The `colby-upstream-sync`
> skill reads this first and updates it last. This is the project's memory —
> a good entry stops the next sync from re-investigating the same change.

## Current alignment

- **Tracked colby version:** `1.5.0` (the `1.4.1 → 1.5.0` PORT subset + the
  post-`v1.5.0` portable commits landed across codegraph-rs `v0.42.1`–`v0.42.4`;
  see the 2026-08-02 CLOSEOUT entry below for the batch table. The preceding
  `1.2.0 → 1.4.1` subset landed across `v0.28.3`–`v0.39.0`; see the 2026-07-17
  CLOSEOUT entry)
- **This project version:** see `Cargo.toml` (`codegraph-rs`, independent line)
- **Last audit:** `2026-08-27` (refreshed `2026-08-31`) — upstream `v1.6.0`
  plus its one following
  docs-only commit was audited through
  `6a056ec5db35172f9dc348f87b54ea415aa5169e`. The exact new range after the
  previously closed 104-commit batch is 28 commits and leaves 15 PORT families;
  see `V1_6_TRIAGE_2026-08-27.md`. Tracked parity therefore remains `1.5.0`
  until that backlog lands
- **Previous exact boundary:** `c6aaa20358cd6adcd04b87bdef8e5803ad146f3a`
  (the 104th commit after `v1.5.0`; merge of upstream PR #1516)
- **Next discovery starts here:** diff
  `6a056ec5db35172f9dc348f87b54ea415aa5169e..origin/main`; do not re-triage
  the v1.6 backlog — use `V1_6_TRIAGE_2026-08-27.md`
- **Upstream repo:** https://github.com/colbymchenry/codegraph

> **1.4.1 → 1.5.0 sync: COMPLETE** (codegraph-rs `v0.42.1` → `v0.42.4`). Every
> portable, in-scope behavior in that range has landed (see the 2026-08-02
> CLOSEOUT entry below). Tracked parity is `1.5.0` with the following recorded
> caveats — this is "faithful to 1.5.0 for all portable in-scope behavior, with
> these explicit exceptions," not byte-for-byte everything:
>
> - **26 DEFER items remain deferred** — 19 resolution/store/synthesis perf
>   strategies plus 7 sync/WAL scheduling strategies, all tuned to upstream's Node
>   / `node:sqlite` runtime (pool sizing, cgroup cache credit, macOS `vm_stat`
>   budgets). They are byte-identical to graph output, so not parity blockers;
>   re-derive each from a Rust profile before porting rather than transcribing
>   upstream's constants. **`2adc7f6`** (WAL truncate only at parked barriers)
>   flips from a perf item to a CORRECTNESS item the moment timer-triggered
>   truncation is adopted here.
> - **`0682137` is N/A (packaging-rooted)** — we ship a native `codegraph.exe`,
>   never a `.cmd` shim, so writing `codegraph.cmd` would name a nonexistent file.
> - **Both formerly-open triaged issues have SHIPPED** — **#1495** Kotlin callable
>   signatures in `v0.42.6` (a documented DIVERGENCE from upstream, not a port), and
>   **#1482 / #1482b** TS export-alias resolution plus `.js`→`.ts` specifier
>   substitution in `v0.42.8`. Both are golden-affecting and carry new fixtures; see
>   the 2026-08-04 entry.
> - The `1.2.0 → 1.4.1` caveats below all still hold and carry forward unchanged.

> **1.2.0 → 1.4.1 sync: COMPLETE.** Every portable, in-scope behavior in that
> range has landed (see the 2026-07-17 CLOSEOUT entry below). Tracked parity is
> `1.4.1` with the following recorded caveats — this is "faithful to 1.4.1 for
> all portable in-scope behavior, with these explicit exceptions," not
> byte-for-byte everything:
> - **Framework-resolution bridges deferred per-language** — ArkUI dispatch
>   (ArkTS), Nix module-system synthesis, Terraform module-boundary wiring,
>   Erlang OTP behaviour/gen_server/spawn-MFA bridging, CFML
>   `<cfscript>`-in-tag/cfquery/receiver-type resolution. The port keeps its
>   single concrete `FrameworkResolver` (`GodotResolver`); extraction for all
>   these languages is fully ported.
> - **COBOL and VB.NET permanently deferred** — no usable `tree-sitter-cobol`
>   crate on crates.io (name-squat stub) and no `tree-sitter-vbnet`/`-vb`
>   equivalent at all; adding either would require vendoring a git grammar,
>   which violates this project's no-vendored-grammar invariant.
> - **Prompt-hook telemetry excluded by policy** — the confidence-gate mechanics
>   (#1126/#1136/#1138/#1144-1146) landed; all telemetry/usage-counter code was
>   deliberately not ported (this project has no tracking pipeline).
> - **TS/Node-only items are N/A** — event-loop yield points, npm/bundled-Node
>   lifecycle steps, and other TypeScript-runtime-specific fixes have no Rust
>   analogue and were recorded, not ported.

> Note: the two version numbers are independent. This line is the _only_ place
> that records colby parity — do not infer it from `Cargo.toml`.

## Sync log

### 2026-08-31 — Wave 0 deep-AST guard IMPLEMENTED in PR #249; release pending

Upstream `838006c947ebe69407e8f64ba756f97132f411f4` fixed the process-crash
class caused by deeply recursive native walkers. Rust PR
[#249](https://github.com/sunerpy/codegraph-rust/pull/249), commit `53360d9`,
implements the portable behavior with a deterministic design rather than
copying upstream's platform stack-pointer / wasm fallback:

- an iterative preflight rejects logical named-node depth above 256 before any
  recursive semantic traversal;
- the main walker and function-body walker share a fail-closed runtime depth
  latch, while Rust's inert single-child `expression_statement` grammar wrapper
  is folded so 200 levels of ordinary source nesting remain valid;
- an over-limit file contributes no partial nodes, edges, or unresolved
  references, records one stable file error, and does not stop sibling files;
- incremental `sync` removes the file's previous graph and is canonically
  identical to a fresh `index --force` for the same final filesystem state.

Coverage includes 30,000-level C, C++, Rust, TypeScript, and Python inputs on a
1 MiB test thread, the exact inclusive 256/257 boundary, per-file isolation, and
the incremental/full-rebuild equivalence oracle. Local `make ci` and the
pre-push gate both passed. This change deliberately keeps extraction version 11
and changes no golden artifact or node-id formula.

This entry records implementation only. The item remains release-pending until
the release-please PR, six-platform artifacts, checksums, and published Linux
black-box smoke test complete; tracked colby parity therefore remains `1.5.0`.

### 2026-08-31 — implementation baseline refreshed at codegraph-rs `v0.48.1`

Before implementation began, both repositories and the three load-bearing open
issues were refreshed against their live GitHub state:

- codegraph-rs `main` and the latest published release are both `v0.48.1`
  (`e5350b3c6eb6312cfae86f2353375e4bfb5e9a3f`);
- upstream's latest release remains `v1.6.0`, and upstream `main` is still
  exactly `6a056ec5db35172f9dc348f87b54ea415aa5169e`;
- the diff after the audit boundary is empty, so the 15 PORT families below do
  not need re-triage;
- #1626 (Python aliases), #1616 (Lua function expressions), and #1617 (custom
  Lua loaders) remain open. The first two stay in the implementation backlog;
  #1617 stays explicitly deferred.

This refresh changes only the Rust implementation/release baseline. It does not
rewrite the 2026-08-27 audit evidence, and tracked upstream parity remains
`1.5.0` until the portable `v1.6.0` backlog lands.

### 2026-08-27 — `v1.6.0` release range AUDITED; 15 PORT families OPEN (tracked parity stays `1.5.0`)

Upstream released `v1.6.0` on 2026-08-26. The earlier post-`v1.5.0` audit
covered exactly 104 commits through `c6aaa20358cd6adcd04b87bdef8e5803ad146f3a`;
that count was re-verified before this round. The fresh range through current
upstream head `6a056ec5db35172f9dc348f87b54ea415aa5169e` is exactly 28 commits
(27 through the release tag plus one docs-only follow-up), 100 files,
+6900/−575.

The complete classification, local source evidence, open-issue triage, and
recommended implementation waves are recorded in
[`V1_6_TRIAGE_2026-08-27.md`](V1_6_TRIAGE_2026-08-27.md).

The highest-priority item is native tree-walker depth protection: upstream
demonstrated a process-level stack-overflow crash on deeply nested source, and
the Rust walker still recurses without a guard. The graph-correctness wave then
contains Erlang arity identity, two Rust method-resolution fixes, JS/TS
object-literal namespaces, inherited TS aliases, C++ `.h` detection, Python
import aliases, Lua function-expression callables, and live watcher-scope
refresh. Retrieval/UX work follows.

One measured performance regression is also now pinned: the Rust database
already has `idx_nodes_lower_name`, but its case-insensitive exact-name query
uses `COLLATE NOCASE` and scans `nodes`; `lower(name) = lower(?)` seeks the
expression index. That upstream change is a real PORT item, not an
already-present optimization.

No source port was implemented in this audit. The next upstream discovery diff
starts after `6a056ec5`; implementation work starts from the linked report.

### 2026-08-20 — post-`v1.5.0` unreleased range TRIAGED and CLOSED, six tiers SHIPPED (`v0.43.2` → `v0.46.1`; tracked parity stays `1.5.0`)

`git ls-remote --tags` still reports `v1.5.0` as upstream's newest tag, so a
version-delta sync had nothing to pull. But `main` had accumulated **104
unreleased commits** past that tag (43 source files, +5737/−368), and issues
#1496–#1566 / PRs #1402–#1564 had never been triaged. "No new version" is not
"no new content" — that is the lesson worth carrying into the next sync.

Three parallel triages ran (extraction/kernel, `explore`/MCP, issues/PRs), and
every disposition below marked MEASURED was reproduced against a built binary
before being acted on. Tracked parity stays `1.5.0`: none of these releases is a
port of an upstream *release* range.

#### Shipped

| release | PR | what landed | upstream refs |
| --- | --- | --- | --- |
| `v0.43.2` | #209 | one shared file-classification signal across every surface | #1507 + the path table behind #1500 |
| `v0.43.3` | #211 | stop binding calls, imports and supertypes to coincidences | #1496, #1566, #1537/#1536 (PR #1538) |
| `v0.44.0` | #213 | unions, unit structs and COM `interface` as containers | PR #1516, #1513/PR #1514, #1519 |
| `v0.44.1` | #215 | imported members, cross-module Go calls, declaration initializers | #1518, #1521, #1510/PR #1511, #1524, #1512, #1504, #671 |
| `v0.45.0` | #217 | Tauri commands bound to their `invoke` call sites | #1543 |
| `v0.46.0` | #220 | generated files detected by their content banner | #1500, `57e0854` |
| `v0.46.1` | #223 | a named symbol always renders; real budget accounting | CG-38/36/30/28/26/31 |

`v0.43.2` also carried `h2` 0.4.16 for **RUSTSEC-2026-0258** — unrelated to the
sync, but it is why that release exists at that moment: the advisory published
mid-round and turned CI red on a lockfile nobody had touched.

#### Two agent findings REFUTED by measurement — recorded so the next sync skips them

| claim | verdict |
| --- | --- |
| `Store::nodes_by_name` lacks `ORDER BY`, so sync and rebuild can bind different same-name targets (upstream CG-33) | **ALREADY-HAVE.** Both production resolution contexts sort candidates themselves — `context.rs:111,125` and `snapshot_context.rs:118-124` call `order_candidates` / `order_candidates_pub`, keyed `(file_path, start_line, start_column, id)`: upstream's key plus an extra total-order `id` tie-break. MEASURED twice — sync and `index --force` both bind `caller.run → pct@alpha.ts` even when sync's rowid order puts `zeta` first. |
| the `explore` staleness banner can be truncated away by our own budget logic | **ALREADY-HAVE.** The banner is *prepended after* truncation: `cut_at_section_boundary` → `mark_surviving_explore_citations(kept_prefix_len)` → `with_staleness_banner`. A cut section's citation is dropped first, so the banner correctly omits it. Pinned by the pre-existing `staleness_banner_omits_a_citation_removed_from_the_final_response`. The agent had conflated the in-section inline warning with the top-level banner. |

#### One upstream premise upstream itself REJECTED — never port it

**CG-27** — treating `function`/`method` as >50% envelope containers so inner
closures rank independently. Upstream measured and rejected it
(`91cb5b4:docs/benchmarks/explore-factory-closure-cg27.md`): delivered chars
7,539 → 397, inner closures 7/11 → 0/11, ~5,200 of a 5,601 reservation unused.
The cautious variant moved nine queries by 68 → 69 definitions total. Our
envelope drop stays limited to `is_container`. The real defect it exposed —
a trivial cluster winning on density while the answer cluster is dropped whole —
is tracked separately as the Tier 5 `explore` work.

#### Divergences to record, not port

- **Self-hosted telemetry** (`49c11fc`, Cloudflare D1 + a password-gated
  dashboard replacing PostHog) and **#1509's request for a Qdrant/vector-RAG
  integration**. Both are permanent divergences: `scripts/guardrail.sh` is a CI
  gate rejecting qdrant/lancedb/candle/onnx/ort, byte-stable output is
  incompatible with a nondeterministic retrieval stage, and our indexing path
  makes no network call. Nothing vector-related is merged upstream yet — the
  stance is recorded now so it is a decision rather than a surprise. `package.json`
  in this range adds no AI dependency.

#### N/A — TypeScript/Node-only, no Rust analogue

#1532 (FTS5 absent from official Node builds — we compile it into `rusqlite`),
#1558 (V8 spread-arg limit), #1559 (V8 RegExp code space — the `regex` crate has
no such budget), #1544/#1547/#1564 (Vapor regex catastrophic backtracking —
`regex` is linear-time by construction), #1553 (V8 4 GB heap), #1549
(`git ls-files --recurse-submodules` on git <2.34 — we never call it),
#1476/#1477 (npm CVEs), #1550 (npm platform bundle).

#### NOT REPRODUCED here — closed questions, with the measurement

#1555 (C/C++ hex-blob header stall — a 360 KB header with 60,000 `0x..`
initializers indexed in **326 ms**), #1522 (16-char raw-string delimiter — both
functions extracted), #1505 (400-function anonymous namespace — all 402
extracted), #1565 (init/sync symbol-count divergence — our byte-equality is a
gated invariant). #1560 (JSX synthesis fabricating edges in a pure C/C++ index)
**cannot occur**: we have no synthesis pass at all. The flip side is that we lack
`jsx-render` edges entirely — a separate known gap.

#### Where we are AHEAD of upstream

Upstream is only now adding GDScript (#1499), which we shipped with a full
framework resolver (F1 autoload calls, F2 signal handlers, F3 impact/affected
unification) and a byte-stable golden corpus.

#### Nothing from this range is left unported

The `explore` budget/selection epic shipped as `v0.46.1`, which closes the range.
Its review took eight rounds, and the reason is worth recording: the whole-file
rule returns a complete section **before** any budget or focus logic exists, and
its line and character thresholds are independent — so a named symbol could
vanish by **two** unrelated routes. A file that passes both gates has a complete
section built and then rejected whole, where no clustering-path fix can reach it;
a file that passes the line gate but fails the character gate falls through to
clustering and is rejected there. Both produced the same **615 bytes with zero
sections** that CG-38 exists to prevent, which is why the port needed two
separate regression tests rather than one.

CG-27 is recorded above as measured-and-rejected by upstream, and is **not**
ported. The 26 DEFER items and the permanently-N/A `0682137` recorded in earlier
entries are unchanged.

The 26 DEFER items and the permanently-N/A `0682137` recorded in earlier entries
are unchanged.

### 2026-08-04 — the two open triaged issues SHIPPED, plus two project-local fixes (`v0.42.5` → `v0.42.8`; tracked parity stays `1.5.0`)

Four releases. Two of them close the issues the 2026-08-02 CLOSEOUT left open
(**#1495**, **#1482 / #1482b**); the other two are project-local — a reverted
invention and the Godot-team liaison doc's only P1. **Tracked parity stays
`1.5.0`**: none of the four is an upstream-range port. #1495 is a documented
divergence FROM upstream; #1482/#1482b close upstream-triaged ISSUES rather than
upstream commits; and `v0.42.5`/`v0.42.7` have no upstream counterpart at all. A
future sync must not read these four releases as parity movement.

Every release published with `isDraft=false` and 7 assets (6 platform binaries +
`SHA256SUMS`). Final workspace test count after `v0.42.8`: **127 suites / 3134
passed / 0 failed**.

| release | our commit | what it fixed |
| - | - | - |
| `v0.42.5` | `5aff695` (PR #190) | index root restored from `.codegraph-v2` to `.codegraph`, with takeover of a pre-existing legacy database |
| `v0.42.6` | `81628f2` (PR #192) | **#1495** Kotlin callable signatures — a DIVERGENCE from upstream |
| `v0.42.7` | `20e3f9d` (PR #194) | per-state CLI diagnostics when an index is unreadable (liaison-doc P1) |
| `v0.42.8` | `4a17919` (PR #196) | **#1482 / #1482b** TS export-alias resolution + `.js`→`.ts` specifier substitution |

**`v0.42.5` — a project-local invention reverted.** `.codegraph-v2` was never an
upstream port: upstream's `src/directory.ts:12` has always used `.codegraph`. Two
blockers had to fall together. `IndexPaths::resolve` rejected any current root
overlapping a legacy root, and the fixed `.codegraph` was always in that set, so
naming it made _every_ resolve fail. And a `.codegraph` written by v0.40.4 or by
the upstream tool has no state slots, so it classifies `Missing` while its database
exists — a state the write gate refused. An explicit `init` now takes such a
database over under the one exclusive lease it already holds, logging what it
replaced. **Only `Missing` qualifies**: `Building`, `Corrupt`, and `Future` still
fail closed, so a running builder never loses its work. `CODEGRAPH_DIR` also
returned to upstream's meaning — a plain project-local join, not a
`<name>-v2-<64 hex>` sibling.

**`v0.42.6` — #1495, and why it is a DIVERGENCE, not a port.**
`KotlinSpec::get_signature` returned `None` unconditionally because upstream's
`kotlin.ts:232-242` looks the parameter list up through a grammar FIELD the Kotlin
grammar does not expose, so upstream's own hook always returns `undefined` and this
port mirrored it faithfully. The fix reuses a pattern already in the same file:
`declared_return_type_node` walks `named_children` and matches on KIND, sidestepping
the missing field. It emits raw parameter text plus the declared return type when
written. Primary constructors stay class details; lambdas, accessors, and anonymous
functions stay outside the node set. `EXTRACTION_VERSION` 3 → 4. The new
`reference/golden/kotlin/` fixture guards five shapes (explicit, inferred, generic,
multiline nullable, extension) plus a primary-constructor negative.

**`v0.42.7` — the message was the bug, not the permission.** The liaison doc's only
P1: an interrupted `index --force` leaves the namespace in `building`, and every
read then refused with "CodeGraph not initialized" pointing at `init`. Twelve
commands shared that one message because `resolve_required_project` collapsed six of
the seven lifecycle states into a single bool. Each state now names its own remedy;
reads still fail closed everywhere. `status` adds recovery guidance for `building`
but **only when a shared lease is free** — a real build holds the namespace
exclusively, and telling someone to rebuild mid-build would destroy work in
progress. `unlock` works in `building`, clearing a stale daemon pid record and the
transient lock, then reporting that the state is still building.

**The doc's own suggestion was REFUSED, with measurements.** It proposed rolling the
newest `building` slot back to the previous `current`. But `begin_from_authorization`
publishes `phase=building` durably _before_ any destructive database work
(`crates/codegraph-store/src/rebuild.rs:324-330`), so the older slot describes a
database that no longer exists. Measured aftermath: db 4096 bytes / `nodes=0`
versus 1098/1221 intact. Presenting that as `Current` would turn every query into a
silent empty answer.

**`v0.42.8` — #1482 / #1482b, two independent misses.** `export const a = fn`,
`export { fn as a }`, and `export default fn` each publish a local declaration
under a different name, and an importer using that name resolved to nothing — so
the original looked uncalled. Re-export extraction now records local aliases
alongside the cross-file forms, and the export lookup follows them back.
Separately, `.js` specifiers never reached their TypeScript source: NodeNext
requires `import './foo.js'` even when the file is `foo.ts`, but the candidate
builder only APPENDED extensions. A specifier ending in a JS-family extension now
tries the matching TypeScript extension first (`.js`/`.jsx` → `.ts`/`.tsx`/`.d.ts`,
`.mjs` → `.mts`, `.cjs` → `.cts`), falling back to the append behaviour. When both
`foo.js` and `foo.ts` exist, TypeScript wins. Scoping is pinned by tests walking
`Language::ALL`: only TypeScript and Tsx substitute, and only the four ECMAScript
languages extract a local alias. `EXTRACTION_VERSION` 4 → 5. The new
`reference/golden/typescript/` fixture carries four positives and three negatives.

#### Exceptions carried forward unchanged

The **26 DEFER items** remain deferred — 19 resolution/store/synthesis perf
strategies plus 7 sync/WAL scheduling strategies, all tuned to upstream's Node /
`node:sqlite` runtime, byte-identical to graph output, to be re-derived from a Rust
profile rather than transcribed. **`2adc7f6` becomes a CORRECTNESS item** if
timer-triggered truncation is ever adopted here. **`0682137` stays N/A**
(packaging-rooted — we ship a native `codegraph.exe`, never a `.cmd` shim). Full
text in `## Current alignment` above.

#### Two findings worth carrying to the next sync

Both cost real investigation and would otherwise be re-derived.

1. **Python has no export semantics** — already recorded in the 2026-08-02 batch-4
   export-semantics finding above, and it stayed load-bearing here: it is what makes
   #1482's alias work TypeScript-scoped rather than generic. `find_exported_symbol`
   requires `is_exported`, and the Python spec never sets it — upstream included.
2. **The double-buffered state slots carry STATE, not DATA.** This is exactly what
   made the liaison doc's rollback suggestion unsafe, and it generalizes: any future
   "just roll back to the previous slot" idea faces the same problem, because the
   slot is a phase record, not a snapshot of the database it describes.

### 2026-08-02 — CLOSEOUT: `1.4.1 → 1.5.0` sync COMPLETE (tracked parity advanced to `1.5.0`)

All four batches of the `1.4.1 → 1.5.0` round have shipped, across codegraph-rs
`v0.42.1` → `v0.42.4`. Batch 4 was the last portable in-scope item, so by this
ledger's own rule — parity advances only when every portable, in-scope behavior in
the range has landed — tracked colby parity advances from `1.4.1` to `1.5.0`. The
bucket tables, the post-`v1.5.0` triage, and the open-issue triage live in the
AUDIT entry directly below; this entry is the batch index plus the exceptions that
a future sync actually needs.

Every release tag is `v<version>`, each published with `isDraft=false` and 7
assets (6 platform binaries + `SHA256SUMS`). Final workspace test count after
batch 4: **128 suites / 3127 passed / 0 failed**.

| batch | release | upstream | our commits | what it actually fixed |
| - | - | - | - | - |
| 1 | `v0.42.1` | `572d22b` | `88e7773` (PR #178), `148090b`, `c167412`, `374d4ff` | The Codex TOML block finder deleted trailing `[[...]]` blocks. Our `find_next_table_header` was the pre-fix logic verbatim, reproduced with the shipped 0.42.0 binary: `uninstall --target codex` silently destroyed a user's `[[mcp_servers.other.env]]` block. The fix also handled a column-0 decoy inside a `"""` string and an indented-header duplicate |
| 2 | `v0.42.2` | `f2a5df3`, `f6ac7b3`, issue #1473 | `4e18f94`, `20e07e1`, `fea40a2` (PR #181) | Three honesty defects — see below |
| 3 | `v0.42.3` | `02c0e2c` | `3fe9b55` (PR #184) | A killed session left an uncheckpointed `-wal` that SQLite kept at its high-water mark forever |
| 4 | `v0.42.4` | `38580e0` | `3860880` (PR #186) | Python class-as-value idioms produced no edges, so `callers` on a DRF serializer showed nothing |

**Batch 2 — three honesty defects.** Lookups silently substituted a different
symbol (`callers ad` answered with `add`'s rows); drifted files were sliced at
stored line numbers; blast radius claimed "no covering tests" after checking only
direct callers. Now: a non-exact lookup REFUSES (CLI non-zero pointing at
`codegraph query`; MCP `isError: true` pointing at `codegraph_search`); freshness
is fail-closed via `SourceFreshness::{ProvenFresh, PossiblyDrifted}`, comparing
size + millisecond mtime and hashing only on a stat mismatch, then emitting a
drifted file whole up to 2000 lines or omitting it entirely; coverage walks 3
caller hops under a 64-lookup budget and reports what it searched. The staleness
banner `docs/mcp.md` had promised now exists. **One audited golden movement**:
`reference/golden/mcp/codegraph_explore.json`, 4 blast-radius lines re-worded, the
rest byte-identical.

**Batch 3 — WAL residue, measured not assumed.** On `0.42.1` a 9 204 112-byte
residue stayed at exactly that size through a sync touching one line, and six
interrupted rounds ratcheted 5 702 112 → 6 439 592 → 6 760 952 bytes.
`journal_size_limit` is now set on both write-connection configuration points from
the same `wal_valve_threshold_bytes()` the valve uses — one concept, one env var
(`CODEGRAPH_WAL_VALVE_MB`) driving pragma, valve, and status warning. `sync` and
`index` fold a leftover WAL before their writer opens the DB, requiring proof the
previous daemon owner is dead AND a bounded exclusive lease, and reporting a heal
only when the fold actually completed. `status` degrades to a read-only diagnostic
on the typed `CurrentWithDatabaseSidecar` refusal, showing DB and WAL sizes without
counts and never checkpointing. Golden-neutral. Parts (c) and (e) are N/A: the only
watchdog here watches the parent pid and holds no `Store`, and every tracing sink
already emits RFC3339 via `local_timer()`.

**Batch 4 — Python class-as-value, four gates moved together.** The capture spec
did not dispatch `return_statement`; the extraction gate admitted only same-file
functions/methods; and resolution accepted only function targets at both the name
matcher and the import fast path. Python now accepts class targets at both
resolution sites through ONE shared predicate; methods stay excluded everywhere;
every other language's kind filter is untouched. `EXTRACTION_VERSION` 2 → 3. The
new `reference/golden/python/` fixture guards six positives (return, assignment
RHS, registry pair, call argument, list literal, imported class as value) and two
negatives (tuple return, bare method name).

**Batch 4 export-semantics finding — record this, it will save a future sync a
day.** The plan initially assumed the imported case resolves through the import
fast path. **It cannot.** `find_exported_symbol` requires `is_exported`
(`crates/codegraph-resolve/src/import_resolver.rs:1476`, `:1489`, `:1496`) and the
Python spec has no override, inheriting the unconditional `false` at
`crates/codegraph-extract/src/spec.rs:106` — measured zero `is_exported` Python
nodes in both the `mini` and `python` goldens. **Upstream is identical**:
`python.ts` implements no `isExported`, and `import-resolver.ts:94` does
`if (!n.isExported) continue`, so upstream's own DRF assertion also comes from the
name matcher's unique-name path. The import-side kind gate still mirrors upstream
but is DEAD for Python; its test states plainly that it hand-builds a state real
indexing cannot produce.

#### Exceptions carried out of this round

The full text is in `## Current alignment` above. In short: **26 DEFER items**
remain (19 resolution/store/synthesis perf strategies + 7 sync/WAL scheduling
strategies, all Node-runtime-tuned, byte-identical to graph output — re-derive from
a Rust profile, and treat `2adc7f6` as a correctness item if timer-triggered
truncation is ever adopted); **`0682137` is N/A** (native `.exe`, no `.cmd` shim);
and **#1482 / #1482b** (TS rename-through-alias false zeros + `.js`→`.ts` specifier
resolution) and **#1495** (Kotlin signatures always absent) were left open at the
time of writing — **both have since shipped**, in `v0.42.8` and `v0.42.6`
respectively (see the 2026-08-04 entry).

#### Process note — one root cause, four batches

Every single review rejection across all four batches reduced to the same thing in
different clothing: **an assertion whose premise does not hold.** Batch 2 needed
three F2 rounds — code claiming freshness it had not proven, a completeness claim
keyed only on a budget while errors were swallowed, and a liveness gate inferring
section survival by string-matching its own response text. Batch 3's plan took four
momus rounds plus an F2 round — a test asserting SQLite clips at
`wal_checkpoint(RESTART)` when the clip actually lands on the next resetting write,
an acceptance criterion the fix deliberately eliminates, observability that does not
exist outside the process, and a SAFETY comment claiming a mutex established a
precondition it did not once `Store::open` began reading the env var. Batch 4's plan
carried three false premises about resolver reachability, caught only because the
golden audit refused to bend the artifacts to fit the plan, then three F2 rounds on
stale comments and citations. Two mechanical rules fell out of it, both invisible to
the compiler and to every test: **widening a filter makes the comment above it
false**, and **inserting a doc section makes every line-range citation below it
false**.

### 2026-08-02 — AUDIT `1.4.1 → 1.5.0` + post-1.5.0 triage; batches 1–2 LANDED (parity stayed `1.4.1` at the time of writing — superseded by the CLOSEOUT entry above)

A full audit of the `1.4.1 → 1.5.0` range plus the 6 unreleased commits above
`v1.5.0`, and a triage of colby's open issues. **Tracked parity deliberately does
NOT advance**: by this ledger's own definition — "all portable in-scope behavior
landed" — two portable items from the 1.5.0 range and four from the issue triage
are still open. Parity advances only when they land or are re-classified with
evidence.

**A bookkeeping defect this audit found and is correcting:** PR #173
(`5d795a5`, released in `v0.41.0`) landed a large slice of the 1.5.0 range with a
10 070-line evidence ledger at `docs/upstream-sync/V1_5_PORTABLE_FIXES.md`, but
**never recorded a disposition here**. The sync happened; the ledger entry did
not. That is exactly the failure this file exists to prevent — the next sync would
have re-investigated all 86 commits from scratch.

#### `1.4.1 → 1.5.0`: 86 commits, bucketed (11 + 2 + 47 + 26 = 86)

| bucket | count | notes |
| - | - | - |
| **COVERED** by a PR #173 batch | 11 | C++ operator calls / template-arg stripping / namespace prefixing (B1-B3), C attribute-macro blanking (B4), imported-singleton + literal-receiver resolution (C1-C2), row-id ref cleanup (C3), `node -f` source body (A4), multi-hump field queries (A2), plus `includeIgnored` and directory-delete watch semantics verified equivalent |
| **NOT COVERED — portable** | 2 | `41c2029` Go field-chain validated type inference (no Go receiver inference in `codegraph-resolve`); `d6efd43` `NO_COLOR` / `--no-color` / piped-stdout policy (no such flag in `codegraph-cli`, though the CLI has **no colouring dependency at all**, so this may be "behavior naturally satisfied, flag absent" rather than a defect — verify before implementing) |
| **NOT COVERED — N/A** | 47 | 21 docs/CI/chore/release; **10 `R7b <lang> walker`** commits that vendor tree-sitter grammars as C — violates this project's no-vendored-grammar invariant, and all 11 of those languages are already supported natively via crates.io grammars; 8 R1-R7a Node→napi-rs kernel migration; 8 upstream-only product/UI surfaces (`clack` UI, `node:sqlite` warnings, CodeGraph Pro signup, npm OIDC/provenance, Go route framework resolver we do not have) |
| **NOT COVERED — DEFER** | 26 | 19 resolution/store/synthesis perf策略 + 7 sync/WAL scheduling策略, all tuned to upstream's Node/`node:sqlite` runtime (pool sizing, cgroup cache credit, macOS `vm_stat` budgets). Byte-identical to graph output, so not parity blockers. Re-derive from a Rust profile before porting; `2adc7f6` (WAL truncate only at parked barriers) becomes a CORRECTNESS item if we ever adopt timer-triggered truncation |

R7b languages verified already present in `crates/codegraph-extract/src/lang/`:
Dart, Scala, Lua, Luau, R, Kotlin, Swift, PHP, Ruby, C#, Rust — 11 of 29 total
language extractors.

#### Post-`v1.5.0` (6 unreleased upstream commits)

| sha | disposition | rationale |
| - | - | - |
| `572d22b` TOML block finder preserves trailing `[[...]]` | **PORTED** — this batch, `88e7773` | Our `find_next_table_header` was the pre-fix logic verbatim, and reproduced with the shipped 0.42.0 binary: `uninstall --target codex` silently deleted a user's `[[mcp_servers.other.env]]` block |
| `f2a5df3` never serve a mis-sliced body from a drifted file | **PORTED** — batch 2, `v0.42.2` | Our exposure was WORSE than upstream's: `read_project_source` sliced current bytes at indexed lines with no freshness check, and the staleness banner promised at `instructions.rs:63` had **no implementation anywhere** |
| `f6ac7b3` blast radius follows caller chains | **PORTED** — batch 2, `v0.42.2` | `CALL_DEPTH = 1` plus an unconditional `"; ⚠️ no covering tests found"`; `codegraph affected` already BFSed correctly, the MCP renderer did not |
| `02c0e2c` bound the WAL after a killed session | **PORTED** — batch 3, `v0.42.3` | No `journal_size_limit` on any write connection, and no heal-at-open at the CLI mutation entry points. The audit's belief that the #1231 valve was bulk-index-only was WRONG and batch 3 corrected it: the valve already fired from three write paths — full index (`crates/codegraph-cli/src/main.rs:4765,4776`), the resolution tail (`crates/codegraph-resolve/src/resolver.rs:1527`), and migration (`crates/codegraph-watch/src/migrate.rs:156,161`) — so it did bound WAL growth across long write phases. What it never covered is the residue a KILLED session leaves behind: no path folded a WAL orphaned by a process that died mid-write, and nothing reclaimed its high-water mark. Part (c) (watchdog `progressPaths`) is N/A because the only watchdog here is the proxy's parent-pid watcher, which just sets a shutdown flag and holds no `Store` (`crates/codegraph-daemon/src/proxy.rs:575-599`), so there is no `progressPaths` to extend. Part (e) (daemon.log ISO timestamps) is N/A because `local_timer()` already emits RFC3339 with a UTC fallback on every tracing sink (`crates/codegraph-core/src/logger.rs:93-118`) |
| `38580e0` Python bare class refs → `references` edges | **PORTED** — batch 4, `v0.42.4` (isolated) | All four gates were pre-fix. The ONLY item in this round that moves `reference/golden/`; land it alone so a golden diff has exactly one possible cause |
| `0682137` Claude prompt hook as `codegraph.cmd` | **N/A (packaging-rooted)** | We ship a native `codegraph.exe` (`install.ps1:34,144-150`), never a `.cmd` shim; Git Bash resolves the bare name to `.exe` natively, so writing `codegraph.cmd` would name a nonexistent file |

#### colby open-issue triage — 4 actionable, verified against our code

- **#1473** callers/callees/impact silently answer for a DIFFERENT symbol —
  **reproduced on CLI AND MCP**. `exact_or_top_matches` falls back to
  `matches.first()` with no note, and `--strict` only fires on an EMPTY result, so
  a substituted non-empty answer passes it. Highest-value: fails toward confident
  wrongness, and it is the default. → batch 2, **LANDED in `v0.42.2`**.
- **#1482 / #1482b** TS rename-through-alias loses caller/impact edges
  (`export const a = fn`, `export { fn as a }`, `export default fn` all produce a
  false zero), and `.js` specifiers never resolve to `.ts` because
  `extension_resolution` only APPENDS. Golden-affecting. → **LANDED in `v0.42.8`**
  (`4a17919`, PR #196).
- **#1495** Kotlin signatures always absent — our `get_signature` deliberately
  mirrors upstream's `undefined` with `None`; the fix pattern already exists ten
  lines away in the same file. Cheap, no Kotlin golden exists yet. → **LANDED in
  `v0.42.6`** (`81628f2`, PR #192), as a documented divergence from upstream.
- **ALREADY-HAVE, verified**: #1455 (exact-name ranking shipped in `v0.42.0` — but
  it changed `search` RANKING, so it does NOT close #1473's separate resolution
  path), #1451 (our sync gate is content-hash, no atime anywhere), #1447/#1445
  (full Godot resolver + golden fixtures — we lead upstream here), #1464 (Qoder
  installer target), #1441 (javadoc already in `node` output).
- **TS-only**: #1443 (our binary is not inside a node-manager shim tree), #1465
  (our MCP engine is request-scoped on `spawn_blocking`, not one event loop),
  #1454 (our prompt hook walks UP only — N/A by absence), #1461 (no Spring route
  extraction exists).

#### Batch 1 — LANDED (`88e7773`)

Ports `572d22b`, plus two further defects that hands-on QA surfaced in the same
function. Both were **silent user-data loss**, and the second was made reachable
by the first fix:

- a col-0 `[mcp_servers.codegraph]` inside a user's `"""` was accepted as our
  header, so the end scan started mid-string and deleted to EOF — 249 bytes → 167,
  unterminated string, exit 0 with "Updated", on a bare `install --yes`;
- an indented header was not matched at all, so install appended a duplicate and
  the config stopped parsing (`duplicate key codegraph in table mcp_servers`) —
  pre-existing, reproduced identically with the shipped 0.42.0.

`make ci` 128 suites / 3097 passed / 0 failed (+26 tests), `reference/` untouched,
scope confined to `crates/codegraph-cli/`. Four Final-Wave gates APPROVED; F3 ran
a ~60-fixture matrix over three rounds and is what found both extra defects.

#### Batch 2 — LANDED (`v0.42.2`: `4e18f94`, `20e07e1`, `fea40a2`; PR #181)

Ports `f2a5df3` and `f6ac7b3` from the `1.5.0` range plus issue #1473 from the
triage above. Each was an **honesty** defect — the graph asserting something it
had not established:

- **#1473 silent symbol substitution.** A lookup matching no exact symbol fell
  through to the top-ranked fuzzy result, so `callers ad` answered with rows
  belonging to `add`. CLI lookup commands now exit non-zero pointing at
  `codegraph query`; MCP returns `isError: true` pointing at `codegraph_search`.
  Exact matching was widened to `qualified_name` and Godot resource paths so the
  supported dotted-name and `.tscn` lookups survive. Decision A1 chose REFUSE over
  annotate; A2 froze `--strict` (its 5 consumers are byte-unchanged).
- **`f2a5df3` drifted source served at stored line numbers**, returning a different
  symbol's body. A request-memoized probe compares size + millisecond mtime first
  and hashes only on a stat mismatch (decision B1). A drifted file is emitted
  whole up to `FILE_MODE_MAX_LINES = 2000` or omitted with a notice — never
  sliced (B2). `docs/mcp.md` had documented a staleness banner that nothing
  produced; B3 made it real. Case 3 of the issue was deliberately NOT implemented.
- **`f6ac7b3` unmeasured coverage denial.** Blast radius claimed "no covering tests"
  after inspecting direct callers only. It now walks callers through hop 3 under a
  64-lookup budget with an uncapped frontier and a 2-file display cap (C1), and
  reports what it actually searched.

**Five review-found defects in the fixes themselves**, all in `codegraph-mcp`:
freshness defaulted to fresh when the `FileRecord` was missing / the query failed
/ the read failed (reachable — `sync` deletes the file record before upserting
nodes and re-inserts it last, so a concurrent reader sees nodes whose file has no
record); the oversized branch compared `size` alone, so a same-size rewrite read
as fresh; a swallowed `get_callers` error still produced the strong three-hop
claim; a duplicated whole-file-fits predicate diverged from the rendering decision
at the trailing-newline boundary, banner-listing a 2000-line file whose source was
omitted; and the first liveness gate inferred section survival by matching
`#### {path} —` in the response text — source bytes as a control channel,
spoofable by a marker literal in a retained file (this repo's own test source
contains one). Freshness is now fail-closed via
`SourceFreshness::{ProvenFresh, PossiblyDrifted}`, and banner liveness comes from
`cut_at_section_boundary`'s surviving-prefix length plus per-section offsets.

`make ci` 128 suites / 3110 passed / 0 failed. Each fix RED-proven by isolated
revert. `cut_at_section_boundary`'s new `(String, usize)` signature is
byte-identical to the old output (200k randomized inputs, 0 mismatches); the
section-offset formula and its `<=` predicate likewise (200k combinations, 0
errors). **One audited golden movement**:
`reference/golden/mcp/codegraph_explore.json`, 4 blast-radius lines re-worded
(`⚠️ no covering tests found` → `no tests found within 3 caller hops`), the rest
byte-identical. `instructions.rs`, `initialize.json`, `codegraph-store/`, and
`codegraph-core/` untouched. Four Final-Wave gates APPROVED — F2 took three rounds
and is what found all five extra defects; F3 hands-on QA covered CLI and real MCP
stdio across six drift scenarios (and established that MCP drift cases need
`--no-watch`, since a background re-index legitimately clears the drift).

**Parity did not advance at this point.** Batch 3 (`02c0e2c` WAL
`journal_size_limit` + heal-at-open) and batch 4 (`38580e0` Python bare class refs
— the only golden-moving item, to land isolated) were still open; both have since
shipped, and the CLOSEOUT entry above advances parity to `1.5.0`.

### 2026-07-17 — CLOSEOUT: 1.2.0 → 1.4.1 sync COMPLETE (tracked parity advanced to 1.4.1)

The `1.2.0 → 1.4.1` sync program evaluated on 2026-07-10 is now finished. Every
chosen PORT item landed, the DEFER phase ran to completion across nine
releases, and the two structurally-blocked languages (COBOL, VB.NET) are
recorded as permanently deferred. Tracked colby parity advances from `1.2.0` to
`1.4.1`. This entry is the compact index; each release has its own detailed
LANDED entry below (and Release D/E have their own dated entries further down)
— read those for the per-item disposition tables.

**Full release table, v0.28.3 → v0.39.0:**

| Release | Item(s) shipped |
| --- | --- |
| `v0.28.3` | fix(cli): cold-start MCP handshake race (pre-program fix, not part of the chosen subset) |
| `v0.28.4` | fix(ci): release CI gate + de-flake reopen test (pre-program fix, not part of the chosen subset) |
| `v0.29.1` | Release A — #1187 orphaned-ref heal-on-sync, #1200/#1185 daemon lifecycle backstops, #1231 WAL-valve (bulk-index half) |
| `v0.30.0` | Release B — #1063 `codegraph.json` `include` list |
| `v0.30.1` | Release C — #1220 PHP `$this->prop->method()` declared-type resolution |
| `v0.31.0` | Release D — C++ namespace-qualified names + template-arg call linking, #1158/#1159/#1133 UE reflection-macro recovery + `.h` C-vs-C++ detection, #1124 Lua/Luau annotation self-match gate |
| `v0.31.1` | DEFER E — #1212 bounded-memory resolution tail: large-graph streaming fallback + WAL valve extended into the resolution write loop (most of #1212 was N/A for Rust; only these two sub-parts were portable) |
| `v0.32.0` | DEFER F — prompt-hook confidence-tiered gate: #1126 multilingual structural-keyword gate, #1138 stem-bounding, #1136 confidence-tier mechanics (query-time vocab substitution, no schema change), #1144/#1145/#1146 segment-match integrity — **all telemetry excluded** |
| `v0.33.0` | DEFER G — Metal (`.metal`) + CUDA (`.cu`/`.cuh`) via the existing `tree-sitter-cpp` grammar (no new `Language` variant) |
| `v0.34.0` | DEFER H1 — ArkTS (`.ets`) via `tree-sitter-arkts`; ArkUI dispatch/callback-synthesis bridges deferred |
| `v0.35.0` | DEFER H2 — Solidity (`.sol`) via `tree-sitter-solidity`; fully self-contained, nothing deferred |
| `v0.36.0` | DEFER H3 — Nix (`.nix`) via `tree-sitter-nix`; module-system/callback-synthesizer bridges deferred |
| `v0.37.0` | DEFER H4 — Terraform/HCL (`.tf`/`.tfvars`/`.tofu`) via `tree-sitter-hcl`; module-boundary `TerraformResolver` deferred |
| `v0.38.0` | DEFER H5 — Erlang (`.erl`/`.hrl`) via `tree-sitter-erlang`; OTP behaviour/gen_server/spawn-MFA bridges deferred |
| `v0.39.0` | DEFER H6 — CFML/ColdFusion (`.cfc`/`.cfm`/`.cfs`) via dual-grammar `tree-sitter-cfml`; `<cfscript>`-in-tag delegation, cfquery, and framework/receiver-type resolvers deferred |

**Permanently deferred (H7/H8) — no crates.io grammar available:**

| Item | Reason |
| --- | --- |
| COBOL (`.cbl`/`.cob`/`.cpy`) | `tree-sitter-cobol` on crates.io is a name-squat stub (768 bytes, `println!("to come!")`, no real grammar, no tree-sitter dependency). Adding COBOL would require vendoring a git grammar, which violates this project's no-vendored-grammar invariant. Deferred until a real crate ships. |
| VB.NET (`.vb`) | No crate exists on crates.io at all — `tree-sitter-vbnet`/`-visual-basic`/`-vb` are absent; the only near-match, `tree-sitter-vb6`, is VB6, not VB.NET. Same no-vendor blocker. Deferred. |

**Parity caveats (see `## Current alignment` above for the full text):**
framework-resolution bridges deferred per-language (ArkUI, Nix module-system,
Terraform module-boundary, Erlang OTP, CFML receiver-type); COBOL/VB.NET
permanently deferred for lack of a crates.io grammar; prompt-hook telemetry
excluded by policy; TS/Node-runtime-only items recorded as N/A. Everything else
portable and in-scope for `1.2.0 → 1.4.1` has landed.

### 2026-07-17 — LANDED DEFER item H6: CFML / ColdFusion (`.cfc`/`.cfm`/`.cfs`, #1153 `816bacb`) via the dual-grammar `tree-sitter-cfml` crate — the EXTRACTION SLICE only, scope B (`<cfscript>`-in-tag delegation + cfquery + framework/receiver-type resolvers deferred)

Landed DEFER item H6 — the **scope-B extraction slice** of the upstream
`816bacb` ("feat(extraction): add CFML/ColdFusion language support (#1153)").
CFML is a NEW `Language::Cfml` variant backed by the DUAL-GRAMMAR
`tree-sitter-cfml = "0.26.30"` crate (crates.io; bundles a `cfscript` script
grammar + a `cfml` tag grammar + a `cfquery` SQL dialect; `LanguageFn` ABI — no
vendored grammar/wasm). CFML has two syntaxes: a first-token sniff
(`is_bare_script_cfml`, BOM+ws+comment skip → first token `!= '<'`) picks the
dialect+grammar PER FILE. This is expressed through a new defaulted
`LanguageSpec::tree_sitter_language_for_source(&self, source)` trait hook
(default = `self.tree_sitter_language()`); only `CfmlSpec` overrides it, so all
41 other specs are byte-identical. Bare-script files (`.cfs` + bare `.cfc`/`.cfm`)
parse with `cfscript` and drive the generic type-set dispatch; tag files parse
with the `cfml` tag grammar and are handled by the `Language::Cfml`-guarded
`visit_cfml_node` walker extension.

| Upstream hunk | Disposition | Notes |
| --- | --- | --- |
| `types.ts`: `'cfml'`/`'cfscript'`/`'cfquery'` in `LANGUAGES` | **PORT (one variant)** | ONE `Language::Cfml` maps all three extensions; `cfscript`/`cfquery` are internal grammar handles. `Language::ALL` 41→42, `LANGUAGE_STRINGS` 41→42 in lockstep |
| `grammars.ts`: `.cfc`/`.cfm`→cfml, `.cfs`→cfscript EXTENSION_MAP + display | **PORT** | `"cfc" \| "cfm" \| "cfs" => Language::Cfml` in `builtin_language_for_ext`; `as_str` `"cfml"` |
| `grammars.ts`: 3 vendored wasm + ABI-15 branch | **N/A** | port pins `tree-sitter-cfml` from crates.io — no vendored wasm |
| `cfml-extractor.ts`: `isBareScriptCfml` dialect switch | **PORT** | `is_bare_script_cfml` first-token detector; drives `tree_sitter_language_for_source` + `visit_cfml_node` dialect gate |
| `cfml-extractor.ts`: `extractBareScript` (delegate to cfscript, file-name component) | **PORT** | cfscript type-set config drives generic dispatch; `emit_cfml_script_component` renames the unnamed `component`/`interface` from the file + script-style `extends`→Extends |
| `cfml-extractor.ts`: `extractTagBased` `walkProgram`/`extractComponent`/`extractFunctionTag`/`tagAttr` | **PORT** | `visit_cfml_node` tag walk: `cf_component_open_tag`→Class (name attr/file, `extends`/`implements` refs, following-sibling body via `cfml_consumed_until`), `cf_function_tag`→Method/Function |
| `cfscript.ts`: `getVisibility`/`getSignature`/`extractImport`/`classifyClassNode` | **PORT** | `CfmlSpec` overrides (visibility from `access_type`, signature from `parameters`, imports, interface-vs-class) |
| `cfml-extractor.ts`: `delegateScriptTag` (`<cfscript>` body re-parse) | **DEFER** | needs a 2nd in-tree parse the one-spec-one-grammar engine lacks |
| `cfml-extractor.ts`: `delegateQueryTag` / `cfquery.ts` (`LANGUAGE_CFQUERY`) | **DEFER** | cfquery SQL-body extraction — same 2nd-parse limitation |
| CFML framework resolver (#1152/#1154/#1155: dotted/relative inheritance, receiver-type inference) | **DEFER** | no 2nd `FrameworkResolver`; port keeps its single `GodotResolver` |
| `__tests__/extraction.test.ts` (+308) | **DEFER** | re-implemented as Rust unit + golden tests |

Golden impact: `.schema` byte-stable (`language` is a stored TEXT VALUE, not DDL —
`schema_parity` green, `colby.schema.sql` byte-identical); the eleven existing
goldens (`cpp`/`godot`/`ruby`/`mini`/`metal`/`cuda`/`arkts`/`solidity`/`nix`/`terraform`/`erlang`)
byte-neutral (none holds a `.cfc`/`.cfm`/`.cfs` file, no regen); one NEW `cfml`
golden (`reference/golden/cfml/`, corpus `crates/codegraph-bench/fixtures/cfml/`
— script `Base.cfc`, tag `Widget.cfm`, bare-script `Gadget.cfs`) carries the only
`"language":"cfml"` string. Both `extends Base` refs RESOLVE to `Base.cfc` as
EDGES; `Gadget.doThing`'s `helper()` call is the sole unresolved `refs.json` row.
The DEFERRED `<cfscript>`-in-tag / cfquery / framework-resolver edges are never
emitted. Extraction-tier add matching the port's other 28 grammar-backed Tier-1
languages (same discipline as ArkTS H1 / Solidity H2 / Nix H3 / Terraform H4 /
Erlang H5).

Tracked colby parity stays `1.2.0` (advances to `1.4.1` only once the full
`1.2.0..1.4.1` PORT/DEFER subset lands — this is one item of that subset).

### 2026-07-16 — LANDED DEFER item H5: Erlang (`.erl`/`.hrl`, #1165 `6511722`) via a dedicated `tree-sitter-erlang` grammar — the EXTRACTION SLICE only (behaviour/gen_server/spawn-MFA framework bridges deferred)

Landed DEFER item H5 — the **extraction slice ONLY** of the upstream `6511722`
("feat(extraction): add Erlang language support with OTP behaviour/gen_server
bridging (#1165)"). Erlang is a NEW `Language::Erlang` variant backed by a NEW
grammar crate (`tree-sitter-erlang = "0.19.0"`, crates.io, WhatsApp/ELP grammar;
`LanguageFn` ABI — no vendored grammar). Erlang is form-based (a function's name
lives on its `function_clause`, the grammar emits one `fun_decl` per clause,
`record_decl` carries fields as direct children, `-spec`/`-callback`/type bodies
parse as `call` nodes), so `ERLANG_SPEC` has all-empty C-family type-sets (only
`package_types`/`import_types` wired, as upstream) and the extraction is driven
entirely by a `Language::Erlang`-guarded `visit_erlang_node` walker extension.

| Upstream hunk | Disposition | Notes |
| --- | --- | --- |
| `types.ts`: `'erlang'` in `LANGUAGES` | **PORT** | new `Language::Erlang` variant; `Language::ALL` 40→41, `LANGUAGE_STRINGS` 40→41 in lockstep |
| `grammars.ts`: `.erl`/`.hrl` EXTENSION_MAP + `Erlang` display | **PORT** | `"erl" \| "hrl" => Language::Erlang` in `builtin_language_for_ext` |
| `erlang.ts` `erlangExtractor` config (`fun_decl`/`record_decl`/`type_alias`/`opaque`/`import_attribute`/`pp_include(_lib)`, `packageTypes: module_attribute`, name/body/params fields) | **PORT** | `ERLANG_SPEC` wires `package_types`+`import_types`; the C-family type-sets stay empty (form-based) |
| `erlang.ts` `handleFunDecl` clause-merge dedup | **PORT** | `visit_erlang_fun_decl` tracks `(last_fn_name, last_fn_id)`; a same-name continuation `fun_decl` attaches to the existing node |
| `erlang.ts` `handleRecordDecl` (struct + direct-child fields) | **PORT** | `visit_erlang_record_decl` |
| `erlang.ts` `handleTypeAlias`/`handlePpDefine` | **PORT** | TypeAlias / Constant, type-position `call` children NOT descended |
| `erlang.ts` `spec`/`callback` type-position guard | **PORT** | both consumed without descent — `-spec f(integer())->integer().` mints no `integer` call ref |
| `tree-sitter.ts` erlang `extractCall`: local `call`→calls, remote `mod:f`→`mod::f`, `?MODULE:f`→bare | **PORT** | `erlang_call_ref_name` (reads module qualifier from the PARENT `remote` node) |
| `tree-sitter.ts` `internal_fun`/`external_fun`→references, record-expr forms→references | **PORT** | function VALUES + record USAGES are `References`, not `Calls` |
| `erlang.ts` `handleBehaviour` (`-behaviour(x)`→implements) | **DEFER** | non-Godot framework resolution; the port has one concrete `FrameworkResolver` (`GodotResolver`) |
| `tree-sitter.ts` gen_server `call/cast(?MODULE\|?SERVER)`→`handle_call`/`handle_cast` (`resolveErlangGenServerTarget`) | **DEFER** | framework bridge |
| `tree-sitter.ts` spawn/apply/proc_lib/timer/rpc MFA-argument callee lift (`ERLANG_MFA_CALLS`) | **DEFER** | framework bridge |
| `tree-sitter.ts` `macro_call_expr` use-site→`-define` constant linking + `ERLANG_PREDEFINED_MACROS` | **DEFER** | macro use-site call chain; the `-define` Constant symbol is still emitted |
| `erlang.ts` `handleAppResourceTuple` (`.app`/`.app.src` `{application,...}` wiring) | **DEFER** | framework/resource resolution |
| `__tests__/*.test.ts` (resolver + extraction) | **DEFER** | re-implemented as Rust unit + golden tests |

Golden impact: `.schema` byte-stable (`language` is a stored TEXT VALUE, not DDL —
`schema_parity` green, `colby.schema.sql` byte-identical); the ten existing
goldens (`cpp`/`godot`/`ruby`/`mini`/`metal`/`cuda`/`arkts`/`solidity`/`nix`/`terraform`)
byte-neutral (none holds a `.erl`/`.hrl` file, no regen); one NEW `erlang` golden
(`reference/golden/erlang/`, corpus `crates/codegraph-bench/fixtures/erlang/m.erl`)
carries the only `"language":"erlang"` string. Its local `g()` self-call and the
`foo.hrl` include resolve as EDGES; the remote `other::h` (the `other` module is
absent from the fixture) is the sole unresolved `refs.json` row; the DEFERRED
behaviour/gen_server/spawn-MFA/`.app` edges are never emitted. This is an
extraction-tier add matching the port's other 27 grammar-backed Tier-1 languages
(same disposition as ArkTS H1 / Nix H3 / Terraform H4 deferred bridges).

Tracked colby parity stays `1.2.0` (advances to `1.4.1` only once the full
`1.2.0..1.4.1` PORT/DEFER subset lands — this is one item of that subset).

### 2026-07-15 — LANDED DEFER item H4: Terraform/HCL (`.tf`/`.tfvars`/`.tofu`, #1173 `6c24f4b`) via a dedicated `tree-sitter-hcl` grammar — the EXTRACTION SLICE only (module-boundary framework resolver deferred)

Landed DEFER item H4 — the **extraction slice ONLY** of the upstream `6c24f4b`
("feat(extraction): add Terraform/OpenTofu language support with module-boundary
bridging (#83, #310, #648 — carries #706) (#1173)"). Terraform is a NEW
`Language::Terraform` variant backed by a NEW grammar crate
(`tree-sitter-hcl = "1.1.0"`, crates.io, Apache-2.0; `LanguageFn` ABI — no
vendored `tree-sitter-terraform.wasm`). HCL is intentionally generic — every
top-level construct is a `block` distinguished only by its first `identifier`
child — so there are no C-family type-set node kinds; `TERRAFORM_SPEC` has
all-empty type-sets (faithful to upstream's empty `terraformExtractor` config) and
the extraction is driven entirely by a `Language::Terraform`-guarded
`visit_terraform_node` walker extension (the same custom-visitor pattern as
`visit_nix_node`). Per-item disposition:

| upstream (`6c24f4b`)                                                                                                              | disposition | notes                                                                                                                                              |
| --------------------------------------------------------------------------------------------------------------------------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `types.ts`: `'terraform'` in `LANGUAGES`                                                                                          | **PORT**    | new `Language::Terraform` variant; `Language::ALL` 39→40, `LANGUAGE_STRINGS` 39→40 in lockstep                                                      |
| `grammars.ts`: `.tf`/`.tfvars`/`.tofu` EXTENSION_MAP + `Terraform` display                                                        | **PORT**    | `"tf" \| "tfvars" \| "tofu" => Language::Terraform` in `builtin_language_for_ext`                                                                   |
| `grammars.ts`: vendored `tree-sitter-terraform.wasm` ABI-15 branch                                                                | **N/A**     | port pins `tree-sitter-hcl = "1.1.0"` from crates.io — no vendored wasm                                                                             |
| `languages/index.ts`: register `terraformExtractor`                                                                               | **PORT**    | `TERRAFORM_SPEC` (empty type-sets) + `spec_for_language` arm + `parse_language` (`queries.rs`)                                                      |
| `terraform.ts` `visitNode`: block-type dispatch resource/data→class, module→module, variable/output→variable, provider→namespace, locals→constant; plain traversal refs | **PORT**    | `visit_terraform_node` + `visit_terraform_locals` + `emit_terraform_references_in_body` + the pure `pub(crate)` AST helpers in `lang/terraform.rs`  |
| `terraform.ts` `emitModuleWiring`: `module.M:file`/`:var.X`/`:output.X` `:`-scoped refs                                            | **DEFER**   | feeds the deferred `TerraformResolver`; a `module` block still emits its Module symbol + plain body refs                                            |
| `terraform.ts` `.tfvars` top-level-assignment `var.X` ref                                                                         | **DEFER**   | feeds the deferred `.tfvars` ancestor-walk; `.tfvars` files still index as `Language::Terraform` file nodes                                          |
| `terraform.ts` `qualifyReference` `module.M:output.<out>` scoped half                                                             | **DEFER**   | plain `module.M` PORTED; the `:output` scoped half feeds the deferred resolver                                                                      |
| `resolution/frameworks/terraform.ts` (+195): `TerraformResolver` (directory-scoped resolution, `:`-scoped bridge, `.tfvars` walk)  | **DEFER**   | the port has exactly one concrete `FrameworkResolver` (`GodotResolver`); a second is out of scope for the extraction-tier program                  |
| `resolution/frameworks/index.ts` (+3): register `terraformResolver`                                                               | **DEFER**   | nothing to register                                                                                                                                |
| `resolution/index.ts` (+6): terraform directory-scoping gate                                                                      | **DEFER**   | resolution-tier gate                                                                                                                                |
| `__tests__/frameworks-integration.test.ts` (+131), `__tests__/extraction.test.ts` (+290)                                          | **DEFER**   | resolver tests / re-implemented as Rust unit + golden tests                                                                                          |

Golden impact: `.schema` byte-stable (`language` is a stored TEXT VALUE, not DDL —
`schema_parity` green, `colby.schema.sql` byte-identical); the nine existing
goldens (`cpp`/`godot`/`ruby`/`mini`/`metal`/`cuda`/`arkts`/`solidity`/`nix`)
byte-neutral (none holds a `.tf`/`.tfvars`/`.tofu` file, no regen); one NEW
`terraform` golden (`reference/golden/terraform/`, corpus
`crates/codegraph-bench/fixtures/terraform/main.tf`) carries the only
`"language":"terraform"` string in the corpus. Its plain traversal refs with a
unique same-file target resolve via the existing generic qualified-name matcher
(`var.region` ×3 → the variable, `aws_s3_bucket.b` → the resource, `module.vpc` →
the module — all EDGES), leaving the undeclared `aws_kms_key.logs` as the sole
unresolved `refs.json` row; the DEFERRED `emitModuleWiring` `:`-scoped refs and
`.tfvars` var ref are never emitted. This is an extraction-tier add matching the
port's other 26 grammar-backed Tier-1 languages (same disposition as ArkTS H1's
deferred ArkUI bridges + Nix H3's module-system synthesizer).

Tracked colby parity stays `1.2.0` (advances to `1.4.1` only once the full
`1.2.0..1.4.1` PORT/DEFER subset lands — this is one item of that subset).

### 2026-07-15 — LANDED DEFER item H3: Nix (`.nix`, #1190 `7f32513`) via a dedicated `tree-sitter-nix` grammar — the EXTRACTION SLICE only (module-system bridges deferred)

Landed DEFER item H3 — the **extraction slice ONLY** of the upstream `7f32513`
("feat(extraction): add Nix language support with module-system option wiring").
Nix is a NEW `Language::Nix` variant backed by a NEW grammar crate
(`tree-sitter-nix = "0.3.0"`, crates.io, MIT; `LanguageFn` ABI — no vendored
wasm). Because Nix is an expression language with no C-family
`class`/`struct`/`method`/`enum` node kinds, `NIX_SPEC` has all-empty type-sets
(faithful to upstream's empty `nixExtractor` config) and the extraction is driven
entirely by a `Language::Nix`-guarded `visit_nix_node` walker extension (the same
custom-visitor pattern as `visit_gdscript_node`). Per-item disposition:

| upstream (`7f32513`)                                                                                          | disposition | notes                                                                                                            |
| ------------------------------------------------------------------------------------------------------------- | ----------- | ---------------------------------------------------------------------------------------------------------------- |
| `types.ts`: `'nix'` in `LANGUAGES` (after `'solidity'`)                                                       | **PORT**    | new `Language::Nix` variant; `Language::ALL` 38→39, `LANGUAGE_STRINGS` 38→39 in lockstep                        |
| `grammars.ts`: `'.nix': 'nix'` EXTENSION_MAP + `Nix` display                                                  | **PORT**    | `"nix" => Language::Nix` in `builtin_language_for_ext`                                                            |
| `languages/index.ts`: register `nixExtractor`                                                                 | **PORT**    | `NIX_SPEC` (empty type-sets) + `spec_for_language` arm + `parse_language` (`queries.rs`)                          |
| `nix.ts` `visitNode`: `binding`→function\|variable, curried lambda signature, `inherit`, `apply`→calls/imports, `import`/`callPackage`/`imports`-list paths | **PORT**    | `visit_nix_node` + `emit_nix_file_import` + the pure `pub(crate)` AST helpers in `lang/nix.rs`                    |
| `resolution/callback-synthesizer.ts` (+178)                                                                   | **DEFER**   | the port has no callback-synthesis subsystem                                                                     |
| `resolution/index.ts` (+31): lexical-scope resolution gates                                                   | **DEFER**   | Nix `let`/`with`/`rec` scope-aware resolution — resolution tier                                                 |
| `resolution/import-resolver.ts` (+34): module-list wiring                                                     | **DEFER**   | binds `imports`/`modules` path refs; the EXTRACTION emits them, the RESOLVER wiring is deferred                  |
| `db/queries.ts` (+15) `getNodesByNamePrefix` + `mcp/tools.ts` (+31): option-path lookup                       | **DEFER**   | query surface feeding the deferred option-path synthesizer                                                       |
| `src/index.ts` (+5): module-lists glue; `__tests__/nix-option-synthesizer.test.ts`                            | **DEFER**   | synthesizer glue + its tests                                                                                      |

Golden impact: `.schema` byte-stable (`language` is a stored TEXT VALUE, not DDL —
`schema_parity` green, `colby.schema.sql` byte-identical); the eight existing
goldens (`cpp`/`godot`/`ruby`/`mini`/`metal`/`cuda`/`arkts`/`solidity`)
byte-neutral (none holds a `.nix` file, no regen); one NEW `nix` golden
(`reference/golden/nix/`, corpus `crates/codegraph-bench/fixtures/nix/`) carries
the only `"language":"nix"` string in the corpus. Its `imports`/`callPackage`
path refs resolve in-corpus (the referenced `.nix` files exist), so `refs.json`
retains only the three unresolved `Calls` refs — the DEFERRED module-system
synthesizer / import-resolver wiring binds nothing new. This is an extraction-tier
add matching the port's other 25 grammar-backed Tier-1 languages (same disposition
as ArkTS H1's deferred ArkUI bridges).

Tracked colby parity stays `1.2.0` (advances to `1.4.1` only once the full
`1.2.0..1.4.1` PORT/DEFER subset lands — this is one item of that subset).

### 2026-07-15 — LANDED DEFER item H2: Solidity (`.sol`, #1170 `1441933`) via a dedicated `tree-sitter-solidity` grammar — the WHOLE commit (all extraction; nothing deferred)

Landed DEFER item H2 — the FULL upstream `1441933` ("feat(extraction): add
Solidity language support (.sol)"). Unlike H1 (ArkTS, which deferred a
callback-synthesizer slice), the whole Solidity commit is EXTRACTION — there is
no bundled framework-resolution / runtime-dispatch layer, so **nothing is
deferred for H2**. Solidity is a NEW `Language::Solidity` variant backed by a NEW
grammar crate (`tree-sitter-solidity = "1.2.13"`, crates.io, MIT; `LanguageFn`
ABI — no vendored wasm). Per-item disposition:

| upstream (`1441933`)                                                                                             | disposition           | notes                                                                                              |
| ---------------------------------------------------------------------------------------------------------------- | --------------------- | -------------------------------------------------------------------------------------------------- |
| `types.ts`: `'solidity'` in `LANGUAGES`                                                                          | **PORT**              | new `Language::Solidity` variant; `Language::ALL` 37→38, `LANGUAGE_STRINGS` 37→38 in lockstep      |
| `grammars.ts`: `'.sol': 'solidity'` EXTENSION_MAP                                                                | **PORT**              | `"sol" => Language::Solidity` in `builtin_language_for_ext`                                        |
| `languages/index.ts`: register `solidityExtractor`                                                               | **PORT**              | `SOLIDITY_SPEC` + `spec_for_language` arm + `parse_language` (`queries.rs`)                        |
| `solidity.ts`: contract/library/interface/struct/enum node-kind sets                                             | **PORT**              | contract/library → Class, interface → Interface, struct → Struct, enum → Enum (generic dispatch)   |
| `solidity.ts`: synthetic ctor/fallback/receive names (`resolveName`)                                             | **PORT**              | `SOLIDITY_SPEC::resolve_name`                                                                      |
| `solidity.ts`: `getSignature` (walk direct `parameter`/`return_type_definition`/`visibility`/`state_mutability`) | **PORT**              | params are direct children, not a `parameters` field                                               |
| `solidity.ts`: `visitNode` inheritance (`inheritance_specifier`)                                                 | **PORT**              | walker D3 (`user_defined_type` ancestor → Extends; resolver promotes to Implements for interfaces) |
| `solidity.ts`: `visitNode` state-var/struct-member/event/error → field                                           | **PORT**              | walker D5 direct-`name` field fallback (+ D10 for file-level event/error)                          |
| `solidity.ts`: `visitNode` `enum_value`                                                                          | **PORT**              | walker D6 bare-text `enum_value` → EnumMember                                                      |
| `solidity.ts`: file-level `constant_variable_declaration`                                                        | **PORT**              | walker D8 Solidity arm in `extract_variable` → Constant                                            |
| `solidity.ts`: `extractImport` (source string)                                                                   | **PORT**              | `SOLIDITY_SPEC::extract_import`                                                                    |
| `tree-sitter.ts`: `extractDecoratorsFor` `modifier_invocation` → `calls`                                         | **PORT**              | walker D7 header `modifier_invocation` → Calls (NOT Decorates)                                     |
| `is`→`implements` reclassification + import resolution                                                           | **EXISTING RESOLVER** | served by `resolver.rs:1231-1247` + name matcher + import resolver — zero new resolve-tier code    |

Golden impact: `.schema` byte-stable (`language` is a stored TEXT VALUE, not DDL —
`schema_parity` green, `colby.schema.sql` byte-identical); the seven existing
goldens (`cpp`/`godot`/`ruby`/`mini`/`metal`/`cuda`/`arkts`) byte-neutral (none
holds a `.sol` file, no regen); one NEW `solidity` golden
(`reference/golden/solidity/`, corpus `crates/codegraph-bench/fixtures/solidity/`)
carries the only `"language":"solidity"` string in the corpus. Being fully
self-contained, its `refs.json` is empty and `edges.json` holds the resolved
`Implements` (`Token → IERC20`) + `Calls` (`transfer → onlyOwner`,
`transfer → Transfer`) edges. This is an extraction-tier add matching the port's
other 24 grammar-backed Tier-1 languages.

Tracked colby parity stays `1.2.0` (advances to `1.4.1` only once the full
`1.2.0..1.4.1` PORT/DEFER subset lands — this is one item of that subset).

### 2026-07-15 — LANDED DEFER item H1: ArkTS (`.ets`, extraction slice of #1186 `9915221`) via a dedicated `tree-sitter-arkts` grammar

Landed DEFER item H1 — the **extraction slice ONLY** of the upstream ArkTS
(HarmonyOS / OpenHarmony `.ets`) support. Unlike Metal/CUDA (item G), ArkTS is a
NEW `Language::ArkTs` variant backed by a NEW grammar crate
(`tree-sitter-arkts = "0.2.0"`, a TypeScript-superset fork that parses the ArkUI
`@Component struct` syntax `tree-sitter-typescript` cannot). Per-item disposition:

| upstream (`9915221`)                                                            | disposition | notes                                                                                              |
| ------------------------------------------------------------------------------- | ----------- | -------------------------------------------------------------------------------------------------- |
| `types.ts`: `'arkts'` in `LANGUAGES`                                            | **PORT**    | new `Language::ArkTs` variant; `Language::ALL` 36→37, `LANGUAGE_STRINGS` 36→37 in lockstep         |
| `grammars.ts`: `'.ets': 'arkts'` EXTENSION_MAP                                  | **PORT**    | `"ets" => Language::ArkTs` in `builtin_language_for_ext`; plain `.ts` stays TypeScript             |
| `languages/index.ts`: register `arktsExtractor`                                 | **PORT**    | `ARKTS_SPEC` + `spec_for_language` arm + `parse_language` (`queries.rs`)                           |
| `arkts.ts`: `structTypes: ['struct_declaration']`                               | **PORT**    | `@Component struct` → `NodeKind::Struct` via the existing `extract_struct` path (no walker change) |
| `arkts.ts`: `callTypes` — the `'call_expression'` element                       | **PORT**    | `call_types = ["call_expression"]`                                                                 |
| `arkts.ts`: `callTypes` — `arkui_component_expression`/`leading_dot_expression` | **DEFER**   | ArkUI component-instantiation / attribute-chain dispatch                                           |
| `arkts.ts`: `extractModifiers`/`collectDecoratorNames` decorator hook           | **DEFER**   | feeds the deferred state→build() synthesizer; `extract_modifiers` left at trait default            |
| `tree-sitter.ts`: arkts `extractCall` branch (attribute chains, `.onXxx`)       | **DEFER**   | ArkUI resolution heuristics                                                                        |
| state→build() re-render, emitter emit→subscriber, router→`@Entry` bridges       | **DEFER**   | callback synthesizer the port lacks                                                                |
| ohpm `oh-package.json5` workspace-package import resolution                     | **DEFER**   | import-resolver feature                                                                            |
| `$r`/`$rawfile` intrinsics, web-family/value-ref/re-export gates                | **DEFER**   | resolution-side gates                                                                              |
| `index_state` / index-completeness guard                                        | **DEFER**   | unrelated subsystem                                                                                |

Golden impact: `.schema` byte-stable (`language` is a stored TEXT VALUE, not DDL —
`schema_parity` green, `colby.schema.sql` byte-identical); the six existing
goldens (`cpp`/`godot`/`ruby`/`mini`/`metal`/`cuda`) byte-neutral (none holds a
`.ets` file, no regen); one NEW `arkts` golden
(`reference/golden/arkts/component.ets`) carries the only `"language":"arkts"`
string in the corpus. Rationale for the DEFER slice: the port has no
callback-synthesizer subsystem and treats every non-Godot framework's
runtime-dispatch layer as out of scope (consistent with Godot being the ONE
concrete `FrameworkResolver`). This is an extraction-tier add matching the port's
25 other grammar-backed languages.

Tracked colby parity stays `1.2.0` (advances to `1.4.1` only once the full
`1.2.0..1.4.1` PORT/DEFER subset lands — this is one item of that subset).

### 2026-07-14 — LANDED DEFER item G: Metal (`.metal`, #1121 `cc89146`) + CUDA (`.cu`/`.cuh`, CUDA-lang parts of #1172 `e1a8d88`) via the existing C++ grammar

Landed DEFER item G — the two cheapest deferred languages from the
`1.2.0..1.4.1` subset. Both ride the ALREADY-PRESENT `tree-sitter-cpp` grammar
(no new grammar crate) and map to `Language::Cpp` (**no new `Language` variant**,
so `.schema` + `Language::ALL` len 36 are byte-stable). Per-item disposition:

| upstream                                                                                       | what it is                                                                                          | disposition                                                                   | golden impact                                                     |
| ---------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| #1121 `cc89146` `.metal`→cpp map                                                               | Metal Shading Language extension mapping                                                            | **PORT** (`engine.rs` `builtin_language_for_ext`)                             | NEUTRAL for existing fixtures; NEW `metal` fixture                |
| #1121 `cc89146` `blankMetalAttributes`                                                         | offset-preserving `.metal`-gated `[[attribute]]` blank (prevents spurious `extends`)                | **PORT** (`lang/cpp.rs` `blank_metal_attributes`)                             | NEUTRAL (gate proven: `[[nodiscard]]` in `.cpp` byte-identical)   |
| #1172 `e1a8d88` `.cu`/`.cuh`→cpp map                                                           | CUDA extension mapping                                                                              | **PORT** (`engine.rs`)                                                        | NEUTRAL for existing fixtures; NEW `cuda` fixture                 |
| #1172 `e1a8d88` `blankCudaConstructs`                                                          | specifier + `__launch_bounds__` + brace-balanced `<<<…>>>` launch blank (offset-preserving)         | **PORT** (`lang/cpp.rs` `blank_cuda_constructs`)                              | NEUTRAL (content/ext-gated; no CUDA markers in existing fixtures) |
| #1172 `e1a8d88` `looksLikeCudaSource`                                                          | strong content markers (`__global__`/`__device__`/`__constant__`/`cudaStream_t`)                    | **PORT** (`lang/cpp.rs` `looks_like_cuda_source`)                             | NEUTRAL                                                           |
| #1172 `e1a8d88` `preParseCSource`                                                              | C-detected `.h` headers get the content-gated CUDA blank                                            | **PORT** (`lang/c.rs` `CSpec::pre_parse`)                                     | NEUTRAL                                                           |
| #1172 `e1a8d88` `recoverCppMacroDefinedName`                                                   | name-in-first-arg macro-kernel recovery (`DEFINE_FLASH_FORWARD_KERNEL(name, …)`; gtest/pybind bail) | **PORT** (`lang/cpp.rs` + `CppSpec::resolve_name`)                            | NEUTRAL (narrow gate; NEW `cuda` fixture guards it)               |
| #1172 `e1a8d88` namespace-prefix + template-arg call strip                                     | (Release D Items 1+2)                                                                               | **EXCLUDE — already landed in Release D**                                     | n/a                                                               |
| #1172 `e1a8d88` `cppLocalFnPtrs` local-fn-ptr launch dispatch (`auto k = &fn<…>; k<<<…>>>(…)`) | narrow real-world pattern, per-body binding map                                                     | **DEFERRED within G** (recorded gap; primary launch edges resolve without it) | n/a                                                               |

**Enabling change:** threaded `file_path: &str` into the `LanguageSpec::pre_parse`
trait method (was path-less) — the faithful analogue of upstream's optional
`filePath` param, needed for the Metal `.metal` gate. Default body ignores the
path; `CsharpSpec` gains an unused `_file_path`, `CppSpec`/`CSpec` use it.

**New golden fixtures:** `reference/golden/metal/` (`shader.metal`) and
`reference/golden/cuda/` (`kernel.cu`), wired into `equivalence.rs` mirroring the
`cpp` pair. The `cpp`/`godot`/`ruby`/`mini` goldens are byte-identical (no regen).
See `docs/equivalence.md` "Metal fixture" / "CUDA fixture" for the regen recipe.

Tracked parity STILL `1.2.0` — advances to `1.4.1` only in the final ledger step
once ALL PORT/DEFER items land.

### 2026-07-14 — LANDED DEFER item F: prompt-hook confidence-tiered gate (#1126 + #1138 + #1136 mechanics; telemetry EXCLUDED; golden-neutral)

Landed DEFER item F — the prompt-hook **gate** improvements from colby
`1.2.0..1.4.1` — turning `codegraph prompt-hook` from a bare unconditional
`codegraph_explore` forwarder into a three-tier confidence gate. All telemetry is
EXCLUDED by maintainer decision (this port has no tracking pipeline and this
change introduces none). Per-item disposition (each cited to the upstream diff):

| upstream                                                  | what it is                                                                          | disposition                                                                                                                  | golden impact                                                 |
| --------------------------------------------------------- | ----------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| #1126 `317e7f4`                                           | multilingual structural-keyword gate (WORDS + STEMS + UNSEGMENTED, Unicode bounds)  | **PORT** (`structural_gate.rs`)                                                                                              | NEUTRAL (pure gate logic in the CLI path)                     |
| #1138 `713ab7a`                                           | right-bound `call`/`trace`/`affect`/`connect` stems (callus/Connecticut don't fire) | **PORT** (folded into `structural_gate.rs`)                                                                                  | NEUTRAL                                                       |
| #1136 `e699ee9` mechanics                                 | confidence-tiered injection (HIGH full-explore / MEDIUM pointer / silent); tokens   | **PORT-minus-telemetry**                                                                                                     | NEUTRAL (MEDIUM tier derived at query time — see below)       |
| #1136 `name_segment_vocab` table + schema v7 + write path | index-time materialized vocab                                                       | **N-A (design-substituted)** — replaced by query-time derivation from `distinct_non_file_node_names`; NO table, NO migration | NEUTRAL by substitution (a table would break `schema_parity`) |
| #1136 gate telemetry / #be55b93                           | anonymous gate-outcome usage counters                                               | **EXCLUDE-telemetry**                                                                                                        | n/a (not ported)                                              |
| #35611b9 #1144                                            | skip file/import-only names as surfaced symbols                                     | **PORT** (`isSegmentableKind` gate in `distinct_non_file_node_names` + representative pick)                                  | NEUTRAL                                                       |
| #35611b9 #1145                                            | English-plural variant folding correctness (`segment_lookup_variants`)              | **PORT** (`segments.rs`)                                                                                                     | NEUTRAL                                                       |
| #35611b9 #1146                                            | co-occurrence variant folding (a plural pair of one word ≠ a two-word match)        | **PORT** (in-Rust word fold in `get_segment_matches`)                                                                        | NEUTRAL                                                       |
| #35611b9 #1141/#1142                                      | updateNode vocab write / heal-if-empty                                              | **N-A** — no write-path vocab in the query-time design                                                                       | NEUTRAL                                                       |

**What landed (all in `crates/codegraph-cli`, plus one read-only store query):**
`structural_gate.rs` (the multilingual WORDS/STEMS/UNSEGMENTED term lists copied
verbatim from `directory.ts`, matched with lookaround-free capture-class
boundaries since Rust `regex` has no lookbehind; `has_structural_keyword`;
`extract_code_tokens`), `segments.rs` (`split_identifier_segments`,
`normalize_prose_word`, `extract_prose_candidates` + full `ENGLISH_PROSE_STOPWORDS`,
`segment_lookup_variants`), `segment_match.rs` (`get_segment_matches` + the
`SegmentMatch` struct — Tier-A co-occurrence + Tier-B rare-word, ported from
`index.ts`), and `distinct_non_file_node_names` in `codegraph-store` (the
golden-neutral query-time substitution for the index-time vocab table).
`cmd_prompt_hook` now parses the Claude `{prompt,cwd}` JSON payload (raw-string
fallback preserved), honors the `CODEGRAPH_NO_PROMPT_HOOK`/`CODEGRAPH_PROMPT_HOOK`
kill-switch, and runs the HIGH → MEDIUM → silent gate.

**Telemetry is EXCLUDED in full** — no counter, no `gate()` recording, no
`DO_NOT_TRACK` surface, nothing that phones home. The **schema version 7 is unchanged**:
NO `name_segment_vocab` table and NO migration were added — the
MEDIUM tier is derived at query time from existing node names, so
`reference/golden/colby.schema.sql` and all golden fixtures stay byte-identical
(`schema_parity` + `cargo test -p codegraph-bench` green, NO regen). No new
AI/vector/LLM crate (guardrail clean); the two added deps (`regex`,
`unicode-normalization`) are plain-text crates.

**Outcome:** DEFER-F is CLOSED. This is a golden-NEUTRAL, telemetry-free `feat:`
release. Tracked parity advances toward `1.4.1` for the prompt-hook gate surface;
the segment-vocab table remains intentionally N-A by query-time substitution.

### 2026-07-14 — LANDED DEFER item E: #1212 (`a3f9008`) bounded-memory resolution tail (2 PORTs; golden-neutral)

Re-evaluated DEFER item E — `a3f9008` _"bounded-memory yielding pipeline tail +
daemon session fixes (#1212) (#1226)"_ — after a momus review. #1212 is MOSTLY
N/A (its TS event-loop yields and OOM'd synthesizers do not exist in this port),
but it has a REAL, golden-neutral Rust port. 7-part classification (each cited
against the current tree):

| #   | #1212 sub-part                                                  | verdict                                                                            | cited Rust evidence + one-line rationale                                                                                                                                                                                                                                                                                                                                                                                                               |
| --- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | event-loop yield points in 31 synthesis passes                  | **N/A (TS-only)**                                                                  | Rust resolution is rayon data-parallel (`resolver.rs` `resolve_chunk_parallel` → `.par_iter().map().collect()` over the `Sync` snapshot), not a JS event loop; no in-process liveness watchdog a synchronous span blocks. No analogue to `await yieldPoint()`.                                                                                                                                                                                         |
| 2   | whole-graph snapshot OOM — kotlin `getAllNodes()` 2M-node array | **PORT (reachable + streamable)**                                                  | `SnapshotResolutionContext::from_store` (`snapshot_context.rs:92-174`) does `store.all_nodes()` (`:94`) then materializes every node across six whole-graph maps + `build_edge_adjacency` (`:347-363`) reads `all_nodes()` again — O(graph)×copies at kernel scale. `StoreResolutionContext` (`context.rs:28-63`) already gives byte-equivalent ordered lookups over bounded LRUs, so a large-graph fallback bounds memory to batch + LRU. **LANDED.** |
| 3   | kotlin expect/actual SQL-side filter                            | **N/A (pass absent)**                                                              | No kotlin expect/actual synthesizer exists in `codegraph-resolve`.                                                                                                                                                                                                                                                                                                                                                                                     |
| 4   | c-fnptr LRU-bounded caches                                      | **N/A (pass absent)**                                                              | No C function-pointer dispatch synthesizer exists (`#932` is DEFER).                                                                                                                                                                                                                                                                                                                                                                                   |
| 5   | spring reads each `.java` once, not twice                       | **N/A (pass absent)**                                                              | Only per-ref Spring DI receiver-typing exists (`name_matcher.rs`), not a whole-repo `publishEvent` event→listener synthesizer.                                                                                                                                                                                                                                                                                                                         |
| 6   | `runMaintenance` off-thread + post-index WAL checkpoint         | **ALREADY-HAVE (valve) + PORT (extend to resolution loop) + N/A (worker offload)** | WAL valve landed R-A/#1231 (`queries.rs` `checkpoint_wal_if_over`), but was called ONLY in spill-replay (`main.rs:3525`/`:3536`), NOT the batched-resolution write loop. **LANDED:** the valve now also fires after each resolution-batch write. The worker-thread offload is N/A (no main-thread heartbeat).                                                                                                                                          |
| 7   | daemon socket-handoff race + `pool.ready` gate                  | **ALREADY-HAVE / N/A**                                                             | The active daemon path (`session.rs` `serve_session_async`) reads the hello + all bytes from one long-lived buffered reader — no Node stream-mode discard. #1200/#1185 already landed R-A. `pool.ready` is a Node cold-start tweak, no analogue.                                                                                                                                                                                                       |

**LANDED PORTs (both golden-neutral, memory/IO only):**

1. **Large-graph streaming fallback** — `resolver.rs`
   `resolve_and_persist_batched_inner` (shared by the public wrappers): when node
   count `>= RESOLVE_STREAMING_NODE_THRESHOLD` (default `1_000_000`, override
   `CODEGRAPH_RESOLVE_STREAMING_THRESHOLD`), resolve each batch SERIALLY through a
   per-batch `StoreResolutionContext` over the live store instead of the
   `SnapshotResolutionContext` + rayon path. Prior-batch `implements`/`extends`
   edges are committed before the next batch reads them, so `get_supertypes` sees
   the same cross-batch growth `build_edge_adjacency` provides. Same rowid cursor,
   persistence order, deletion order, marker, and #750/#808 passes. The default
   threshold is high enough that normal repos ALWAYS keep the unchanged snapshot
   path (byte-identical output; every golden fixture exercises it).
2. **WAL valve in the resolution write loop** — the same env-gated
   `store.checkpoint_wal_if_over(wal_valve_bytes)` (TRUNCATE-only, row-neutral)
   now fires after each resolution-batch write, bounding WAL growth through the
   resolution tail, not just spill-replay. `CODEGRAPH_NO_WAL_DEFER` /
   `CODEGRAPH_WAL_VALVE_MB` govern it exactly as before.

**Golden-verification bridge:** the dual-path equivalence test
`streaming_fallback_matches_snapshot_across_multiple_batches` (`resolver.rs`
tests) forces batch_size 1 (≥2 batches, asserted) with a cross-batch
`Sub extends Base` / `sub.ping()` case, runs BOTH paths on separate identical
stores, and asserts `nodes`/`edges`/`unresolved_refs` byte-identical — so the
snapshot path's golden verification transitively covers the fallback. A deliberate
fallback perturbation was confirmed to fail the assertion (teeth), then reverted.
`wal_valve_fires_during_resolution_batch_writes` asserts the valve fired DURING
resolution (telemetry counter) and that the resolved graph is byte-identical valve
on vs off. `cargo test -p codegraph-bench` passes with NO regen; goldens unchanged.

Tracked parity STILL `1.2.0` — advances to `1.4.1` only in the final ledger step
once every PORT item lands.

### 2026-07-14 — LANDED Release D: C++ namespace/template resolution + UE macro recovery + `.h` detection + Lua gate

Ports the C++ half of the `1.2.0..1.4.1` PORT subset plus the folded-in Lua
resolve fix. Tracked parity STILL `1.2.0` — it advances to `1.4.1` only in the
final ledger step once every PORT item lands. Five upstream items landed:

- **C++ namespace-block qualified names + `ns::fn()` resolution** (from
  `e1a8d88`, the CUDA commit): `codegraph-extract/walker.rs` gains a
  `namespace_prefix` stack pushed on `namespace_definition` (prefix-only, no
  namespace node — avoids the #1093 crowd-out) and seeded into
  `build_qualified_name`. `ns::fn()` calls resolve for free via the existing
  qualified-name matcher (no resolver change). C++17 `namespace a::b {` included;
  anonymous namespaces fall through to bare names.
- **C++ template-arg call linking `fn<T,256>()`** (from `e1a8d88`):
  `extract_call` strips template args on C++/C callees via the existing
  `strip_cpp_template_args` (the #1043 base-class normalization), excluding
  `operator<`/`operator<<`.
- **#1158 / `8e697a0` — UE in-body reflection-macro recovery**: `CppSpec` now
  overrides `pre_parse` with three offset-preserving byte-blanking passes
  (`blank_cpp_api_prefix_macros`, `blank_cpp_inline_annotation_macros`,
  `blank_cpp_annotation_macro_calls`), each `contains`-gated so macro-free C++ is
  byte-identical. The look-ahead boundaries upstream used are reimplemented in
  code (the `regex` crate has none).
- **#1159 / #1133 / `c049d9e` — `.h` C-vs-C++ detection**: `looks_like_cpp` /
  `looks_like_objc` sniff the 8 KB source prefix inside `extract_source` and
  reclassify a `.h` from C to C++/ObjC. The path-only `detect_language` and its
  watch/cli/resolve callers are UNCHANGED (they pass no source, so default-C
  holds — matching upstream).
- **#1124 / `e53968c` — Lua/Luau annotation self-match gate**: the shipped Rust
  pattern was a broken lookahead port (`(?!` is silently `.ok()`-dropped by the
  `regex` crate), so `obj:Method()` still self-matched. Fixed CODE-SIDE:
  `local_receiver_type_patterns_tagged` tags the annotation regex and
  `infer_local_receiver_type` rejects the capture via
  `lua_annotation_is_method_call` when a Lua call form (`(`/`"`/`'`/`[`/`{`) or a
  `[\w.]` continuation follows.

Two discoveries worth recording: (a) the namespace + template-arg-call code lives
in the CUDA commit `e1a8d88`, whose CUDA (`blankCudaConstructs`,
`looksLikeCudaSource`) and local-fn-ptr (`cppLocalFnPtrs`) slices were
DELIBERATELY EXCLUDED (they belong to a future CUDA release); (b) the pre-existing
Rust Lua pattern was a broken lookahead port, fixed with a code-side gate rather
than an (unsupported) regex look-ahead.

Golden: a DELIBERATE `cpp` fixture BUMP. The corpus gained `namespaced.cpp`,
`templated_call.cpp`, and `ue_actor.h`; the diff is exactly (i) `Tpl`/`Tpl::wrap`
→ `ns::Tpl`/`ns::Tpl::wrap` (node-ids stable — name is unqualified), (ii) the
`Q : ns::Tpl<int>` base migrating unresolved→resolved (`Extends` edge appears,
ref leaves `refs.json`), and (iii)-(iv) the three new files' additive
file/symbol/Contains records + the intended `ns::compute()` / `process<int>()` /
`UFoo`→`UObject` edges. `godot`/`ruby`/`mini` goldens byte-identical.

### 2026-07-10 — EVALUATION 1.2.0 → 1.4.1 (parity NOT yet advanced; PORT subset in progress)

Ran Workflow A's diff step against the live upstream delta,
`git diff v1.2.0..v1.4.1 -- src/` (76 files, +10965/−1209). The range is
dominated by 11 new language grammars and a prompt-hook feature suite — bulk
that doesn't map cleanly to this port — so the portable high-value subset out
of it is small. Two items from this range are already landed under other
entries: #1043 (C/C++ templated-base inheritance, `v0.27.0`) and #1064 (explore
change-surface rescue, `v0.28.0`) — both recorded above, not repeated here.
Tracked parity stays `1.2.0`; the table below is the chosen PORT set, in
progress across four releases (A/B/C/D), plus the ALREADY-HAVE / N-A / DEFER
dispositions for everything else in range.

**PORT (chosen, in progress)**

| upstream # / commit                                                                                                                      | title                                                                                                                                                                                          | Rust target                                                                                         | golden impact                | disposition                                                         |
| ---------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | ---------------------------- | ------------------------------------------------------------------- |
| #1220 / `70b1be6`                                                                                                                        | PHP `$this->dep->method()` resolves via property declared type                                                                                                                                 | `codegraph-resolve/name_matcher.rs` + codegraph-extract PHP field types                             | golden bump                  | PORT (Release C)                                                    |
| #1231 (WAL-valve half) / `a11a439`                                                                                                       | defer WAL checkpoint write-back during bulk index (`CODEGRAPH_NO_WAL_DEFER` / `CODEGRAPH_WAL_VALVE_MB`)                                                                                        | `codegraph-store/connection.rs`                                                                     | golden-neutral               | PORT (Release A)                                                    |
| #1187 / `4c15f84`                                                                                                                        | orphaned unresolved-ref sweep so an interrupted index heals on sync + partial-index status warning                                                                                             | `codegraph-watch/sync.rs`, CLI `status`, MCP `status`                                               | golden-neutral               | PORT (Release A)                                                    |
| #1200 / `356f5f7`                                                                                                                        | daemon inactivity backstop gated on client liveness                                                                                                                                            | `codegraph-daemon/lib.rs`                                                                           | golden-neutral               | PORT (Release A)                                                    |
| #1185 / `c9f8c0e`                                                                                                                        | reap server when launcher killed during startup + `CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS` backstop                                                                                            | `codegraph-daemon/spawn.rs`                                                                         | golden-neutral               | PORT (Release A)                                                    |
| C++ namespace-block qualified names + `ns::fn()` resolution + template-arg `fn<T,256>()` call linking (part of 1.3.0, code in `e1a8d88`) | —                                                                                                                                                                                              | `codegraph-extract/walker.rs` (namespace prefix + template-call strip; `name_matcher.rs` unchanged) | golden bump (cpp fixture)    | **LANDED (Release D, 2026-07-14)**                                  |
| #1158 / #1159 / #1133 (`8e697a0`, `a9e8fa4`, `c049d9e`)                                                                                  | recover heavily-reflected Unreal Engine C++ classes (in-body reflection macros, member-level `*_API`, mid-line `UMETA`/`UPARAM`/`UE_DEPRECATED`, `.h` C-vs-C++ detection through export macro) | `codegraph-extract/lang/cpp.rs` (`pre_parse` blanking) + `engine.rs` (`.h` content sniff)           | golden bump (cpp fixture)    | **LANDED (Release D, 2026-07-14)**                                  |
| #1124 / `e53968c`                                                                                                                        | Lua/Luau capitalized `obj:Method()` misread as type annotation                                                                                                                                 | `codegraph-resolve/name_matcher.rs`                                                                 | golden-sensitive (Lua edges) | **LANDED (Release D, 2026-07-14; golden-neutral — no Lua fixture)** |
| #1063 / `2a06d9a`                                                                                                                        | `codegraph.json` `include` list to force gitignored first-party source in                                                                                                                      | `codegraph-core/config.rs` + codegraph-extract scan + codegraph-watch policy                        | golden-neutral               | PORT (Release B)                                                    |
| #1182 / `f5edf8c`                                                                                                                        | iBatis2 `<sqlMap>` coverage + MyBatis quote/comment robustness + dup-id                                                                                                                        | `codegraph-extract/embedded/mybatis.rs`                                                             | golden bump                  | PORT (low; optional)                                                |
| #1243 / `4782394`                                                                                                                        | MCP update-available notice (background, once/day, honors `DO_NOT_TRACK` / `CODEGRAPH_NO_UPDATE_CHECK`)                                                                                        | `codegraph-mcp/engine.rs` status                                                                    | golden-neutral               | PORT (low; UX)                                                      |

**ALREADY-HAVE**

| upstream #    | what it does                                 | Rust evidence                                                                                                        |
| ------------- | -------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| #1129 / #1130 | typed-param receiver inference               | `name_matcher.rs:765-833`, landed R5 (`v0.26.0`) — Rust already covers more languages than this upstream change adds |
| #1240         | incremental cross-file re-resolve            | `codegraph-watch/sync.rs` + `queries.rs` already re-resolve in both directions                                       |
| #1235         | closure-collection scan across all languages | no Rust analogue exists for this to duplicate — the described failure mode cannot occur here                         |

**N/A (TS/Node-only)**

| upstream #                 | why it doesn't apply                                                                                                                                    |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| #1231 (parse-timeout half) | worker-clock race is a Node worker/coordinator artifact; Rust indexing is rayon in-process with compiled-in grammars — no separate worker clock to race |
| #1238                      | npm self-shadow — npm-specific install-path collision, no Rust analogue                                                                                 |
| #1071                      | uninstall binaries — npm/install.sh lifecycle step, not applicable to the cargo-installed/binary-download build                                         |
| #1238                      | `install --refresh` — npm reinstall flow, no Rust analogue                                                                                              |
| #1238                      | Windows `npm.cmd` shim handling — npm-only                                                                                                              |
| #1180                      | Spring config-key scan — this port has no Spring config-key resolver at all; not a gap this upstream fix closes                                         |
| #1156                      | gitignored child-repo offer — no embedded-repo discovery mechanism exists in this port (already recorded as an architecture gap in earlier entries)     |

**DEFER**

| item                                                                                                                             | why deferred                                                                                                                                                                                                               |
| -------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 11 new languages (Nix, ArkTS, Terraform, CUDA, Solidity, Erlang, VB.NET, COBOL, CFML, Metal + iBatis2 counted as low PORT above) | each needs a grammar crate + `LanguageSpec` + golden fixture; Metal/CUDA are cheapest (ride the existing C++ grammar)                                                                                                      |
| prompt-hook suite (#1126 / #1136 / #1138 / #1141-1146 + telemetry)                                                               | large surface; telemetry conflicts with this project's no-tracking posture — if pursued, cherry-pick the multilingual/plain-words gate WITHOUT the telemetry half                                                          |
| #1252                                                                                                                            | explore NL-word guard — Rust `explore` is FTS-seeded, not token-seeded, so applicability is low; `KNOWN_DIFFS.md` already records relevance re-ranking as intentionally not ported                                         |
| #1212                                                                                                                            | ~~snapshot-OOM streaming refactor~~ — **RE-EVALUATED + LANDED 2026-07-14** as DEFER item E (large-graph streaming fallback + resolution-loop WAL valve; both golden-neutral). See the 2026-07-14 DEFER-item-E entry above. |
| #1114                                                                                                                            | dead reasoning-offload cleanup — Rust never had the reasoning-offload feature, so there's nothing to remove                                                                                                                |

**Outcome:** tracked parity remains `1.2.0`. A 4-release PORT program is in
progress: Release A is a golden-neutral robustness bundle (#1187 + #1200 +
#1185 + #1231's WAL-valve half); Release B is #1063 `include`; Release C is
#1220 (PHP property-typed receiver); Release D is C++ namespace/template
resolution + Unreal Engine macro recovery. A DEFER phase follows (snapshot-OOM
#1212, the prompt-hook suite minus telemetry, then the 11 new languages).
Tracked version advances to `1.4.1` only once the chosen PORT items above
actually land; the three low/optional items (#1124, #1182, #1243) may fold into
a related release round or be individually deferred at that time.

### 2026-07-07 — PORT #1064 explore change-surface rescue (explore-side only)

Ports upstream `2b256b9` (`fix(explore): surface a named method's buried
signature type (#1064)`, shipped in colby 1.1.5) into this project's
deterministic explore, adapted to the query/ranking spine (no RWR/graph-mass
score exists here). Extraction is UNCHANGED — the signature edge the rescue
follows (`References`, function→param-type) already exists for TS/TSX, proven by
a live 2-file repro; extraction-widening to emit `TypeOf`/`Returns` for more
languages is DEFERRED.

| Round | Upstream          | Disposition | codegraph-rs | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ----- | ----------------- | ----------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B     | #1064 / `2b256b9` | PORT        | `v0.28.0`    | `codegraph-mcp/src/engine.rs` `find_relevant_context`: (1) named-seed **tier de-noise** — only the top callable seed + any seed with caller-count ≥ 25% of the max is tiered, so a low-centrality namesake can't flood the tier; (2) **change-surface rescue** — from each tiered callable seed, follow outgoing signature edges (`References`/`TypeOf`/`Returns`) to TYPE-kind nodes; a type whose file is BURIED (not a seed file AND < 2 query-term hits) is inserted + marked `rescued_files` so `finalize` floats it to a new TOP tier (3). Deterministic: candidates sorted by `(file, line, id)` before insert; no HashMap/HashSet iteration reaches output. Query/ranking-side only → **golden unchanged** (the mini golden's `Counter add runDemo` query has only primitive `number` param types, so the rescue never fires). |

### 2026-07-06 — FULL SYNC `1.1.2 → 1.2.0` (advances tracked parity to 1.2.0)

Ported the actionable subset of the `1.1.2..1.2.0` upstream delta, in six
momus-approved, individually-released, Final-Wave-gated rounds (each: TDD ≥95%
patch coverage → golden regen where extraction/resolution changed → 4-reviewer
Final Wave → own `fix:`/`feat:` release). Schema round ran first so later rounds
regenerated golden against the final schema.

| Round | Upstream                                     | Disposition | codegraph-rs | Notes                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ----- | -------------------------------------------- | ----------- | ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R0    | #1034                                        | PORT        | `v0.25.2`    | UNIQUE edge-identity index + migration v7 (dedup keep-lowest-id then create index). `.schema` golden bump; golden fixtures had no dup edges so edges.json unchanged.                                                                                                                                                                                                                                                  |
| R1    | #1045, #1047, #1127                          | PORT        | `v0.25.3`    | query drops nonsensical `NNNN%` score (raw score kept in `--json`); Android `res/` default-excluded (res/raw + `src/main/resources` kept; `.gitignore` `!` negation now honored); auto-sync backs off + disables after repeated non-contention failures. Query/config/watch only — no golden.                                                                                                                         |
| R2    | #1086, #1087, #1089, #1090 (#1088 lock-test) | PORT        | `v0.25.4`    | impact keeps direct edge to already-reached node; callers/callees keep multi-KIND edges; hard node-limit enforced per-insertion. Query-side; golden unchanged.                                                                                                                                                                                                                                                        |
| R3    | #1093, #1096, #1061, #1035, #1100-#1103      | PORT        | `v0.25.5`    | C++ extraction: skip bodiless forward decls (C/C++-gated; Kotlin/Scala kept), conversion-operator names, export/visibility-macro class recovery, stack/brace construction Instantiates, inline-specifier-macro name/return-type recovery. Guarded by unit tests; retroactively byte-guarded by the `reference/golden/cpp/` fixture added with #1043 in `v0.27.0`.                                                     |
| R4    | #1110                                        | PORT        | `v0.25.6`    | Ruby `receiver.method` calls now emit edges (`Const.new`→Instantiates, else→Calls); bare `include/extend/prepend` unchanged. Added a Ruby golden fixture (`reference/golden/ruby/`) + equivalence test.                                                                                                                                                                                                               |
| R5    | #1108, #1125, #1112, #1079                   | PORT        | `v0.26.0`    | Receiver-type inference generalized beyond C++ to TS/JS/Python/Java/C#/Kotlin/Swift/Go/Rust/Dart/Scala/PHP (local-var + typed-param); Lua `:` / R `$` / Pascal `.` separators; same-file preference in 3 method-call paths. All validated via `resolve_method_on_type` (wrong guess → no edge). Mini golden's 3 local-var instance-method edges rose 0.8→0.9 to match upstream 1.2.0's receiver-inference confidence. |

**DEFERRED (recorded, not ported):**

- **#1043 / `703629e`** — C/C++ inheritance from a templated base. ~~This project has
  no general C++ `base_class_clause` inheritance extraction yet~~ **PORTED** in
  codegraph-rs `v0.27.0` (general C++ `base_class_clause` extraction + template
  stripping; added a C++ golden fixture).
- **#1064** — `explore` change-surface rescue. **PORTED** 2026-07-07 (explore-side
  only, over the existing `References` signature edge; see the 2026-07-07 entry
  above). Extraction-widening (emit `TypeOf`/`Returns` for more languages) stays
  DEFERRED — not needed for the TS/TSX value.

**ALREADY-HAVE (verified, no work):** #1088 (node-id visited dedup, locked with a test in R2), #1046 (explore count).

**N/A (no Rust analogue):** #1044 (CLI `node` subcommand — MCP file-mode already works), #1091 (index watchdog yield), #1092 (Windows console-flash windowsHide), #1041 & gitlink nested-repo items, `dfe13b0` (MCP worker pool), and all `install.sh` / npm / PATH-shadow / bundled-Node items (the Rust build has no npm/install.sh).

### 2026-06-30 — DOWNSTREAM forward-port: always-expose tools/list + required projectPath (issue #94)

This is a **targeted downstream forward-port**, NOT a full upstream sync. Tracked
colby parity stays `1.1.2` — this entry does not advance it.

Commit `35935be` (branch `feat/mcp-autodetect-no-default-tools`) ports two
upstream behavioral fixes that were missing from this Rust port, driven by issue
#94 (通义灵码/Lingma shows 0 tools when no `-p` is given):

- **colby #964 (PR #966)** — `tools/list` ALWAYS returns the visible tool surface,
  even when no default project is resolved. The prior Rust behavior returned an
  empty array (`[]`) for the no-default path, which is the #94 root cause.
- **colby #993 (PR #1007)** — when no default project resolves, `projectPath` is
  added to each tool's `inputSchema.required` array (a schema-level nudge so a
  roots-less client's agent supplies the path per call). When a default project IS
  resolved, `projectPath` stays OPTIONAL (byte-identical to the indexed case).

**Supersedes the 2026-06-28 `d3179f5` ALREADY-HAVE disposition** (see triage
table below at line 60): that entry called our "hide all tools when no default
project" behavior "stronger" than upstream's `d3179f5` approach, but that
hide-all is the #94 root cause. We now adopt colby's always-list (#964) +
required-projectPath (#993) instead.

Golden artifacts stayed byte-identical — the change only affects the no-default
path, which no golden frame exercises (`git diff reference/` empty). `make ci`
green; `cargo test -p codegraph-bench` green; `docs/upstream-sync/KNOWN_DIFFS.md`
untouched.

| item                   | disposition       | note                                                                                                                                                                                                          |
| ---------------------- | ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| colby #964/PR#966      | **PORT (landed)** | `tools/list` always returns visible tools; Rust `dispatch` arm in `server.rs` changed from `Value::Array(Vec::new())` to `schemas::visible_tool_definitions_requiring_project_path()` on the no-default path. |
| colby #993/PR#1007     | **PORT (landed)** | `with_required_project_path()` pure fn injects `"projectPath"` into each tool's `inputSchema.required`; fires only on the no-default branch; idempotent.                                                      |
| `d3179f5` (superseded) | **SUPERSEDED**    | The prior ALREADY-HAVE disposition (2026-06-28) incorrectly called hide-all "stronger". The hide-all behavior was the #94 root cause and is now reversed.                                                     |

### 2026-06-28 — LANDED `v1.1.1..v1.1.2` PORT items (codegraph-rs `v0.20.0`)

Shipped the portable subset of `v1.1.1..v1.1.2` in PR #90 → release `v0.20.0`
(three feature commits + one Windows-clippy fixup). Tracked parity advanced to
`1.1.2`. Golden impact: the ONLY `reference/` change was
`reference/golden/mcp/tools_list.json` (the deliberate `readOnlyHint`
annotations); the extraction corpus is byte-unchanged. `make ci` green, golden
oracle green, Final Verification Wave (F1 goal/constraint + F2 code quality +
F3 golden/invariant) all APPROVE.

| commit        | landed as                            | disposition (final) | note                                                                                                                                                                                                                                                   |
| ------------- | ------------------------------------ | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `a79fa51`     | `feat(mcp)` readOnlyHint annotations | **PORTED**          | all 10 source tool defs annotated `{readOnlyHint:true, destructiveHint:false, idempotentHint:true, openWorldHint:false}`; golden fixture + `golden_mcp.rs` assertion extended to compare `annotations` (load-bearing).                                 |
| `f83a1ec`     | daemon FS fallback                   | **PORTED**          | `bind_with_fallback` walks a deterministic socket-candidate chain on bind failure; chosen socket persisted into the lock; all 4 client sites read `recorded_socket_path` (pid/lock stays at `.codegraph/daemon.pid`). golden-neutral.                  |
| `45d3293` pt1 | name-ceiling                         | **PORTED**          | `AMBIGUOUS_NAME_CEILING=500` (env `CODEGRAPH_AMBIGUOUS_NAME_CEILING`); gates ONLY `match_fuzzy` + the multi-candidate `find_best_match` branch of `match_by_exact_name`. Edge-producing strategies untouched → golden edges byte-identical (verified). |
| `45d3293` pt2 | `[indexing] exclude`                 | **PORTED**          | TOML `exclude: Vec<String>` (`#[serde(default)]`), root-relative patterns threaded via `ExtractOptions` through the filesystem walk; honored by index AND sync. Off-by-default. NO git-tracked framing (Rust has no git layer — D-Exclude).            |
| `7a361ef`     | docs for `exclude`                   | **PORTED**          | README + `docs/cli.md`/`docs/mcp.md` document `exclude`, readOnlyHint, daemon FS fallback.                                                                                                                                                             |
| `b3f59c7`     | Swift computed properties            | **PORTED**          | computed `var body { ... }` / accessors → `property` node, getter routed through the function-body walk; `protocol var x { get }` → `property`; stored/static unchanged. Golden-neutral (no `.swift` in the TS+Py corpus); locked by 5 new unit tests. |
| `dfe13b0`     | —                                    | **N-A**             | MCP read-tool worker pool. The Rust MCP server is synchronous single-stdin (`server.rs`); concurrent-call starvation cannot occur, so the fix has no Rust analogue. Not ported by design.                                                              |
| `703629e`     | —                                    | **DEFER**           | C/C++ function-pointer command tables. Rust has no C/C++ resolver foundation; deferred as a future capability-build (user-approved exclusion from this batch).                                                                                         |

(ALREADY-HAVE / N-A items `d3179f5`, `30dc303`, `9716fb2`, `b45f309` need no
action — see the triage entry below for evidence.)

### 2026-06-28 — triaged `v1.1.1..v1.1.2` (PORT items NOT yet landed)

Read the real per-commit diffs of the 11 meaningful commits in `v1.1.1..v1.1.2`
and classified each against the Rust code (file:line both sides). This is a
**triage** — tracked parity stays `1.1.1` until the PORT items land. Two highest-
stakes items cross-checked by hand (30dc303 SQLite, b45f309 prompt-hook).

| commit    | what it does                                                                                     | Rust status                                                                                                                                                         | disposition      | golden                                       |
| --------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- | -------------------------------------------- |
| `dfe13b0` | MCP off-loads read-tool dispatch to a worker pool (fix concurrent-call timeouts)                 | MCP server is synchronous/serial (`crates/codegraph-mcp/src/server.rs`, `engine.rs:60-81`) — no worker pool                                                         | **PORT**         | none                                         |
| `703629e` | C/C++ function-pointer command tables (macro-built / conditional / bare arrays)                  | No C/C++ resolver at all (extraction only; no `frameworks/c*`)                                                                                                      | **DEFER**        | would change output                          |
| `b45f309` | prompt-hook: structured detection for non-English prompts                                        | No prompt-hook / front-load-hook feature in Rust (grep: only unrelated "structural" hits; installer only writes MCP config)                                         | **N/A**          | none                                         |
| `d3179f5` | require `projectPath` when MCP server has no default project                                     | Rust already stronger: hides all tools when no default project (`server.rs:374-380`) + errors on missing path (`:439-455`)                                          | **ALREADY-HAVE** | none                                         |
| `45d3293` | `codegraph.json exclude` config + resolver ambiguous-name ceiling (anti-wedge) + index watchdogs | Rust config is `.codegraph/config.toml` with only `ignore_dirs` — no `exclude`; resolver has no name-ceiling (`config.rs:48-58`, `resolver.rs:882-992`)             | **PORT**         | exclude none; name-ceiling may affect golden |
| `7a361ef` | docs for `exclude`                                                                               | depends on 45d3293 landing first                                                                                                                                    | **DEFER**        | none                                         |
| `f83a1ec` | daemon on ExFAT/FAT/network FS — socket/lock candidate fallback after bind failure               | Rust only does tmp fallback for over-long Unix paths, binds a single socket, no bind-failure retry (`daemon/src/paths.rs:24-31`, `lib.rs:259-270`, `lock.rs:66-71`) | **PORT**         | none                                         |
| `30dc303` | chunk `deleteResolvedReferences` IN-list under SQLite param limit (public QueryBuilder API)      | Rust delete path binds **per-row** (`queries.rs:677-693`), structurally immune; chunking (`SQLITE_PARAM_CHUNK_SIZE=500`) already systemic                           | **ALREADY-HAVE** | none                                         |
| `b3f59c7` | index Swift computed properties (computed→property, getter calls attributed)                     | Rust does not node-ify Swift computed properties (`extract/src/walker.rs:149-160`, `lang/swift.rs`)                                                                 | **PORT**         | changes extraction → golden bump             |
| `9716fb2` | parallelize indexing via parse worker pool                                                       | Rust already parallel-indexes via rayon file-level parallelism (`extract/src/engine.rs:21-36, 213-217`) — different design, same capability                         | **ALREADY-HAVE** | none                                         |
| `a79fa51` | MCP `readOnlyHint`/annotations so tools work in Cursor Ask mode                                  | Rust `tools/list` emits schema/value only, no `annotations` (`server.rs:374-380`)                                                                                   | **PORT**         | none                                         |

**PORT backlog from this range (pending user go-ahead), low-risk-first:**

1. `a79fa51` MCP readOnlyHint annotations — golden-neutral, small.
2. `f83a1ec` daemon FS fallback chain — golden-neutral, daemon robustness.
3. `dfe13b0` MCP read-tool worker pool — golden-neutral, concurrency.
4. `45d3293` `exclude` config (+ later the resolver name-ceiling, which may touch golden).
5. `b3f59c7` Swift computed properties — **golden bump** (extraction change), isolate.
6. `703629e` C/C++ fn-pointer resolver — large; needs a C/C++ resolver foundation first.

**No-action:** `d3179f5`, `30dc303`, `9716fb2` (ALREADY-HAVE); `b45f309` (N/A); `7a361ef` (DEFER until `exclude` lands).

Newest first. Each entry covers one sync of an upstream version range and records
every meaningful change with its disposition:
**PORT** (landed in Rust) · **ALREADY-HAVE** (port already implemented it) ·
**N/A** (TypeScript-only, no Rust analogue) · **DEFER** (real but postponed).

### 2026-06-27 — downstream-only features (no upstream sync)

NOT an upstream sync. Two **downstream-only** features were added to this Rust
port that have **no analogue in colby** (verified against colby `v1.1.1` HEAD on
2026-06-27: `git grep` over upstream `src/` + CLI command inventory + docs/tests
found no orphan/dangling audit command and no Godot id-field / DSL-schema config).
Both are Godot-research conveniences requested by a downstream Godot/WorldFlipper
user; they do not change extraction output (golden byte-neutral). Tracked colby
version is UNCHANGED at `1.1.1` — these do not advance parity.

| feature (this port only)                                                                                                                                                                                                                                                                                            | disposition     | rationale / golden impact                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Resource audit** — read-only `codegraph audit` subcommand: orphan `.tres`/`.tscn` resources, dangling path references, reverse-dependency impact (`crates/codegraph-graph/src/graph/mod.rs` `find_orphan_resources`/`find_dangling_references`/`resource_impact`; `crates/codegraph-cli/src/main.rs` `cmd_audit`) | DOWNSTREAM-ONLY | No upstream analogue. Pure read-only over the existing graph + on-disk existence checks — adds NO extraction, NO node/edge writes. Golden byte-neutral (`git diff reference/` empty). Keyed on resource PATHS (godot resource files have no `file:` node; refs live in `unresolved_refs`). Separate `audit` subcommand leaves `check` byte-identical (committed snapshot test). Exclusions: `.godot/`/`addons/` prefix → `godot:dynamic:` → on-disk existence.                                                                  |
| **Opt-in `idFields` DSL** — capture bare/compound IDs in `.tres` `[resource]` bodies as `godot:id:<kind>:<value>` sentinel references via `.codegraph/codegraph.json` `godot.dsl.idFields` (`crates/codegraph-resolve/src/frameworks/godot_dsl_config.rs`, `godot_resource.rs` `dsl_id_targets`)                    | DOWNSTREAM-ONLY | No upstream analogue (upstream has no godot id-field/schema config). Golden byte-neutral by two-part mechanism: (1) opt-in gating — fires ONLY when `idFields` is configured, and committed golden corpora have no such config; (2) sentinel-unresolvability — a colon-delimited `godot:id:*` literal can never name-match a node (`match_fuzzy` callable/type kinds only), so it stays in `unresolved_refs`, never becomes a golden `edges` row. No domain hardcoding — all field names/kinds/separators are project-supplied. |
| Bundled fix: Godot DSL config resolved against PROJECT ROOT, not process CWD (`FrameworkResolver::extract` gains an additive `project_root` param; `config_lookup_path`)                                                                                                                                            | DOWNSTREAM-ONLY | Fixes a latent defect (shared by the pre-existing `resourceFields`): the framework resolver received a relative path and joined it onto CWD, so the per-project config was only found when CWD == project root. Now config lookup uses `project_root.join(file_path)` while node/ref attribution stays RELATIVE (golden-safe). Locked by an e2e CLI test that indexes from a foreign CWD.                                                                                                                                       |

**Outcome:** two downstream-only Godot conveniences landed; colby parity unchanged
at `1.1.1`. No golden impact, no upstream divergence (these are additive features
upstream does not have, not deviations from upstream behavior). Should a future
colby release add a comparable audit or id-schema feature, reconcile naming/shape
against these at that sync.

### 2026-06-26 — synced `v1.1.0` -> `v1.1.1`

A small upstream patch release: two `fix(...)` commits plus release/changelog
chores. `git diff v1.1.0..v1.1.1` against the live upstream source. **No PORT
items** — both fixes are either a TypeScript/Node-architecture-specific problem
with no Rust analogue, or behavior this port already has. The proxy half of #983
was verified line-by-line against `codegraph-daemon::proxy::run_proxy` and its
CLI callers before recording it as ALREADY-HAVE. Golden output untouched (no
extraction change in range). Tracked version advanced to `1.1.1` (upstream HEAD).

| upstream change                                                                                                                                                                                                                                                                                                    | disposition                   | rust landing / rationale                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| #983 (`7c6417e`) daemon half — release the lockfile when `bind` fails (e.g. AF_UNIX flaky on WSL2 `/mnt` DrvFs) so the next launcher doesn't spin on a stale lock (`src/mcp/daemon.ts:183-198`)                                                                                                                    | ALREADY-HAVE                  | `start_with_lock` calls `bind(&rendezvous)?` with `?`: a bind failure propagates up to the `try_acquire_daemon_lock` caller and the RAII lock guard releases the pid lock automatically — no stale lock pointing at a dying pid, no duplicate `serve --mcp` pileup. Structurally cannot leave a hung lock.                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| #983 (`7c6417e`) proxy half — a daemon-socket `error` with no listener became a Node `uncaughtException` → `process.exit(1)`, which the MCP client saw as a bare "Transport closed"; fix attaches a lifelong no-op `error` listener so it degrades to the in-process fallback (`src/mcp/proxy.ts:135-145,323-336`) | ALREADY-HAVE (verified)       | Node-specific defect with no Rust analogue. In `run_proxy`, every socket error is an explicit `io::Result::Err`, never an unhandled exception: connect failure → `Err` → CLI `Err` arm falls back to direct (`main.rs:1350`); hello-read failure → `?` → same fallback; host→daemon write failure → `pump_host_to_daemon` `Err` → `up_result?` → fallback; daemon→host read failure → `Err(_) => break` (treated as EOF, clean exit). No path lets an unhandled socket error panic/exit the process, so the "Transport closed" symptom cannot occur. (Minor design difference: on a mid-session daemon drop the Rust proxy exits the stdio session for a clean client reconnect rather than hot-swapping to an in-process engine — a normal transport close, not a crash.) |
| #980 (`73bcc1a`) extraction — respect `.gitignore` by DEFAULT for embedded-repo discovery: gitignored dirs holding nested `.git` repos are no longer walked/indexed unless opted in via `codegraph.json` `includeIgnored` (#970, #976; `src/extraction/index.ts`, `src/project-config.ts`)                         | N/A (architecture difference) | colby's `git ls-files`-based discovery actively descended INTO gitignored dirs to find embedded `.git` roots (`findIgnoredEmbeddedRepos`), which over-indexed huge reference/data dirs of clones — the bug #980 fixes. This port's walker is a pure filesystem walk (`scan_project`/`scan_dir`, `engine.rs:221-256`): a gitignored dir is matched by `is_ignored_by_patterns` and the whole subtree is `continue`d/skipped. There is no embedded-repo discovery and never was, so the Rust port has ALWAYS exhibited the #980-fixed default (respect `.gitignore`). Nothing to port.                                                                                                                                                                                       |
| #980 new `codegraph.json` `includeIgnored` opt-in config field                                                                                                                                                                                                                                                     | DEFER (no analogue yet)       | This is a new _feature_ (not the fix): opt-in to index gitignored embedded repos. It only has meaning alongside an embedded-repo / multi-repo-workspace discovery mechanism, which this port does not implement. Recorded as a known gap should a future "multi-repo workspace" direction be pursued; not actionable in this range.                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `ec0c625` / `4077ed1` / `1e48861` version bump + CHANGELOG edits; `__tests__/*` (daemon-bind-failure, proxy-connect, include-ignored-config, multi-repo-workspace)                                                                                                                                                 | N/A                           | release chores and TypeScript test files — no behavior to port.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |

**Outcome:** tracked version is `1.1.1` (upstream HEAD). No PORT items in this
range; no golden impact (extraction output unchanged). The only deferred item is
the `includeIgnored` config field, which is inert without an embedded-repo
discovery mechanism this port intentionally lacks.

**Next sync:** when upstream publishes a tag > `1.1.1`, run Workflow A from this
point (`git diff v1.1.1..v<new>`). Outstanding port backlog is unchanged: the
B-class extraction/framework golden-bump plan from the v1.1.0 range.

### 2026-06-24 — synced `v1.0.1` -> `v1.1.0`

First true catch-up delta since the baseline: `git diff v1.0.1..v1.1.0` against the
live upstream source. The headline of v1.1.0 is the issue-#411 **live-watch daemon**
architecture (a detached background daemon + thin per-client proxy sharing one file
watcher), plus a batch of extraction/framework upgrades. This sync lands the entire
**A-class** (daemon/proxy/watcher/robustness/UX — no extraction-output change, golden
byte-untouched) on branch `feat/live-watch-daemon`. The **B-class** extraction ports
(value-reference edges, framework dispatch synthesizers, etc.) are each recorded as
**DEFER** below: every one of them changes extraction _output_ and therefore requires
a deliberate golden regeneration, so they are scoped to a **separate future plan**
rather than smuggled into this no-golden-bump sync.

| upstream change                                                                                                                                                                                            | disposition  | rust landing / rationale                                                                                                                                                                                                                                                                                              |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Detached daemon process + launcher decision order (`CODEGRAPH_NO_DAEMON` opt-out, `CODEGRAPH_DAEMON_INTERNAL` be-the-daemon, no-`.codegraph/`→direct, else spawn+proxy) (`src/mcp/index.ts:260-292`, #411) | PORT         | `select_serve_mode` decision in `cmd_serve` + `spawn_detached_daemon` re-invokes the CLI with `CODEGRAPH_DAEMON_INTERNAL=1`, own process group (unix) / `DETACHED_PROCESS\|CREATE_NEW_PROCESS_GROUP` (windows), stdio→`.codegraph/daemon.log`, child dropped (unref) — daemon survives the launcher (this plan T1/T2) |
| Per-MCPEngine file watcher in `serve --mcp`, direct + daemon (`src/mcp/engine.ts:178-240`)                                                                                                                 | PORT         | one `ProjectWatcher` in `cmd_serve` (direct) and one in the daemon process (`run_accept_loop`, shared by N clients — the #411 collapse of N inotify sets to 1); never per-session (this plan T3/T4)                                                                                                                   |
| Non-blocking catch-up sync on open (#905)                                                                                                                                                                  | PORT         | background `sync_project_once` thread + `Arc<AtomicBool>` ready flag; the first `tools/call` never blocks on the reconcile (this plan T5)                                                                                                                                                                             |
| Reopen DB when `.codegraph` is replaced on disk (#925, `src/db/index.ts`)                                                                                                                                  | PORT         | `engine_for` records the db file identity (unix inode / windows `file_index()`) at open and reopens the cached engine only on a true replacement — never on an in-place WAL write (this plan T6)                                                                                                                      |
| Detached daemon needs a thin client proxy (local-handshake stdio↔socket pipe, hello-version verify, direct fallback, PPID watchdog, refcount-decrement-on-close) (`src/mcp/proxy.ts:194-289`, #277/#411)   | PORT         | `codegraph-daemon::proxy::run_proxy` answers `initialize`/`tools/list` locally from this build's static constants, suppresses the daemon's forwarded `initialize` reply, forwards the rest line-framed, verifies hello → `VersionMismatch` falls back to direct (this plan T7)                                        |
| Daemon idle-linger + max-idle backstop (`DEFAULT_IDLE_TIMEOUT_MS=300000`, `MAX_IDLE_MS=1800000`, #692) (`src/mcp/daemon.ts:60-72,505-521`)                                                                 | PORT         | `run_accept_loop` tracks `last_active`, exits when idle past `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` with zero clients, and on the `CODEGRAPH_DAEMON_MAX_IDLE_MS` backstop regardless of count (this plan T8)                                                                                                              |
| Dead-client sweep via a client-hello pid protocol (`DaemonClientHello`, `CLIENT_HELLO_TIMEOUT_MS`, `DEFAULT_CLIENT_SWEEP_MS=30000`, #692) (`src/mcp/daemon.ts:103-127,569-624`)                            | PORT         | optional `{"hostPid":N}` client-hello read from ONE long-lived `BufReader` (buffer-safe `run_session_recv` seam — no dropped JSON-RPC bytes), per-session pid in the registry, 30s sweep reaps peers failing `is_process_alive` (this plan T9)                                                                        |
| Watcher degraded handling — EMFILE/ENFILE degrade, ENOSPC warn-not-degrade, bounded exp lock-retry backoff (cap 30s), `onDegraded`/`isDegraded` (`src/sync/watcher.ts` +254, #891/#892/#893)               | PORT         | `WatchOptions` gains `on_degraded`/`on_sync_error`; the notify `Err` arm is plumbed as `LoopMessage::WatchError` and classified via `raw_os_error()` (EMFILE 24 / ENFILE 23 → degrade; ENOSPC 28 → warn); surfaced to the agent via **STDERR only** so `ToolResult` bytes stay golden-identical (this plan T10)       |
| Custom file-extension→language mapping via project config (`{"extensions":{".x":"lua"}}`, mtime-cached, #906) (`src/project-config.ts:1-155`)                                                              | PORT         | net-new `.codegraph/codegraph.json` reader (mtime-cached) merged over `builtin_language_for_ext`; a hard skip-list gate makes an override unable to change any extension a built-in/embedded mapping already resolves — proven golden-safe by a hostile-remap test (`.ts`→`lua` ignored) (this plan T11)              |
| Front-load prompt-hook + Claude `UserPromptSubmit` installer hook (#964/#966, monorepo-aware) (`src/installer/*`, `bin/codegraph.ts prompt-hook`)                                                          | PORT         | hidden `codegraph prompt-hook` subcommand emits `codegraph_explore`-style retrieval to stdout (NO LLM — pure deterministic retrieval) + an opt-in Claude hook marker block in the installer (this plan T12)                                                                                                           |
| Pare back the default MCP tool surface                                                                                                                                                                     | ALREADY-HAVE | this port already exposes the intended tool set (the 8 colby tools + additive `check`/`export`); no further trimming needed                                                                                                                                                                                           |
| Bold labels instead of ATX headings in tool output                                                                                                                                                         | ALREADY-HAVE | the Rust tool formatters already emit bold-label framing; tool-output bytes already match the v1.1.0 shape                                                                                                                                                                                                            |
| Re-resolve the project path on every call (#926)                                                                                                                                                           | ALREADY-HAVE | `McpServer` already resolves the path per request rather than caching a stale root                                                                                                                                                                                                                                    |
| Cross-file caller edges preserved across re-index (#899)                                                                                                                                                   | ALREADY-HAVE | `resolver.rs` already retains cross-file caller edges through an incremental re-index                                                                                                                                                                                                                                 |
| Stop auto-indexing on install                                                                                                                                                                              | ALREADY-HAVE | this port's `install` only writes agent config; it never triggers an index                                                                                                                                                                                                                                            |
| React `forwardRef`/`memo` component detection                                                                                                                                                              | ALREADY-HAVE | `react.rs` already recognizes `forwardRef`/`memo` wrappers                                                                                                                                                                                                                                                            |
| In-root symlink indexing (#935)                                                                                                                                                                            | ALREADY-HAVE | the walker already follows in-root symlinks during indexing                                                                                                                                                                                                                                                           |
| Reasoning offload / CodeGraph AI managed tier / login / logout / usage / offload telemetry                                                                                                                 | N/A          | barred by the no-AI/LLM guardrail (`scripts/guardrail.sh`); also **removed upstream pre-1.1.0** via `e5897d0`, so there is nothing in the v1.0.1→v1.1.0 range to port                                                                                                                                                 |
| `git ls-files` `./` self-entry handling (#936)                                                                                                                                                             | N/A          | architecture difference — the Rust walker does not enumerate via `git ls-files`, so the self-entry bug does not exist here                                                                                                                                                                                            |
| Index full-rebuild semantics (#874)                                                                                                                                                                        | N/A          | architecture difference — the Rust indexer already rebuilds fully on `index --force`; no behavioral gap to close                                                                                                                                                                                                      |
| Submodule de-duplication (#945)                                                                                                                                                                            | N/A          | architecture difference — the Rust walker's submodule handling differs from the TS path; not the same bug                                                                                                                                                                                                             |
| Same-file value-reference edges across 15 languages (#895/#897)                                                                                                                                            | DEFER        | changes extraction OUTPUT (new edges) → golden regen required; scoped to the separate B-class plan                                                                                                                                                                                                                    |
| Framework dispatch synthesizers — GoFrame `g.Meta` (#747), Laravel event→listener, Sidekiq, MediatR, Spring `publishEvent`, Celery, Vuex, Pinia, RTK Query, redux-thunk, object-literal registry           | DEFER        | each synthesizes new edges → extraction OUTPUT change → golden regen required; largest B-class surface (~1000+ LOC), separate plan                                                                                                                                                                                    |
| C/C++ function-pointer dispatch (#932)                                                                                                                                                                     | DEFER        | new dispatch edges → extraction OUTPUT change → golden regen required; separate B-class plan                                                                                                                                                                                                                          |
| Java Lombok synthesized members (#912)                                                                                                                                                                     | DEFER        | new synthesized member nodes → extraction OUTPUT change → golden regen required; separate B-class plan                                                                                                                                                                                                                |
| React `styled()` / JSX-file route detection (#841)                                                                                                                                                         | DEFER        | new nodes/edges → extraction OUTPUT change → golden regen required; separate B-class plan                                                                                                                                                                                                                             |
| C++ phantom-macro-function fix (#946)                                                                                                                                                                      | DEFER        | changes which function nodes are emitted → extraction OUTPUT change → golden regen required; separate B-class plan                                                                                                                                                                                                    |
| Perf: O(K²) import-node resolution (#915)                                                                                                                                                                  | DEFER        | verify-if-applicable to the Rust resolver; if it reproduces, the fix may shift resolution output → treat as golden-sensitive in the separate B-class plan                                                                                                                                                             |

**Outcome:** tracked version is `1.1.0` (upstream HEAD). The full A-class live-watch
daemon stack landed (13 todos, T1–T12 implementation + this ledger) with golden
output **byte-untouched** — `cargo test -p codegraph-bench` (extraction golden) and
`cargo test -p codegraph-mcp golden_mcp` (MCP-response golden) pass unchanged, and
`git status --porcelain reference/golden/` is empty. No AI/vector/LLM crate crept in
(`scripts/guardrail.sh` green). The **B-class** extraction/framework ports are
deferred to a **separate future plan** specifically because each one changes
extraction OUTPUT and therefore demands a deliberate golden regeneration — they are
recorded above so the next sync does not re-investigate them.

**Next sync:** when upstream publishes a tag > `1.1.0`, run Workflow A from this
point (`git diff v1.1.0..v<new>`). The pending B-class golden-bump plan is the
outstanding port backlog from this range.

### 2026-06-21 — baseline established at `v1.0.1`

First ledger entry. This port was built directly against colby `1.0.1` (the
latest upstream tag), so this is not a catch-up delta but a **baseline parity
audit** confirming faithful coverage. Verified by cross-checking the live
upstream source (`git clone` @ `1.0.1`) against this workspace.

| upstream surface                                                                                 | finding                                                                                                                                                                                  | disposition        |
| ------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ |
| `LANGUAGES` (`src/types.ts`) — 32 languages                                                      | this project's `Language` enum has all 32, byte-identical set (incl. `c`, `r`); zero diff either direction                                                                               | ALREADY-HAVE       |
| MCP tool set — 8 tools (`search`/`callers`/`callees`/`impact`/`node`/`explore`/`status`/`files`) | this project exposes all 8 **plus** two additive own tools (`check`, `export`) — a superset                                                                                              | ALREADY-HAVE       |
| SQLite `.schema`                                                                                 | golden-anchored to colby 1.0.1 (`reference/golden/colby.schema.sql`); `schema_parity` test green                                                                                         | ALREADY-HAVE       |
| Golden extraction equivalence                                                                    | `crates/codegraph-bench/tests/equivalence.rs` — **4 passed, 0 failed** against the 1.0.1 mini golden                                                                                     | ALREADY-HAVE       |
| Per-language edge sets                                                                           | 15 of 17 benchmarked languages byte-identical to colby 1.0.1; the 2 exceptions (Dart, Pascal `function_ref` shape) are deliberate, documented in `KNOWN_DIFFS.md` (Tier-3 allowed diffs) | DEFER (documented) |

**Outcome:** tracked version is `1.0.1`. No PORT items outstanding (upstream has
no release newer than 1.0.1). Two known per-language edge-set differences (Dart,
Pascal) remain intentionally deferred and are recorded in `KNOWN_DIFFS.md` — they
are golden-oracle-allowed Tier-3 diffs, not regressions. Golden + guardrail +
`cargo test --workspace` green.

**Next sync:** when upstream publishes a tag > `1.0.1`, run Workflow A from this
baseline (`git diff v1.0.1..v<new>`).

### 2026-06-22 — Windows daemon parity (C6, `feat/windows-daemon`)

Internal port commit, no upstream version bump. Closes the Unix-only IPC gap that
was the one outstanding platform limitation since the `1.0.1` baseline.

| upstream surface                                                                                            | finding                                                                                                                                                                                                                             | disposition |
| ----------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| `src/mcp/daemon-paths.ts` — named-pipe branch (`\\.\pipe\codegraph-<hash>`) on Windows, Unix socket on unix | Rust port now matches: `codegraph-daemon` uses `interprocess` 2.4 `local_socket` — UDS on unix, named pipe on Windows; pipe name stored bare (`codegraph-<sha256hash16>`), `GenericNamespaced` prepends `\\.\pipe\` at connect time | PORT        |
| Windows process-liveness check (no `kill -0` on Windows)                                                    | `process.rs` cfg-split: unix uses `rustix` getppid + signal-0; Windows uses `windows-sys` `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `GetExitCodeProcess == STILL_ACTIVE`                                                   | PORT        |
| Stale-socket cleanup on unix (`.sock` unlink)                                                               | cfg-gated to unix only; Windows pipe names are OS-reclaimed, no file to remove                                                                                                                                                      | PORT        |
| MSRV declaration                                                                                            | bumped `rust-version` 1.70 → 1.75 in workspace `Cargo.toml` and both READMEs (`interprocess` 2.x requires rustc 1.75; actual toolchain was already `stable`/1.96)                                                                   | PORT        |
| Prebuilt release matrix                                                                                     | `x86_64-pc-windows-msvc` added to `release-please.yml`; packaged as `.zip` via PowerShell `Compress-Archive`; upload-assets glob extended to `dist/*.tar.gz` + `dist/*.zip`                                                         | PORT        |
| CI gate                                                                                                     | `windows-latest` job added to `ci.yml`; wired into `ci-success` as merge blocker; linux job gains a `cargo check --target x86_64-pc-windows-msvc` cross-check step                                                                  | PORT        |

**Outcome:** Windows IPC gap is fully closed. The daemon start/attach/stop cycle,
named-pipe accept, lock contention, and ppid-watchdog behavior are all covered by
the `windows-latest` CI job. No upstream version change — this is Rust-side
infrastructure work matching the colby `1.0.1` cross-platform design.
Tracked colby version was `1.0.1` at the time of this entry (subsequently bumped
to `1.1.0` in the 2026-06-24 sync above).
