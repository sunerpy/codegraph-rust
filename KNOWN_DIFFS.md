# Known CodeGraph Equivalence Differences

This file is parsed by `codegraph-bench::oracle::diff::KnownDiffs` for Tier-3
differences only. Tier-1 and Tier-2 differences are never allowlisted by this
file.

## Rule format

One grep-able rule per line:

```text
RULE tier=3 surface=<surface> key=<substring-or-*> justification=<short-token>
```

- `tier`: currently only `3` can allow a diff.
- `surface`: presentation or behavioral surface name reported by the differ.
- `key`: substring matched against the diff key, or `*` for the whole surface.
- `justification`: short no-spaces reason; expand with prose nearby when needed.

## Current rules

No Tier-3 rules are active yet.

## Canonicalized by design, not allowlisted

Timestamps are stripped before comparison and are therefore not represented as
Tier-3 rules:

- `nodes.updated_at`
- `files.modified_at`
- `files.indexed_at`

## Deferred colby resolution behavior (Task 20, codegraph-resolve)

The v1 Rust port of `reference/colby/src/resolution/` ships the two CORE
deterministic strategies — import resolution (`import-resolver.ts`) and name
matching (`name-matcher.ts`) — orchestrated by `ReferenceResolver`
(`index.ts`), plus THREE concrete `FrameworkResolver` implementations behind the
`FrameworkResolver` trait EXTENSION POINT (`crates/codegraph-resolve/src/framework.rs`):
React/Next.js, Vue/Nuxt, and NestJS (`crates/codegraph-resolve/src/frameworks/`),
detected per-project by `detect_frameworks`. The remaining ~19 colby resolvers
stay deferred. The following colby behaviors are intentionally NOT ported in v1:

- **The other framework-specific resolvers** — every concrete `FrameworkResolver`
  in `reference/colby/src/resolution/frameworks/**` EXCEPT react/vue/nestjs:
  Spring/Java, Drupal `routing.yml`, Express, Svelte, React Native / Expo / Fabric
  bridges, Swift↔ObjC bridging, the Go-module / Python / Ruby / C# / Play route
  extractors, and the Astro resolver
  (`frameworks/astro.ts`: `astro:*` module imports + the `Astro` global resolved
  as framework-provided, and `src/pages` file-based route-node mapping). These
  are an additive heuristic layer; on a project where none of react/vue/nestjs is
  detected the orchestrator holds an empty resolver list, so behavior matches
  colby with zero frameworks detected. The Astro
  EXTRACTION half (frontmatter + `<script>` delegation, template `{call(...)}`
  and `<Component>` refs) IS ported (`crates/codegraph-extract/src/embedded/astro.rs`,
  #768); only its framework-resolution half is deferred. Re-enable any of the
  deferred resolvers by adding it to `frameworks/` and the `detect_frameworks`
  registry — no orchestrator change required.
- **Callback / observer edge synthesis** — `synthesizeCallbackEdges`
  (`reference/colby/src/resolution/callback-synthesizer.ts`, invoked at
  `index.ts:959-963`). Heuristic dynamic-dispatch edge synthesis; deferred behind
  the same extension point.
- **C++ receiver-type inference for method calls** — `inferCppReceiverType` /
  `matchCppCallChain` / `resolveCppCallResultType` (`name-matcher.ts:333-570`).
  Reads source text + return types to type bare `recv.method()` receivers.
- **Java/Kotlin field-receiver inference** — `inferJavaFieldReceiverType`
  (`name-matcher.ts:705-752`), the Spring `@Autowired` field-injection receiver
  path.
- **Per-language import refinements in `resolveViaImport`** — Go cross-package
  (`resolveGoCrossPackageReference`), Python module-member / absolute-module
  (`resolvePythonModuleMember` / `resolvePythonAbsoluteModule` /
  `findPythonModuleFile`), Rust qualified-path (`resolveRustPathReference` and
  the `crate::`/`self::`/`super::` module-file walk), and Lua/Luau `require`
  (`resolveLuaRequire`) — `import-resolver.ts:1202-1253,1311-1658`. The generic
  import-mapping + re-export-chase core ships; these language-specific
  refinements are follow-ups.
- **Razor `@using` cascade resolution** — `resolveRazorUsing` / `getRazorUsings`
  (`index.ts:1116-1158`), tied to the Razor framework layer.
- **Batched/conformance passes** — `resolveAndPersistBatched` +
  `resolveChainedCallsViaConformance` (`index.ts:836-970`). The v1 port resolves
  the full `unresolved_refs` set in one pass (`resolveAndPersist`); the bounded
  batched loop and the deferred-chain conformance second pass are deferred.

None of the above are Tier-3 allowlist rules: they do not introduce a different
value for a surface colby produces on the validated mini golden — they are
additional resolution paths colby would exercise only on inputs outside the v1
core scope. They are recorded here per Task 20's requirement to document deferred
colby resolution behavior.

## #2c — callback function_ref (colby 8a114ba5 / #756, multi-language)

The Rust port captures colby's "function-as-value / callback registration"
feature (`reference/colby/src/extraction/function-ref.ts`) and resolves it as a
`references` edge tagged `fnRef: true` / `resolvedBy: "function-ref"`. The model
is internal-only: extraction emits an `UnresolvedRef` with `is_function_ref`,
persisted via the literal `unresolved_refs.reference_kind` string `function_ref`
(no `.schema` change — the column has no CHECK constraint), read back as
`references` + `is_function_ref`, and resolved to a `references` edge.

15 of the 17 supported languages produce edge sets BYTE-IDENTICAL to colby 1.0.1
for the function_ref feature (TS/JS/TSX/JSX, Python, Go, Rust, C, C++, Java,
Kotlin, Ruby, PHP, C#, Swift, Scala, Lua/Luau). The following two carry minor,
pre-existing grammar-modeling divergences ORTHOGONAL to function_ref:

- **Dart** — colby's Dart extractor sets `functionTypes: ['function_signature']`,
  so its generic walker descends into a `function_declaration`'s body at FILE
  scope and emits an extra duplicate `file -> fn` fnRef edge alongside the
  enclosing-function edge. The Rust port models a Dart function as the whole
  `function_declaration` (consuming its body once), so it emits only the
  enclosing-function fnRef edge. This is a function-node-modeling difference that
  predates function_ref; forcing the duplicate would require remodeling Dart
  function extraction (regression risk on existing Dart goldens).
- **Pascal** — in `tree-sitter-pascal` a `defProc` nests the `declProc` header and
  the `block` body as SIBLINGS, and `PascalSpec` has no `resolve_body` override
  that reaches the sibling block, so procedure bodies are not walked under the
  procedure's node-stack scope. A `@Handler` callback inside a procedure body is
  therefore attributed to the FILE node rather than the enclosing procedure
  (colby attributes it to the procedure). This is the same pre-existing
  body-resolution limitation that already leaves regular Pascal in-body `calls`
  edges unextracted in BOTH engines — orthogonal to function_ref.

Neither is a Tier-3 allowlist rule on the validated mini golden (mini is
TS/Python with no callbacks); they are recorded here as scoped follow-ups.

## Task 22 — MCP tool output (Tier-2 text-formatting differences)

The MCP server (`crates/codegraph-mcp`) renders 8 tools byte-identical to colby
on the mini golden EXCEPT the following text-formatting / relevance-ordering
differences. Six of the eight tools (`codegraph_search`, `codegraph_callers`,
`codegraph_callees`, `codegraph_impact`, `codegraph_node` symbol+file modes,
`codegraph_files` tree/flat/grouped) are byte-identical; the diffs below are
confined to `codegraph_explore` and `codegraph_status`.

- **`codegraph_explore` relevance ranking (RWR/flow/skeletonization) is not
  ported; the size-adaptive output BUDGET is.** colby's `findRelevantContext`
  (`reference/colby/src/context/index.ts`) ranks the subgraph with
  Random-Walk-with-Restart / personalized PageRank, computes a FLOW spine among
  named symbols (`buildFlowFromNamedSymbols`), and adaptively SKELETONIZES
  off-spine polymorphic-sibling files to signatures (`adaptiveExplore`,
  `tools.ts:2422-2623`). The Rust port reproduces the DETERMINISTIC structure —
  header, blast radius, relationship map, dynamic boundaries, and per-file source
  grouped by file — by seeding roots from the FTS search results
  (`searchLimit: 8`) and pulling their callers/callees + `contains` children into
  the subgraph. It DOES now apply colby's `getExploreOutputBudget`
  (`tools.ts:160-258`, ported in `explore_budget.rs`): the size-tiered total /
  per-file caps, `defaultMaxFiles`, gap-threshold clustering, header symbol cap,
  relationship edge cap, `excludeLowValueFiles`, the whole-method-drop +
  "Not shown above" excluded-file list, and the gated completeness-signal /
  budget-note / relationships sections. Consequences on the mini golden:
  - The `Found N symbols across M files` count differs (colby prunes the import
    seed via RWR; the port keeps it → 7 vs 6). The blast-radius entries, section
    headers, and every verbatim numbered source line are byte-identical.
  - Within a `#### <file> — <symbols>` source header, the symbol list ordering
    can differ (the port sorts by start line; colby uses subgraph insertion
    order). Same symbols, same set.
  - The mini corpus lands in the <150-file tier (13000-char cap, files small
    enough to render whole, `includeRelationships/AdditionalFiles/Completeness/
    BudgetNote` all off), so the budget produces output byte-identical to the
    pre-budget whole-file render and colby itself emits none of the gated
    sections.
  - **Off-spine skeletonization is DEFERRED (follow-up).** The budget tiers,
    whole-method-drop, clustering, and excluded-list (the core size-adaptive
    value) ARE ported; colby's `adaptiveExplore` polymorphic-sibling
    skeletonization (collapse off-spine sibling/god-file methods to signatures)
    depends on the un-ported FLOW spine + supertype-implementer analysis and is
    not yet reproduced. Files over the per-file cap fall to importance-ranked
    cluster windowing (whole methods, never sliced) instead of signature
    skeletons — a strictly larger-but-bounded response on sibling-heavy flows,
    never a correctness change.
- **`codegraph_status` Database size MB can differ by rounding.** colby reports
  `fs.statSync(dbPath).size` (`reference/colby/src/db/index.ts:177-180`); the
  port reports the same on-disk file size. The byte count drifts only because
  colby checkpointed its WAL before the capture (0.14 vs 0.15 MB). Every other
  status line — counts, backend, journal mode, nodes-by-kind, languages — is
  byte-identical.
- **`codegraph_status` daemon-only sections are omitted.** The `Pending sync:`
  watcher section (`tools.ts:2936-2944`) and the git-worktree-mismatch warning
  (`tools.ts:2889-2891`) require the live file watcher / git probe; a static
  index has neither, so the port omits them (colby also omits them when there is
  nothing pending — which is the indexed-and-quiescent steady state).

These are Tier-2 text-formatting differences (identical input schemas; equal
output structure), not Tier-3 allowlist rules: they do not change a value the
oracle compares on the SQLite golden surfaces.
